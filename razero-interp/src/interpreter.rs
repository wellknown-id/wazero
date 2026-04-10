#![doc = "Interpreter runtime, eval loop, and host-call dispatch."]

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Debug, Display, Formatter};
use std::ops::{Deref, DerefMut};
use std::panic::{self, AssertUnwindSafe};
use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc, RwLock,
};

use crate::operations::{
    AtomicArithmeticOp, FloatKind, InclusiveRange, Instruction, Label, OperationKind, Shape,
    SignedInt, SignedType, UnsignedInt, UnsignedType, V128CmpType, V128LoadType,
};
use crate::signature::Signature;
use razero_wasm::module::RefType;
use razero_wasm::module_instance::ModuleCloseState;

pub const DEFAULT_CALL_STACK_CEILING: usize = 2_000;
pub const WASM_PAGE_SIZE: usize = 65_536;
pub const WASM_MEMORY_LIMIT_PAGES: u32 = 65_536;

pub type RuntimeResult<T> = Result<T, Trap>;
pub type HostFuncRef = Arc<dyn HostFunction>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct YieldSuspend;

pub fn is_yield_suspend_payload(payload: &(dyn Any + Send)) -> bool {
    if payload.is::<YieldSuspend>() {
        return true;
    }
    payload
        .downcast_ref::<Box<dyn Any + Send>>()
        .is_some_and(|inner| is_yield_suspend_payload(inner.as_ref()))
}

thread_local! {
    static ACTIVE_HOST_CALL_STACKS: RefCell<Vec<Vec<ActiveCallFrame>>> = const { RefCell::new(Vec::new()) };
    static ACTIVE_FUEL_REMAINING: RefCell<Vec<Arc<AtomicI64>>> = const { RefCell::new(Vec::new()) };
}

struct ActiveFuelRemainingGuard;

impl Drop for ActiveFuelRemainingGuard {
    fn drop(&mut self) {
        ACTIVE_FUEL_REMAINING.with(|active| {
            active.borrow_mut().pop();
        });
    }
}

pub fn with_active_fuel_remaining<T>(
    fuel_remaining: Option<Arc<AtomicI64>>,
    f: impl FnOnce() -> T,
) -> T {
    let Some(fuel_remaining) = fuel_remaining else {
        return f();
    };
    ACTIVE_FUEL_REMAINING.with(|active| active.borrow_mut().push(fuel_remaining));
    let _guard = ActiveFuelRemainingGuard;
    f()
}

fn active_fuel_remaining() -> Option<Arc<AtomicI64>> {
    ACTIVE_FUEL_REMAINING.with(|active| active.borrow().last().cloned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trap {
    message: String,
}

impl Trap {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for Trap {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for Trap {}

pub trait HostFunction: Send + Sync + 'static {
    fn call(&self, module: &mut Module, stack: &mut [u64]) -> RuntimeResult<()>;
}

struct ClosureHostFunction<F>(F);

impl<F> HostFunction for ClosureHostFunction<F>
where
    F: Fn(&mut Module, &mut [u64]) -> RuntimeResult<()> + Send + Sync + 'static,
{
    fn call(&self, module: &mut Module, stack: &mut [u64]) -> RuntimeResult<()> {
        (self.0)(module, stack)
    }
}

pub fn host_function<F>(func: F) -> HostFuncRef
where
    F: Fn(&mut Module, &mut [u64]) -> RuntimeResult<()> + Send + Sync + 'static,
{
    Arc::new(ClosureHostFunction(func))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalValue {
    pub lo: u64,
    pub hi: u64,
    pub is_vector: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    elements: Arc<RwLock<Vec<Option<u64>>>>,
    max: Option<u32>,
    ty: RefType,
}

impl PartialEq for Table {
    fn eq(&self, other: &Self) -> bool {
        self.max == other.max && self.ty == other.ty && self.elements() == other.elements()
    }
}

impl Eq for Table {}

impl Table {
    pub fn from_elements(elements: Vec<Option<u64>>) -> Self {
        Self::from_elements_typed(elements, None, RefType::FUNCREF)
    }

    pub fn from_elements_typed(elements: Vec<Option<u64>>, max: Option<u32>, ty: RefType) -> Self {
        Self {
            elements: Arc::new(RwLock::new(elements)),
            max,
            ty,
        }
    }

    pub fn from_shared_elements(
        elements: Arc<RwLock<Vec<Option<u64>>>>,
        max: Option<u32>,
        ty: RefType,
    ) -> Self {
        Self { elements, max, ty }
    }

    pub fn elements(&self) -> Vec<Option<u64>> {
        self.elements.read().expect("table read lock").clone()
    }

    pub fn len(&self) -> usize {
        self.elements.read().expect("table read lock").len()
    }

    pub fn get(&self, index: usize) -> Option<Option<u64>> {
        self.elements
            .read()
            .expect("table read lock")
            .get(index)
            .copied()
    }

    pub fn write_range(&self, offset: usize, values: &[Option<u64>]) {
        self.elements.write().expect("table write lock")[offset..offset + values.len()]
            .clone_from_slice(values);
    }

    pub fn copy_within(&self, src: usize, dst: usize, len: usize) {
        self.elements
            .write()
            .expect("table write lock")
            .copy_within(src..src + len, dst);
    }

    pub fn fill(&self, offset: usize, len: usize, value: Option<u64>) {
        self.elements.write().expect("table write lock")[offset..offset + len].fill(value);
    }

    pub fn stack_value(&self, reference: Option<u64>) -> u64 {
        match self.ty {
            RefType::FUNCREF => reference.map(|value| value + 1).unwrap_or(0),
            RefType::EXTERNREF => reference.unwrap_or(0),
            _ => reference.unwrap_or(0),
        }
    }

    pub fn reference_from_stack(&self, value: u64) -> Option<u64> {
        match self.ty {
            RefType::FUNCREF => value.checked_sub(1),
            RefType::EXTERNREF => (value != 0).then_some(value),
            _ => (value != 0).then_some(value),
        }
    }

    pub fn grow(&self, delta: u32, initial_ref: Option<u64>) -> u32 {
        let mut elements = self.elements.write().expect("table write lock");
        let current_len = elements.len() as u32;
        if delta == 0 {
            return current_len;
        }
        let Some(new_len) = current_len.checked_add(delta) else {
            return u32::MAX;
        };
        if new_len == u32::MAX || self.max.is_some_and(|max| new_len > max) {
            return u32::MAX;
        }
        elements.resize(new_len as usize, None);
        if let Some(initial_ref) = initial_ref {
            elements[current_len as usize..].fill(Some(initial_ref));
        }
        current_len
    }
}

#[derive(Debug)]
pub struct MemoryBytesRead<'a>(std::sync::RwLockReadGuard<'a, Vec<u8>>);

impl Deref for MemoryBytesRead<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.0.as_slice()
    }
}

#[derive(Debug)]
pub struct MemoryBytesWrite<'a>(std::sync::RwLockWriteGuard<'a, Vec<u8>>);

impl Deref for MemoryBytesWrite<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.0.as_slice()
    }
}

impl DerefMut for MemoryBytesWrite<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut_slice()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Memory {
    bytes: Arc<RwLock<Vec<u8>>>,
    pub max_pages: Option<u32>,
    pub shared: bool,
}

impl PartialEq for Memory {
    fn eq(&self, other: &Self) -> bool {
        let left = self.bytes();
        let right = other.bytes();
        left.as_ref() == right.as_ref()
            && self.max_pages == other.max_pages
            && self.shared == other.shared
    }
}

impl Eq for Memory {}

impl Memory {
    pub fn new(initial_pages: u32, max_pages: Option<u32>) -> Self {
        Self {
            bytes: Arc::new(RwLock::new(vec![
                0;
                initial_pages as usize * WASM_PAGE_SIZE
            ])),
            max_pages,
            shared: false,
        }
    }

    pub fn from_bytes(bytes: Vec<u8>, max_pages: Option<u32>, shared: bool) -> Self {
        Self {
            bytes: Arc::new(RwLock::new(bytes)),
            max_pages,
            shared,
        }
    }

    pub fn bytes(&self) -> MemoryBytesRead<'_> {
        MemoryBytesRead(self.bytes.read().expect("memory read lock"))
    }

    pub fn bytes_mut(&self) -> MemoryBytesWrite<'_> {
        MemoryBytesWrite(self.bytes.write().expect("memory write lock"))
    }

    pub fn overwrite(&self, bytes: &[u8]) {
        let mut current = self.bytes.write().expect("memory write lock");
        current.clear();
        current.extend_from_slice(bytes);
    }

    pub fn pages(&self) -> u32 {
        (self.bytes().len() / WASM_PAGE_SIZE) as u32
    }

    pub fn grow(&mut self, additional_pages: u32) -> Option<u32> {
        let previous = self.pages();
        let new_pages = previous.checked_add(additional_pages)?;
        if new_pages > WASM_MEMORY_LIMIT_PAGES {
            return None;
        }
        if self
            .max_pages
            .is_some_and(|max_pages| new_pages > max_pages)
        {
            return None;
        }

        let new_len = new_pages as usize * WASM_PAGE_SIZE;
        self.bytes
            .write()
            .expect("memory write lock")
            .resize(new_len, 0);
        Some(previous)
    }

    fn range(&self, offset: u32, len: usize) -> Option<std::ops::Range<usize>> {
        let start = offset as usize;
        let end = start.checked_add(len)?;
        (end <= self.bytes().len()).then_some(start..end)
    }

    pub fn read_byte(&self, offset: u32) -> Option<u8> {
        self.bytes().get(offset as usize).copied()
    }

    pub fn write_byte(&mut self, offset: u32, value: u8) -> bool {
        match self.bytes_mut().get_mut(offset as usize) {
            Some(byte) => {
                *byte = value;
                true
            }
            None => false,
        }
    }

    pub fn read_u16_le(&self, offset: u32) -> Option<u16> {
        let range = self.range(offset, 2)?;
        Some(u16::from_le_bytes(self.bytes()[range].try_into().ok()?))
    }

    pub fn write_u16_le(&mut self, offset: u32, value: u16) -> bool {
        let Some(range) = self.range(offset, 2) else {
            return false;
        };
        self.bytes_mut()[range].copy_from_slice(&value.to_le_bytes());
        true
    }

    pub fn read_u32_le(&self, offset: u32) -> Option<u32> {
        let range = self.range(offset, 4)?;
        Some(u32::from_le_bytes(self.bytes()[range].try_into().ok()?))
    }

    pub fn write_u32_le(&mut self, offset: u32, value: u32) -> bool {
        let Some(range) = self.range(offset, 4) else {
            return false;
        };
        self.bytes_mut()[range].copy_from_slice(&value.to_le_bytes());
        true
    }

    pub fn read_u64_le(&self, offset: u32) -> Option<u64> {
        let range = self.range(offset, 8)?;
        Some(u64::from_le_bytes(self.bytes()[range].try_into().ok()?))
    }

    pub fn write_u64_le(&mut self, offset: u32, value: u64) -> bool {
        let Some(range) = self.range(offset, 8) else {
            return false;
        };
        self.bytes_mut()[range].copy_from_slice(&value.to_le_bytes());
        true
    }
}

#[derive(Clone)]
pub struct Function {
    pub signature: Signature,
    body: Arc<[Instruction]>,
    host: Option<HostFuncRef>,
    pub table_reference: Option<u64>,
}

impl Debug for Function {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Function")
            .field("signature", &self.signature)
            .field("body_len", &self.body.len())
            .field("is_host", &self.host.is_some())
            .finish()
    }
}

impl Function {
    pub fn new_native(
        signature: impl Into<Signature>,
        body: Vec<Instruction>,
    ) -> RuntimeResult<Self> {
        let mut body = body;
        resolve_labels(&mut body)?;
        Ok(Self {
            signature: signature.into(),
            body: Arc::from(body),
            host: None,
            table_reference: None,
        })
    }

    pub fn new_host(signature: impl Into<Signature>, host: HostFuncRef) -> Self {
        Self {
            signature: signature.into(),
            body: Arc::from([]),
            host: Some(host),
            table_reference: None,
        }
    }

    pub fn new_host_with_table_reference(
        signature: impl Into<Signature>,
        host: HostFuncRef,
        table_reference: u64,
    ) -> Self {
        Self {
            signature: signature.into(),
            body: Arc::from([]),
            host: Some(host),
            table_reference: Some(table_reference),
        }
    }

    pub fn body(&self) -> &[Instruction] {
        &self.body
    }
}

#[derive(Debug, Clone, Default)]
pub struct Module {
    pub functions: Vec<Function>,
    pub globals: Vec<GlobalValue>,
    pub memory: Option<Memory>,
    pub tables: Vec<Table>,
    pub types: Vec<Signature>,
    pub data_instances: Vec<Option<Vec<u8>>>,
    pub element_instances: Vec<Option<Vec<Option<u64>>>>,
    pub closed: ModuleCloseState,
}

impl Module {
    pub fn fail_if_closed(&self) -> RuntimeResult<()> {
        match self.closed.exit_code() {
            Some(exit_code) => Err(Trap::new(format!("module exited with code {exit_code}"))),
            None => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveCallFrame {
    pub function_index: usize,
    pub program_counter: usize,
}

pub fn active_host_call_stack() -> Option<Vec<ActiveCallFrame>> {
    ACTIVE_HOST_CALL_STACKS.with(|active| active.borrow().last().cloned())
}

fn with_active_host_call_stack<T>(frames: &[CallFrame], f: impl FnOnce() -> T) -> T {
    let snapshot = frames
        .iter()
        .rev()
        .map(|frame| ActiveCallFrame {
            function_index: frame.function_index,
            program_counter: frame.pc,
        })
        .collect();
    ACTIVE_HOST_CALL_STACKS.with(|active| active.borrow_mut().push(snapshot));
    let result = f();
    ACTIVE_HOST_CALL_STACKS.with(|active| {
        active.borrow_mut().pop();
    });
    result
}

#[derive(Debug, Clone, Default)]
pub struct Interpreter {
    pub module: Module,
    pub call_stack_ceiling: usize,
}

#[derive(Debug)]
pub struct SuspendedCall {
    engine: CallEngine,
    root_function_index: usize,
}

pub struct InterpreterSuspend {
    pub payload: Box<dyn Any + Send>,
    pub suspended_call: SuspendedCall,
}

impl Interpreter {
    pub fn new(module: Module) -> Self {
        Self {
            module,
            call_stack_ceiling: DEFAULT_CALL_STACK_CEILING,
        }
    }

    pub fn call(&mut self, function_index: usize, params: &[u64]) -> RuntimeResult<Vec<u64>> {
        let function = self.function(function_index)?.clone();
        if params.len() != function.signature.param_slots {
            return Err(Trap::new(format!(
                "expected {} params, but passed {}",
                function.signature.param_slots,
                params.len()
            )));
        }

        self.module.fail_if_closed()?;

        let mut engine = CallEngine::default();
        engine.fuel_remaining = active_fuel_remaining();
        engine.push_values(params);
        self.execute(function_index, engine)
    }

    pub fn resume(
        &mut self,
        suspended_call: SuspendedCall,
        host_results: &[u64],
    ) -> RuntimeResult<Vec<u64>> {
        self.module.fail_if_closed()?;
        let mut engine = suspended_call.engine;
        engine.resume_host_function(self, host_results)?;
        self.execute(suspended_call.root_function_index, engine)
    }

    pub fn expected_host_result_count(
        &self,
        suspended_call: &SuspendedCall,
    ) -> RuntimeResult<usize> {
        let frame = suspended_call
            .engine
            .frames
            .last()
            .ok_or_else(|| Trap::new("host function resume is missing a suspended frame"))?;
        let function = self.function(frame.function_index)?;
        if function.host.is_none() {
            return Err(Trap::new(
                "host function resume expected a suspended host frame",
            ));
        }
        Ok(function.signature.result_slots)
    }

    fn execute(
        &mut self,
        root_function_index: usize,
        mut engine: CallEngine,
    ) -> RuntimeResult<Vec<u64>> {
        let result_slots = self.function(root_function_index)?.signature.result_slots;
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            if engine.frames.is_empty() {
                engine.call_function(self, root_function_index)?;
                engine.run_frames(self)
            } else {
                engine.run_frames(self)
            }
        }));
        match outcome {
            Ok(Ok(())) => {
                let mut results = vec![0; result_slots];
                engine.pop_values(&mut results);
                Ok(results)
            }
            Ok(Err(trap)) => Err(trap),
            Err(payload) => panic::panic_any(InterpreterSuspend {
                payload,
                suspended_call: SuspendedCall {
                    engine,
                    root_function_index,
                },
            }),
        }
    }

    fn function(&self, function_index: usize) -> RuntimeResult<&Function> {
        self.module
            .functions
            .get(function_index)
            .ok_or_else(|| Trap::new(format!("function[{function_index}] is undefined")))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CallFrame {
    pc: usize,
    function_index: usize,
    base: usize,
}

#[derive(Debug, Clone, Default)]
struct CallEngine {
    stack: Vec<u64>,
    frames: Vec<CallFrame>,
    fuel_remaining: Option<Arc<AtomicI64>>,
}

impl CallEngine {
    fn push_value(&mut self, value: u64) {
        self.stack.push(value);
    }

    fn push_values(&mut self, values: &[u64]) {
        self.stack.extend_from_slice(values);
    }

    fn pop_value(&mut self) -> u64 {
        self.stack
            .pop()
            .expect("validated interpreter stack underflow")
    }

    fn pop_values(&mut self, out: &mut [u64]) {
        if out.is_empty() {
            return;
        }
        let split = self.stack.len() - out.len();
        out.copy_from_slice(&self.stack[split..]);
        self.stack.truncate(split);
    }

    #[allow(dead_code)]
    fn peek_values(&self, count: usize) -> &[u64] {
        if count == 0 {
            return &[];
        }
        &self.stack[self.stack.len() - count..]
    }

    fn drop_range(&mut self, raw: u64) {
        let range = InclusiveRange::from_u64(raw);
        if range.start == -1 {
            return;
        }

        let len = self.stack.len() as i32;
        if range.start == 0 {
            self.stack.truncate((len - 1 - range.end) as usize);
            return;
        }

        let keep_until = (len - 1 - range.end) as usize;
        let move_from = (len - range.start) as usize;
        let mut retained = self.stack[..keep_until].to_vec();
        retained.extend_from_slice(&self.stack[move_from..]);
        self.stack = retained;
    }

    fn push_frame(&mut self, frame: CallFrame, call_stack_ceiling: usize) -> RuntimeResult<()> {
        if self.frames.len() >= call_stack_ceiling {
            return Err(Trap::new("stack overflow"));
        }
        self.frames.push(frame);
        Ok(())
    }

    fn pop_frame(&mut self) -> Option<CallFrame> {
        self.frames.pop()
    }

    fn call_function(
        &mut self,
        interpreter: &mut Interpreter,
        function_index: usize,
    ) -> RuntimeResult<()> {
        let function = interpreter
            .module
            .functions
            .get(function_index)
            .ok_or_else(|| Trap::new(format!("function[{function_index}] is undefined")))?
            .clone();

        if function.host.is_some() {
            self.call_host_function(interpreter, function_index, &function)
        } else {
            self.consume_fuel(1)?;
            self.call_native_function(interpreter, function_index, &function)
        }
    }

    fn consume_fuel(&self, amount: i64) -> RuntimeResult<()> {
        if amount <= 0 {
            return Ok(());
        }
        let Some(remaining) = &self.fuel_remaining else {
            return Ok(());
        };
        let previous = remaining.fetch_sub(amount, Ordering::SeqCst);
        if previous - amount < 0 {
            return Err(Trap::new("fuel exhausted"));
        }
        Ok(())
    }

    fn consume_fuel_for_branch(&self, pc: usize, target: usize) -> RuntimeResult<()> {
        if target < pc {
            self.consume_fuel(1)?;
        }
        Ok(())
    }

    fn call_host_function(
        &mut self,
        interpreter: &mut Interpreter,
        function_index: usize,
        function: &Function,
    ) -> RuntimeResult<()> {
        let signature = &function.signature;
        let param_len = signature.param_slots;
        let result_len = signature.result_slots;
        let stack_window_len = signature.stack_window_len();

        if stack_window_len > param_len {
            self.stack
                .extend(std::iter::repeat_n(0, stack_window_len - param_len));
        }

        self.push_frame(
            CallFrame {
                pc: 0,
                function_index,
                base: self.stack.len(),
            },
            interpreter.call_stack_ceiling,
        )?;

        let start = self.stack.len() - stack_window_len;
        let host = function
            .host
            .clone()
            .ok_or_else(|| Trap::new("host function missing implementation"))?;
        let result = with_active_host_call_stack(&self.frames, || {
            host.call(&mut interpreter.module, &mut self.stack[start..])
        });

        self.pop_frame();

        if param_len > result_len {
            self.stack
                .truncate(self.stack.len() - (param_len - result_len));
        }

        result
    }

    fn resume_host_function(
        &mut self,
        interpreter: &mut Interpreter,
        host_results: &[u64],
    ) -> RuntimeResult<()> {
        let frame = self
            .frames
            .last()
            .cloned()
            .ok_or_else(|| Trap::new("host function resume is missing a suspended frame"))?;
        let function = interpreter.function(frame.function_index)?.clone();
        if function.host.is_none() {
            return Err(Trap::new(
                "host function resume expected a suspended host frame",
            ));
        }
        let signature = &function.signature;
        if host_results.len() != signature.result_slots {
            return Err(Trap::new(format!(
                "expected {} host results, received {}",
                signature.result_slots,
                host_results.len()
            )));
        }
        let stack_window_len = signature.stack_window_len();
        if self.stack.len() < stack_window_len {
            return Err(Trap::new("host function resume stack is corrupted"));
        }
        let start = self.stack.len() - stack_window_len;
        self.stack[start..start + signature.result_slots].copy_from_slice(host_results);
        self.pop_frame();
        if signature.param_slots > signature.result_slots {
            self.stack
                .truncate(self.stack.len() - (signature.param_slots - signature.result_slots));
        }
        Ok(())
    }

    fn call_native_function(
        &mut self,
        interpreter: &mut Interpreter,
        function_index: usize,
        _function: &Function,
    ) -> RuntimeResult<()> {
        self.push_frame(
            CallFrame {
                pc: 0,
                function_index,
                base: self.stack.len(),
            },
            interpreter.call_stack_ceiling,
        )?;

        Ok(())
    }

    fn drop_for_tail_call(
        &mut self,
        frame: &CallFrame,
        current: &Function,
        target: &Function,
    ) -> RuntimeResult<()> {
        let base = frame
            .base
            .checked_sub(current.signature.param_slots)
            .ok_or_else(|| Trap::new("tail call stack is corrupted"))?;
        let param_count = target.signature.param_slots;
        if self.stack.len() < param_count {
            return Err(Trap::new("tail call stack is corrupted"));
        }
        let start = self.stack.len() - param_count;
        if base != start {
            self.stack.copy_within(start.., base);
        }
        self.stack.truncate(base + param_count);
        Ok(())
    }

    fn reset_tail_call_frame(&mut self, function_index: usize) -> RuntimeResult<()> {
        let frame = self
            .frames
            .last_mut()
            .ok_or_else(|| Trap::new("tail call missing active frame"))?;
        frame.function_index = function_index;
        frame.base = self.stack.len();
        frame.pc = 0;
        Ok(())
    }

    fn run_frames(&mut self, interpreter: &mut Interpreter) -> RuntimeResult<()> {
        while let Some(frame) = self.frames.last().cloned() {
            let function = interpreter.function(frame.function_index)?.clone();
            if function.host.is_some() {
                return Err(Trap::new(
                    "suspended host frame must be resumed before continuing",
                ));
            }
            let body = &function.body;
            if frame.pc >= body.len() {
                self.pop_frame();
                continue;
            }

            let op = body[frame.pc].clone();
            match op.kind {
                OperationKind::BuiltinFunctionCheckExitCode => {
                    interpreter.module.fail_if_closed()?;
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Unreachable => return Err(Trap::new("unreachable")),
                OperationKind::Label => self.frames.last_mut().expect("frame").pc += 1,
                OperationKind::Br => {
                    let target = op.u1 as usize;
                    self.consume_fuel_for_branch(frame.pc, target)?;
                    self.frames.last_mut().expect("frame").pc = target;
                }
                OperationKind::BrIf => {
                    if self.pop_value() > 0 {
                        self.drop_range(op.u3);
                        let target = op.u1 as usize;
                        self.consume_fuel_for_branch(frame.pc, target)?;
                        self.frames.last_mut().expect("frame").pc = target;
                    } else {
                        self.frames.last_mut().expect("frame").pc = op.u2 as usize;
                    }
                }
                OperationKind::BrTable => {
                    let value = self.pop_value();
                    let default_index = op.us.len() / 2 - 1;
                    let target_index = usize::try_from(value)
                        .ok()
                        .filter(|index| *index <= default_index)
                        .unwrap_or(default_index)
                        * 2;
                    self.drop_range(op.us[target_index + 1]);
                    let target = op.us[target_index] as usize;
                    self.consume_fuel_for_branch(frame.pc, target)?;
                    self.frames.last_mut().expect("frame").pc = target;
                }
                OperationKind::Call => {
                    self.frames.last_mut().expect("frame").pc += 1;
                    self.call_function(interpreter, op.u1 as usize)?;
                }
                OperationKind::CallIndirect => {
                    self.frames.last_mut().expect("frame").pc += 1;
                    let table_offset = self.pop_value() as usize;
                    let table = interpreter
                        .module
                        .tables
                        .get(op.u2 as usize)
                        .ok_or_else(|| Trap::new(format!("table[{}] is undefined", op.u2)))?;
                    let function_index = table
                        .get(table_offset)
                        .flatten()
                        .and_then(|reference| usize::try_from(reference).ok())
                        .ok_or_else(|| Trap::new("invalid table access"))?;
                    let expected = interpreter
                        .module
                        .types
                        .get(op.u1 as usize)
                        .ok_or_else(|| Trap::new(format!("type[{}] is undefined", op.u1)))?;
                    let actual = &interpreter
                        .module
                        .functions
                        .get(function_index)
                        .ok_or_else(|| Trap::new("invalid table access"))?
                        .signature;
                    if expected != actual {
                        return Err(Trap::new("indirect call type mismatch"));
                    }
                    self.call_function(interpreter, function_index)?;
                }
                OperationKind::TailCallReturnCall => {
                    let frame = self.frames.last().cloned().expect("frame");
                    let current = interpreter.function(frame.function_index)?.clone();
                    let function_index = op.u1 as usize;
                    let target = interpreter.function(function_index)?.clone();
                    self.drop_for_tail_call(&frame, &current, &target)?;
                    if target.host.is_some() {
                        self.call_function(interpreter, function_index)?;
                        self.pop_frame();
                    } else {
                        self.reset_tail_call_frame(function_index)?;
                    }
                }
                OperationKind::TailCallReturnCallIndirect => {
                    let table_offset = self.pop_value() as usize;
                    let table = interpreter
                        .module
                        .tables
                        .get(op.u2 as usize)
                        .ok_or_else(|| Trap::new(format!("table[{}] is undefined", op.u2)))?;
                    let function_index = table
                        .get(table_offset)
                        .flatten()
                        .and_then(|reference| usize::try_from(reference).ok())
                        .ok_or_else(|| Trap::new("invalid table access"))?;
                    let expected = interpreter
                        .module
                        .types
                        .get(op.u1 as usize)
                        .ok_or_else(|| Trap::new(format!("type[{}] is undefined", op.u1)))?;
                    let actual = &interpreter
                        .module
                        .functions
                        .get(function_index)
                        .ok_or_else(|| Trap::new("invalid table access"))?
                        .signature;
                    if expected != actual {
                        return Err(Trap::new("indirect call type mismatch"));
                    }
                    let frame = self.frames.last().cloned().expect("frame");
                    let current = interpreter.function(frame.function_index)?.clone();
                    let target = interpreter.function(function_index)?.clone();
                    if target.host.is_some() {
                        self.call_function(interpreter, function_index)?;
                        if let Some(raw) = op.us.first().copied() {
                            self.drop_range(raw);
                        }
                        self.frames.last_mut().expect("frame").pc =
                            op.us.get(1).copied().unwrap_or(u64::MAX) as usize;
                    } else {
                        self.drop_for_tail_call(&frame, &current, &target)?;
                        self.reset_tail_call_frame(function_index)?;
                    }
                }
                OperationKind::Drop => {
                    self.drop_range(op.u1);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Select => {
                    let condition = self.pop_value();
                    if op.b3 {
                        let x2_hi = self.pop_value();
                        let x2_lo = self.pop_value();
                        if condition == 0 {
                            let _ = self.pop_value();
                            let _ = self.pop_value();
                            self.push_value(x2_lo);
                            self.push_value(x2_hi);
                        }
                    } else {
                        let v2 = self.pop_value();
                        if condition == 0 {
                            let _ = self.pop_value();
                            self.push_value(v2);
                        }
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Pick => {
                    let index = self.stack.len() - 1 - op.u1 as usize;
                    self.push_value(self.stack[index]);
                    if op.b3 {
                        self.push_value(self.stack[index + 1]);
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Set => {
                    if op.b3 {
                        let low_index = self.stack.len() - 1 - op.u1 as usize;
                        let high_index = low_index + 1;
                        let hi = self.pop_value();
                        let lo = self.pop_value();
                        self.stack[low_index] = lo;
                        self.stack[high_index] = hi;
                    } else {
                        let index = self.stack.len() - 1 - op.u1 as usize;
                        self.stack[index] = self.pop_value();
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::GlobalGet => {
                    let global = interpreter
                        .module
                        .globals
                        .get(op.u1 as usize)
                        .ok_or_else(|| Trap::new(format!("global[{}] is undefined", op.u1)))?;
                    self.push_value(global.lo);
                    if global.is_vector {
                        self.push_value(global.hi);
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::GlobalSet => {
                    let global = interpreter
                        .module
                        .globals
                        .get_mut(op.u1 as usize)
                        .ok_or_else(|| Trap::new(format!("global[{}] is undefined", op.u1)))?;
                    if global.is_vector {
                        global.hi = self.pop_value();
                    }
                    global.lo = self.pop_value();
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Load => {
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_ref()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    match decode_unsigned_type(op.b1) {
                        UnsignedType::I32 | UnsignedType::F32 => {
                            self.push_value(
                                memory
                                    .read_u32_le(offset)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?
                                    as u64,
                            );
                        }
                        UnsignedType::I64 | UnsignedType::F64 => self.push_value(
                            memory
                                .read_u64_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                        ),
                        UnsignedType::V128 | UnsignedType::Unknown => {
                            return Err(Trap::new("unsupported load type"));
                        }
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Load8 => {
                    let offset = self.pop_memory_offset(&op)?;
                    let value = interpreter
                        .module
                        .memory
                        .as_ref()
                        .ok_or_else(|| Trap::new("memory is undefined"))?
                        .read_byte(offset)
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    let extended = match decode_signed_int(op.b1) {
                        SignedInt::Int32 => u64::from(value as i8 as i32 as u32),
                        SignedInt::Int64 => value as i8 as i64 as u64,
                        SignedInt::Uint32 | SignedInt::Uint64 => u64::from(value),
                    };
                    self.push_value(extended);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Load16 => {
                    let offset = self.pop_memory_offset(&op)?;
                    let value = interpreter
                        .module
                        .memory
                        .as_ref()
                        .ok_or_else(|| Trap::new("memory is undefined"))?
                        .read_u16_le(offset)
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    let extended = match decode_signed_int(op.b1) {
                        SignedInt::Int32 => u64::from(value as i16 as i32 as u32),
                        SignedInt::Int64 => value as i16 as i64 as u64,
                        SignedInt::Uint32 | SignedInt::Uint64 => u64::from(value),
                    };
                    self.push_value(extended);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Load32 => {
                    let offset = self.pop_memory_offset(&op)?;
                    let value = interpreter
                        .module
                        .memory
                        .as_ref()
                        .ok_or_else(|| Trap::new("memory is undefined"))?
                        .read_u32_le(offset)
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    self.push_value(if op.b1 == 1 {
                        value as i32 as i64 as u64
                    } else {
                        u64::from(value)
                    });
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Store => {
                    let value = self.pop_value();
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let ok = match decode_unsigned_type(op.b1) {
                        UnsignedType::I32 | UnsignedType::F32 => {
                            memory.write_u32_le(offset, value as u32)
                        }
                        UnsignedType::I64 | UnsignedType::F64 => memory.write_u64_le(offset, value),
                        UnsignedType::V128 | UnsignedType::Unknown => false,
                    };
                    if !ok {
                        return Err(Trap::new("out of bounds memory access"));
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Store8 => {
                    let value = self.pop_value() as u8;
                    let offset = self.pop_memory_offset(&op)?;
                    if !interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?
                        .write_byte(offset, value)
                    {
                        return Err(Trap::new("out of bounds memory access"));
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Store16 => {
                    let value = self.pop_value() as u16;
                    let offset = self.pop_memory_offset(&op)?;
                    if !interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?
                        .write_u16_le(offset, value)
                    {
                        return Err(Trap::new("out of bounds memory access"));
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Store32 => {
                    let value = self.pop_value() as u32;
                    let offset = self.pop_memory_offset(&op)?;
                    if !interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?
                        .write_u32_le(offset, value)
                    {
                        return Err(Trap::new("out of bounds memory access"));
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::MemorySize => {
                    let pages = interpreter.module.memory.as_ref().map_or(0, Memory::pages);
                    self.push_value(u64::from(pages));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::MemoryGrow => {
                    let additional_pages = self.pop_value() as u32;
                    let grown = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?
                        .grow(additional_pages);
                    self.push_value(grown.map_or(u64::from(u32::MAX), u64::from));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::ConstI32
                | OperationKind::ConstI64
                | OperationKind::ConstF32
                | OperationKind::ConstF64 => {
                    self.push_value(op.u1);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Const => {
                    self.push_value(op.u1);
                    self.push_value(op.u2);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Load => {
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_ref()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    match op.b1 {
                        x if x == V128LoadType::Load128 as u8 => {
                            let lo = memory
                                .read_u64_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            let hi = memory
                                .read_u64_le(
                                    offset
                                        .checked_add(8)
                                        .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                                )
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            self.push_value(lo);
                            self.push_value(hi);
                        }
                        x if x == V128LoadType::Load8x8S as u8 => {
                            let mut lo = 0_u64;
                            let mut hi = 0_u64;
                            for lane in 0..4 {
                                let value = memory
                                    .read_byte(offset + lane)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?
                                    as i8 as i16 as u16
                                    as u64;
                                lo |= value << (lane * 16);
                            }
                            for lane in 0..4 {
                                let value = memory
                                    .read_byte(offset + 4 + lane)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?
                                    as i8 as i16 as u16
                                    as u64;
                                hi |= value << (lane * 16);
                            }
                            self.push_value(lo);
                            self.push_value(hi);
                        }
                        x if x == V128LoadType::Load8x8U as u8 => {
                            let mut lo = 0_u64;
                            let mut hi = 0_u64;
                            for lane in 0..4 {
                                let value = u64::from(
                                    memory
                                        .read_byte(offset + lane)
                                        .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                                );
                                lo |= value << (lane * 16);
                            }
                            for lane in 0..4 {
                                let value = u64::from(
                                    memory
                                        .read_byte(offset + 4 + lane)
                                        .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                                );
                                hi |= value << (lane * 16);
                            }
                            self.push_value(lo);
                            self.push_value(hi);
                        }
                        x if x == V128LoadType::Load16x4S as u8 => {
                            let lo = u64::from(
                                memory
                                    .read_u16_le(offset)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?
                                    as i16 as i32 as u32,
                            ) | (u64::from(
                                memory
                                    .read_u16_le(offset + 2)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?
                                    as i16 as i32 as u32,
                            ) << 32);
                            let hi = u64::from(
                                memory
                                    .read_u16_le(offset + 4)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?
                                    as i16 as i32 as u32,
                            ) | (u64::from(
                                memory
                                    .read_u16_le(offset + 6)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?
                                    as i16 as i32 as u32,
                            ) << 32);
                            self.push_value(lo);
                            self.push_value(hi);
                        }
                        x if x == V128LoadType::Load16x4U as u8 => {
                            let lo = u64::from(
                                memory
                                    .read_u16_le(offset)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                            ) | (u64::from(
                                memory
                                    .read_u16_le(offset + 2)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                            ) << 32);
                            let hi = u64::from(
                                memory
                                    .read_u16_le(offset + 4)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                            ) | (u64::from(
                                memory
                                    .read_u16_le(offset + 6)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                            ) << 32);
                            self.push_value(lo);
                            self.push_value(hi);
                        }
                        x if x == V128LoadType::Load32x2S as u8 => {
                            self.push_value(
                                memory
                                    .read_u32_le(offset)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?
                                    as i32 as i64 as u64,
                            );
                            self.push_value(
                                memory
                                    .read_u32_le(offset + 4)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?
                                    as i32 as i64 as u64,
                            );
                        }
                        x if x == V128LoadType::Load32x2U as u8 => {
                            self.push_value(u64::from(
                                memory
                                    .read_u32_le(offset)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                            ));
                            self.push_value(u64::from(
                                memory
                                    .read_u32_le(offset + 4)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                            ));
                        }
                        x if x == V128LoadType::Load8Splat as u8 => {
                            let value = u64::from(
                                memory
                                    .read_byte(offset)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                            );
                            let splat = value
                                | (value << 8)
                                | (value << 16)
                                | (value << 24)
                                | (value << 32)
                                | (value << 40)
                                | (value << 48)
                                | (value << 56);
                            self.push_value(splat);
                            self.push_value(splat);
                        }
                        x if x == V128LoadType::Load16Splat as u8 => {
                            let value = u64::from(
                                memory
                                    .read_u16_le(offset)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                            );
                            let splat = value | (value << 16) | (value << 32) | (value << 48);
                            self.push_value(splat);
                            self.push_value(splat);
                        }
                        x if x == V128LoadType::Load32Splat as u8 => {
                            let value = u64::from(
                                memory
                                    .read_u32_le(offset)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                            );
                            let splat = value | (value << 32);
                            self.push_value(splat);
                            self.push_value(splat);
                        }
                        x if x == V128LoadType::Load64Splat as u8 => {
                            let value = memory
                                .read_u64_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            self.push_value(value);
                            self.push_value(value);
                        }
                        x if x == V128LoadType::Load32Zero as u8 => {
                            self.push_value(u64::from(
                                memory
                                    .read_u32_le(offset)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                            ));
                            self.push_value(0);
                        }
                        x if x == V128LoadType::Load64Zero as u8 => {
                            self.push_value(
                                memory
                                    .read_u64_le(offset)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                            );
                            self.push_value(0);
                        }
                        _ => return Err(Trap::new("unsupported v128 load type")),
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Store => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let upper_offset = offset
                        .checked_add(8)
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    if !memory.write_u64_le(upper_offset, hi) || !memory.write_u64_le(offset, lo) {
                        return Err(Trap::new("out of bounds memory access"));
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128LoadLane => {
                    let mut hi = self.pop_value();
                    let mut lo = self.pop_value();
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_ref()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    match op.b1 {
                        8 => {
                            let value = memory
                                .read_byte(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            if op.b2 < 8 {
                                let shift = op.b2 << 3;
                                lo = (lo & !(0xff_u64 << shift)) | (u64::from(value) << shift);
                            } else {
                                let shift = (op.b2 - 8) << 3;
                                hi = (hi & !(0xff_u64 << shift)) | (u64::from(value) << shift);
                            }
                        }
                        16 => {
                            let value = memory
                                .read_u16_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            if op.b2 < 4 {
                                let shift = op.b2 << 4;
                                lo = (lo & !(0xffff_u64 << shift)) | (u64::from(value) << shift);
                            } else {
                                let shift = (op.b2 - 4) << 4;
                                hi = (hi & !(0xffff_u64 << shift)) | (u64::from(value) << shift);
                            }
                        }
                        32 => {
                            let value = memory
                                .read_u32_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            if op.b2 < 2 {
                                let shift = op.b2 << 5;
                                lo = (lo & !(0xffff_ffff_u64 << shift))
                                    | (u64::from(value) << shift);
                            } else {
                                let shift = (op.b2 - 2) << 5;
                                hi = (hi & !(0xffff_ffff_u64 << shift))
                                    | (u64::from(value) << shift);
                            }
                        }
                        64 => {
                            let value = memory
                                .read_u64_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            if op.b2 == 0 {
                                lo = value;
                            } else {
                                hi = value;
                            }
                        }
                        _ => return Err(Trap::new("unsupported v128 load lane size")),
                    }
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128StoreLane => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let ok = match op.b1 {
                        8 => {
                            let value = if op.b2 < 8 {
                                byte_at(lo, op.b2 as usize)
                            } else {
                                byte_at(hi, (op.b2 - 8) as usize)
                            };
                            memory.write_byte(offset, value)
                        }
                        16 => {
                            let value = if op.b2 < 4 {
                                half_at(lo, op.b2 as usize)
                            } else {
                                half_at(hi, (op.b2 - 4) as usize)
                            };
                            memory.write_u16_le(offset, value)
                        }
                        32 => {
                            let value = if op.b2 < 2 {
                                (lo >> (op.b2 * 32)) as u32
                            } else {
                                (hi >> ((op.b2 - 2) * 32)) as u32
                            };
                            memory.write_u32_le(offset, value)
                        }
                        64 => {
                            let value = if op.b2 == 0 { lo } else { hi };
                            memory.write_u64_le(offset, value)
                        }
                        _ => return Err(Trap::new("unsupported v128 store lane size")),
                    };
                    if !ok {
                        return Err(Trap::new("out of bounds memory access"));
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128ReplaceLane => {
                    let value = self.pop_value();
                    let mut hi = self.pop_value();
                    let mut lo = self.pop_value();
                    match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            if op.b2 < 8 {
                                let shift = op.b2 << 3;
                                lo =
                                    (lo & !(0xff_u64 << shift)) | (u64::from(value as u8) << shift);
                            } else {
                                let shift = (op.b2 - 8) << 3;
                                hi =
                                    (hi & !(0xff_u64 << shift)) | (u64::from(value as u8) << shift);
                            }
                        }
                        x if x == Shape::I16x8 as u8 => {
                            if op.b2 < 4 {
                                let shift = op.b2 << 4;
                                lo = (lo & !(0xffff_u64 << shift))
                                    | (u64::from(value as u16) << shift);
                            } else {
                                let shift = (op.b2 - 4) << 4;
                                hi = (hi & !(0xffff_u64 << shift))
                                    | (u64::from(value as u16) << shift);
                            }
                        }
                        x if x == Shape::I32x4 as u8 || x == Shape::F32x4 as u8 => {
                            if op.b2 < 2 {
                                let shift = op.b2 << 5;
                                lo = (lo & !(0xffff_ffff_u64 << shift))
                                    | (u64::from(value as u32) << shift);
                            } else {
                                let shift = (op.b2 - 2) << 5;
                                hi = (hi & !(0xffff_ffff_u64 << shift))
                                    | (u64::from(value as u32) << shift);
                            }
                        }
                        x if x == Shape::I64x2 as u8 || x == Shape::F64x2 as u8 => {
                            if op.b2 == 0 {
                                lo = value;
                            } else {
                                hi = value;
                            }
                        }
                        _ => return Err(Trap::new("unsupported v128 replace lane shape")),
                    }
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128ExtractLane => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let value = match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            let lane = if op.b2 < 8 {
                                byte_at(lo, op.b2 as usize)
                            } else {
                                byte_at(hi, (op.b2 - 8) as usize)
                            };
                            if op.b3 {
                                u64::from(i32::from(lane as i8) as u32)
                            } else {
                                u64::from(lane)
                            }
                        }
                        x if x == Shape::I16x8 as u8 => {
                            let lane = if op.b2 < 4 {
                                half_at(lo, op.b2 as usize)
                            } else {
                                half_at(hi, (op.b2 - 4) as usize)
                            };
                            if op.b3 {
                                u64::from(i32::from(lane as i16) as u32)
                            } else {
                                u64::from(lane)
                            }
                        }
                        x if x == Shape::I32x4 as u8 || x == Shape::F32x4 as u8 => {
                            if op.b2 < 2 {
                                u64::from(((lo >> (op.b2 * 32)) & 0xffff_ffff) as u32)
                            } else {
                                u64::from(((hi >> ((op.b2 - 2) * 32)) & 0xffff_ffff) as u32)
                            }
                        }
                        x if x == Shape::I64x2 as u8 || x == Shape::F64x2 as u8 => {
                            if op.b2 == 0 {
                                lo
                            } else {
                                hi
                            }
                        }
                        _ => return Err(Trap::new("unsupported v128 extract lane shape")),
                    };
                    self.push_value(value);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Splat => {
                    let value = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            let byte = u64::from(value as u8);
                            let splat = byte
                                | (byte << 8)
                                | (byte << 16)
                                | (byte << 24)
                                | (byte << 32)
                                | (byte << 40)
                                | (byte << 48)
                                | (byte << 56);
                            (splat, splat)
                        }
                        x if x == Shape::I16x8 as u8 => {
                            let half = u64::from(value as u16);
                            let splat = half | (half << 16) | (half << 32) | (half << 48);
                            (splat, splat)
                        }
                        x if x == Shape::I32x4 as u8 || x == Shape::F32x4 as u8 => {
                            let word = u64::from(value as u32);
                            let splat = word | (word << 32);
                            (splat, splat)
                        }
                        x if x == Shape::I64x2 as u8 || x == Shape::F64x2 as u8 => (value, value),
                        _ => return Err(Trap::new("unsupported v128 splat shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Swizzle => {
                    let idx_hi = self.pop_value();
                    let idx_lo = self.pop_value();
                    let base_hi = self.pop_value();
                    let base_lo = self.pop_value();
                    let mut bytes = [0_u8; 16];
                    for (i, out) in bytes.iter_mut().enumerate() {
                        let id = if i < 8 {
                            byte_at(idx_lo, i)
                        } else {
                            byte_at(idx_hi, i - 8)
                        };
                        *out = if id < 8 {
                            byte_at(base_lo, id as usize)
                        } else if id < 16 {
                            byte_at(base_hi, (id - 8) as usize)
                        } else {
                            0
                        };
                    }
                    self.push_value(u64::from_le_bytes(bytes[0..8].try_into().expect("len")));
                    self.push_value(u64::from_le_bytes(bytes[8..16].try_into().expect("len")));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Shuffle => {
                    let x_hi = self.pop_value();
                    let x_lo = self.pop_value();
                    let y_hi = self.pop_value();
                    let y_lo = self.pop_value();
                    let mut bytes = [0_u8; 16];
                    for (i, lane) in op.us.iter().enumerate() {
                        bytes[i] = if *lane < 8 {
                            byte_at(y_lo, *lane as usize)
                        } else if *lane < 16 {
                            byte_at(y_hi, (*lane - 8) as usize)
                        } else if *lane < 24 {
                            byte_at(x_lo, (*lane - 16) as usize)
                        } else if *lane < 32 {
                            byte_at(x_hi, (*lane - 24) as usize)
                        } else {
                            0
                        };
                    }
                    self.push_value(u64::from_le_bytes(bytes[0..8].try_into().expect("len")));
                    self.push_value(u64::from_le_bytes(bytes[8..16].try_into().expect("len")));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128AnyTrue => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    self.push_value(u64::from(hi != 0 || lo != 0));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128AllTrue => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let result = match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            (lo as u8 != 0)
                                && ((lo >> 8) as u8 != 0)
                                && ((lo >> 16) as u8 != 0)
                                && ((lo >> 24) as u8 != 0)
                                && ((lo >> 32) as u8 != 0)
                                && ((lo >> 40) as u8 != 0)
                                && ((lo >> 48) as u8 != 0)
                                && ((lo >> 56) as u8 != 0)
                                && (hi as u8 != 0)
                                && ((hi >> 8) as u8 != 0)
                                && ((hi >> 16) as u8 != 0)
                                && ((hi >> 24) as u8 != 0)
                                && ((hi >> 32) as u8 != 0)
                                && ((hi >> 40) as u8 != 0)
                                && ((hi >> 48) as u8 != 0)
                                && ((hi >> 56) as u8 != 0)
                        }
                        x if x == Shape::I16x8 as u8 => {
                            (lo as u16 != 0)
                                && ((lo >> 16) as u16 != 0)
                                && ((lo >> 32) as u16 != 0)
                                && ((lo >> 48) as u16 != 0)
                                && (hi as u16 != 0)
                                && ((hi >> 16) as u16 != 0)
                                && ((hi >> 32) as u16 != 0)
                                && ((hi >> 48) as u16 != 0)
                        }
                        x if x == Shape::I32x4 as u8 => {
                            (lo as u32 != 0)
                                && ((lo >> 32) as u32 != 0)
                                && (hi as u32 != 0)
                                && ((hi >> 32) as u32 != 0)
                        }
                        x if x == Shape::I64x2 as u8 => lo != 0 && hi != 0,
                        _ => return Err(Trap::new("unsupported v128 all_true shape")),
                    };
                    self.push_value(u64::from(result));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128BitMask => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let mut result = 0_u64;
                    match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            for lane in 0..8 {
                                if ((lo >> (lane * 8)) as i8) < 0 {
                                    result |= 1 << lane;
                                }
                            }
                            for lane in 0..8 {
                                if ((hi >> (lane * 8)) as i8) < 0 {
                                    result |= 1 << (lane + 8);
                                }
                            }
                        }
                        x if x == Shape::I16x8 as u8 => {
                            for lane in 0..4 {
                                if ((lo >> (lane * 16)) as i16) < 0 {
                                    result |= 1 << lane;
                                }
                            }
                            for lane in 0..4 {
                                if ((hi >> (lane * 16)) as i16) < 0 {
                                    result |= 1 << (lane + 4);
                                }
                            }
                        }
                        x if x == Shape::I32x4 as u8 => {
                            for lane in 0..2 {
                                if ((lo >> (lane * 32)) as i32) < 0 {
                                    result |= 1 << lane;
                                }
                            }
                            for lane in 0..2 {
                                if ((hi >> (lane * 32)) as i32) < 0 {
                                    result |= 1 << (lane + 2);
                                }
                            }
                        }
                        x if x == Shape::I64x2 as u8 => {
                            if (lo as i64) < 0 {
                                result |= 0b01;
                            }
                            if (hi as i64) < 0 {
                                result |= 0b10;
                            }
                        }
                        _ => return Err(Trap::new("unsupported v128 bitmask shape")),
                    }
                    self.push_value(result);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128And => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    self.push_value(lhs_lo & rhs_lo);
                    self.push_value(lhs_hi & rhs_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Not => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    self.push_value(!lo);
                    self.push_value(!hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Or => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    self.push_value(lhs_lo | rhs_lo);
                    self.push_value(lhs_hi | rhs_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Xor => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    self.push_value(lhs_lo ^ rhs_lo);
                    self.push_value(lhs_hi ^ rhs_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Bitselect => {
                    let mask_hi = self.pop_value();
                    let mask_lo = self.pop_value();
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    self.push_value((lhs_lo & mask_lo) | (rhs_lo & !mask_lo));
                    self.push_value((lhs_hi & mask_hi) | (rhs_hi & !mask_hi));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128AndNot => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    self.push_value(lhs_lo & !rhs_lo);
                    self.push_value(lhs_hi & !rhs_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Cmp => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let cmp = *V128CmpType::ALL
                        .get(op.b1 as usize)
                        .ok_or_else(|| Trap::new("unsupported v128 compare type"))?;
                    let (lo, hi) = match cmp {
                        V128CmpType::I8x16Eq => (
                            u64::from(mask8(lhs_lo as u8 == rhs_lo as u8))
                                | (u64::from(mask8((lhs_lo >> 8) as u8 == (rhs_lo >> 8) as u8))
                                    << 8)
                                | (u64::from(mask8((lhs_lo >> 16) as u8 == (rhs_lo >> 16) as u8))
                                    << 16)
                                | (u64::from(mask8((lhs_lo >> 24) as u8 == (rhs_lo >> 24) as u8))
                                    << 24)
                                | (u64::from(mask8((lhs_lo >> 32) as u8 == (rhs_lo >> 32) as u8))
                                    << 32)
                                | (u64::from(mask8((lhs_lo >> 40) as u8 == (rhs_lo >> 40) as u8))
                                    << 40)
                                | (u64::from(mask8((lhs_lo >> 48) as u8 == (rhs_lo >> 48) as u8))
                                    << 48)
                                | (u64::from(mask8((lhs_lo >> 56) as u8 == (rhs_lo >> 56) as u8))
                                    << 56),
                            u64::from(mask8(lhs_hi as u8 == rhs_hi as u8))
                                | (u64::from(mask8((lhs_hi >> 8) as u8 == (rhs_hi >> 8) as u8))
                                    << 8)
                                | (u64::from(mask8((lhs_hi >> 16) as u8 == (rhs_hi >> 16) as u8))
                                    << 16)
                                | (u64::from(mask8((lhs_hi >> 24) as u8 == (rhs_hi >> 24) as u8))
                                    << 24)
                                | (u64::from(mask8((lhs_hi >> 32) as u8 == (rhs_hi >> 32) as u8))
                                    << 32)
                                | (u64::from(mask8((lhs_hi >> 40) as u8 == (rhs_hi >> 40) as u8))
                                    << 40)
                                | (u64::from(mask8((lhs_hi >> 48) as u8 == (rhs_hi >> 48) as u8))
                                    << 48)
                                | (u64::from(mask8((lhs_hi >> 56) as u8 == (rhs_hi >> 56) as u8))
                                    << 56),
                        ),
                        V128CmpType::I8x16Ne => (
                            u64::from(mask8(lhs_lo as u8 != rhs_lo as u8))
                                | (u64::from(mask8((lhs_lo >> 8) as u8 != (rhs_lo >> 8) as u8))
                                    << 8)
                                | (u64::from(mask8((lhs_lo >> 16) as u8 != (rhs_lo >> 16) as u8))
                                    << 16)
                                | (u64::from(mask8((lhs_lo >> 24) as u8 != (rhs_lo >> 24) as u8))
                                    << 24)
                                | (u64::from(mask8((lhs_lo >> 32) as u8 != (rhs_lo >> 32) as u8))
                                    << 32)
                                | (u64::from(mask8((lhs_lo >> 40) as u8 != (rhs_lo >> 40) as u8))
                                    << 40)
                                | (u64::from(mask8((lhs_lo >> 48) as u8 != (rhs_lo >> 48) as u8))
                                    << 48)
                                | (u64::from(mask8((lhs_lo >> 56) as u8 != (rhs_lo >> 56) as u8))
                                    << 56),
                            u64::from(mask8(lhs_hi as u8 != rhs_hi as u8))
                                | (u64::from(mask8((lhs_hi >> 8) as u8 != (rhs_hi >> 8) as u8))
                                    << 8)
                                | (u64::from(mask8((lhs_hi >> 16) as u8 != (rhs_hi >> 16) as u8))
                                    << 16)
                                | (u64::from(mask8((lhs_hi >> 24) as u8 != (rhs_hi >> 24) as u8))
                                    << 24)
                                | (u64::from(mask8((lhs_hi >> 32) as u8 != (rhs_hi >> 32) as u8))
                                    << 32)
                                | (u64::from(mask8((lhs_hi >> 40) as u8 != (rhs_hi >> 40) as u8))
                                    << 40)
                                | (u64::from(mask8((lhs_hi >> 48) as u8 != (rhs_hi >> 48) as u8))
                                    << 48)
                                | (u64::from(mask8((lhs_hi >> 56) as u8 != (rhs_hi >> 56) as u8))
                                    << 56),
                        ),
                        V128CmpType::I8x16LtS => (
                            u64::from(mask8((lhs_lo as i8) < (rhs_lo as i8)))
                                | (u64::from(mask8(((lhs_lo >> 8) as i8) < ((rhs_lo >> 8) as i8)))
                                    << 8)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 16) as i8) < ((rhs_lo >> 16) as i8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 24) as i8) < ((rhs_lo >> 24) as i8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 32) as i8) < ((rhs_lo >> 32) as i8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 40) as i8) < ((rhs_lo >> 40) as i8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 48) as i8) < ((rhs_lo >> 48) as i8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 56) as i8) < ((rhs_lo >> 56) as i8),
                                )) << 56),
                            u64::from(mask8((lhs_hi as i8) < (rhs_hi as i8)))
                                | (u64::from(mask8(((lhs_hi >> 8) as i8) < ((rhs_hi >> 8) as i8)))
                                    << 8)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 16) as i8) < ((rhs_hi >> 16) as i8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 24) as i8) < ((rhs_hi >> 24) as i8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 32) as i8) < ((rhs_hi >> 32) as i8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 40) as i8) < ((rhs_hi >> 40) as i8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 48) as i8) < ((rhs_hi >> 48) as i8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 56) as i8) < ((rhs_hi >> 56) as i8),
                                )) << 56),
                        ),
                        V128CmpType::I8x16LtU => (
                            u64::from(mask8((lhs_lo as u8) < (rhs_lo as u8)))
                                | (u64::from(mask8(((lhs_lo >> 8) as u8) < ((rhs_lo >> 8) as u8)))
                                    << 8)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 16) as u8) < ((rhs_lo >> 16) as u8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 24) as u8) < ((rhs_lo >> 24) as u8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 32) as u8) < ((rhs_lo >> 32) as u8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 40) as u8) < ((rhs_lo >> 40) as u8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 48) as u8) < ((rhs_lo >> 48) as u8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 56) as u8) < ((rhs_lo >> 56) as u8),
                                )) << 56),
                            u64::from(mask8((lhs_hi as u8) < (rhs_hi as u8)))
                                | (u64::from(mask8(((lhs_hi >> 8) as u8) < ((rhs_hi >> 8) as u8)))
                                    << 8)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 16) as u8) < ((rhs_hi >> 16) as u8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 24) as u8) < ((rhs_hi >> 24) as u8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 32) as u8) < ((rhs_hi >> 32) as u8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 40) as u8) < ((rhs_hi >> 40) as u8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 48) as u8) < ((rhs_hi >> 48) as u8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 56) as u8) < ((rhs_hi >> 56) as u8),
                                )) << 56),
                        ),
                        V128CmpType::I8x16GtS => (
                            u64::from(mask8((lhs_lo as i8) > (rhs_lo as i8)))
                                | (u64::from(mask8(((lhs_lo >> 8) as i8) > ((rhs_lo >> 8) as i8)))
                                    << 8)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 16) as i8) > ((rhs_lo >> 16) as i8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 24) as i8) > ((rhs_lo >> 24) as i8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 32) as i8) > ((rhs_lo >> 32) as i8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 40) as i8) > ((rhs_lo >> 40) as i8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 48) as i8) > ((rhs_lo >> 48) as i8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 56) as i8) > ((rhs_lo >> 56) as i8),
                                )) << 56),
                            u64::from(mask8((lhs_hi as i8) > (rhs_hi as i8)))
                                | (u64::from(mask8(((lhs_hi >> 8) as i8) > ((rhs_hi >> 8) as i8)))
                                    << 8)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 16) as i8) > ((rhs_hi >> 16) as i8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 24) as i8) > ((rhs_hi >> 24) as i8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 32) as i8) > ((rhs_hi >> 32) as i8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 40) as i8) > ((rhs_hi >> 40) as i8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 48) as i8) > ((rhs_hi >> 48) as i8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 56) as i8) > ((rhs_hi >> 56) as i8),
                                )) << 56),
                        ),
                        V128CmpType::I8x16GtU => (
                            u64::from(mask8((lhs_lo as u8) > (rhs_lo as u8)))
                                | (u64::from(mask8(((lhs_lo >> 8) as u8) > ((rhs_lo >> 8) as u8)))
                                    << 8)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 16) as u8) > ((rhs_lo >> 16) as u8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 24) as u8) > ((rhs_lo >> 24) as u8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 32) as u8) > ((rhs_lo >> 32) as u8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 40) as u8) > ((rhs_lo >> 40) as u8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 48) as u8) > ((rhs_lo >> 48) as u8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 56) as u8) > ((rhs_lo >> 56) as u8),
                                )) << 56),
                            u64::from(mask8((lhs_hi as u8) > (rhs_hi as u8)))
                                | (u64::from(mask8(((lhs_hi >> 8) as u8) > ((rhs_hi >> 8) as u8)))
                                    << 8)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 16) as u8) > ((rhs_hi >> 16) as u8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 24) as u8) > ((rhs_hi >> 24) as u8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 32) as u8) > ((rhs_hi >> 32) as u8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 40) as u8) > ((rhs_hi >> 40) as u8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 48) as u8) > ((rhs_hi >> 48) as u8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 56) as u8) > ((rhs_hi >> 56) as u8),
                                )) << 56),
                        ),
                        V128CmpType::I8x16LeS => (
                            u64::from(mask8((lhs_lo as i8) <= (rhs_lo as i8)))
                                | (u64::from(mask8(
                                    ((lhs_lo >> 8) as i8) <= ((rhs_lo >> 8) as i8),
                                )) << 8)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 16) as i8) <= ((rhs_lo >> 16) as i8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 24) as i8) <= ((rhs_lo >> 24) as i8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 32) as i8) <= ((rhs_lo >> 32) as i8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 40) as i8) <= ((rhs_lo >> 40) as i8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 48) as i8) <= ((rhs_lo >> 48) as i8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 56) as i8) <= ((rhs_lo >> 56) as i8),
                                )) << 56),
                            u64::from(mask8((lhs_hi as i8) <= (rhs_hi as i8)))
                                | (u64::from(mask8(
                                    ((lhs_hi >> 8) as i8) <= ((rhs_hi >> 8) as i8),
                                )) << 8)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 16) as i8) <= ((rhs_hi >> 16) as i8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 24) as i8) <= ((rhs_hi >> 24) as i8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 32) as i8) <= ((rhs_hi >> 32) as i8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 40) as i8) <= ((rhs_hi >> 40) as i8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 48) as i8) <= ((rhs_hi >> 48) as i8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 56) as i8) <= ((rhs_hi >> 56) as i8),
                                )) << 56),
                        ),
                        V128CmpType::I8x16LeU => (
                            u64::from(mask8((lhs_lo as u8) <= (rhs_lo as u8)))
                                | (u64::from(mask8(
                                    ((lhs_lo >> 8) as u8) <= ((rhs_lo >> 8) as u8),
                                )) << 8)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 16) as u8) <= ((rhs_lo >> 16) as u8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 24) as u8) <= ((rhs_lo >> 24) as u8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 32) as u8) <= ((rhs_lo >> 32) as u8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 40) as u8) <= ((rhs_lo >> 40) as u8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 48) as u8) <= ((rhs_lo >> 48) as u8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 56) as u8) <= ((rhs_lo >> 56) as u8),
                                )) << 56),
                            u64::from(mask8((lhs_hi as u8) <= (rhs_hi as u8)))
                                | (u64::from(mask8(
                                    ((lhs_hi >> 8) as u8) <= ((rhs_hi >> 8) as u8),
                                )) << 8)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 16) as u8) <= ((rhs_hi >> 16) as u8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 24) as u8) <= ((rhs_hi >> 24) as u8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 32) as u8) <= ((rhs_hi >> 32) as u8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 40) as u8) <= ((rhs_hi >> 40) as u8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 48) as u8) <= ((rhs_hi >> 48) as u8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 56) as u8) <= ((rhs_hi >> 56) as u8),
                                )) << 56),
                        ),
                        V128CmpType::I8x16GeS => (
                            u64::from(mask8((lhs_lo as i8) >= (rhs_lo as i8)))
                                | (u64::from(mask8(
                                    ((lhs_lo >> 8) as i8) >= ((rhs_lo >> 8) as i8),
                                )) << 8)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 16) as i8) >= ((rhs_lo >> 16) as i8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 24) as i8) >= ((rhs_lo >> 24) as i8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 32) as i8) >= ((rhs_lo >> 32) as i8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 40) as i8) >= ((rhs_lo >> 40) as i8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 48) as i8) >= ((rhs_lo >> 48) as i8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 56) as i8) >= ((rhs_lo >> 56) as i8),
                                )) << 56),
                            u64::from(mask8((lhs_hi as i8) >= (rhs_hi as i8)))
                                | (u64::from(mask8(
                                    ((lhs_hi >> 8) as i8) >= ((rhs_hi >> 8) as i8),
                                )) << 8)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 16) as i8) >= ((rhs_hi >> 16) as i8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 24) as i8) >= ((rhs_hi >> 24) as i8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 32) as i8) >= ((rhs_hi >> 32) as i8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 40) as i8) >= ((rhs_hi >> 40) as i8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 48) as i8) >= ((rhs_hi >> 48) as i8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 56) as i8) >= ((rhs_hi >> 56) as i8),
                                )) << 56),
                        ),
                        V128CmpType::I8x16GeU => (
                            u64::from(mask8((lhs_lo as u8) >= (rhs_lo as u8)))
                                | (u64::from(mask8(
                                    ((lhs_lo >> 8) as u8) >= ((rhs_lo >> 8) as u8),
                                )) << 8)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 16) as u8) >= ((rhs_lo >> 16) as u8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 24) as u8) >= ((rhs_lo >> 24) as u8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 32) as u8) >= ((rhs_lo >> 32) as u8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 40) as u8) >= ((rhs_lo >> 40) as u8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 48) as u8) >= ((rhs_lo >> 48) as u8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_lo >> 56) as u8) >= ((rhs_lo >> 56) as u8),
                                )) << 56),
                            u64::from(mask8((lhs_hi as u8) >= (rhs_hi as u8)))
                                | (u64::from(mask8(
                                    ((lhs_hi >> 8) as u8) >= ((rhs_hi >> 8) as u8),
                                )) << 8)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 16) as u8) >= ((rhs_hi >> 16) as u8),
                                )) << 16)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 24) as u8) >= ((rhs_hi >> 24) as u8),
                                )) << 24)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 32) as u8) >= ((rhs_hi >> 32) as u8),
                                )) << 32)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 40) as u8) >= ((rhs_hi >> 40) as u8),
                                )) << 40)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 48) as u8) >= ((rhs_hi >> 48) as u8),
                                )) << 48)
                                | (u64::from(mask8(
                                    ((lhs_hi >> 56) as u8) >= ((rhs_hi >> 56) as u8),
                                )) << 56),
                        ),
                        V128CmpType::I16x8Eq => (
                            u64::from(mask16(lhs_lo as u16 == rhs_lo as u16))
                                | (u64::from(mask16(
                                    (lhs_lo >> 16) as u16 == (rhs_lo >> 16) as u16,
                                )) << 16)
                                | (u64::from(mask16(
                                    (lhs_lo >> 32) as u16 == (rhs_lo >> 32) as u16,
                                )) << 32)
                                | (u64::from(mask16(
                                    (lhs_lo >> 48) as u16 == (rhs_lo >> 48) as u16,
                                )) << 48),
                            u64::from(mask16(lhs_hi as u16 == rhs_hi as u16))
                                | (u64::from(mask16(
                                    (lhs_hi >> 16) as u16 == (rhs_hi >> 16) as u16,
                                )) << 16)
                                | (u64::from(mask16(
                                    (lhs_hi >> 32) as u16 == (rhs_hi >> 32) as u16,
                                )) << 32)
                                | (u64::from(mask16(
                                    (lhs_hi >> 48) as u16 == (rhs_hi >> 48) as u16,
                                )) << 48),
                        ),
                        V128CmpType::I16x8Ne => (
                            u64::from(mask16(lhs_lo as u16 != rhs_lo as u16))
                                | (u64::from(mask16(
                                    (lhs_lo >> 16) as u16 != (rhs_lo >> 16) as u16,
                                )) << 16)
                                | (u64::from(mask16(
                                    (lhs_lo >> 32) as u16 != (rhs_lo >> 32) as u16,
                                )) << 32)
                                | (u64::from(mask16(
                                    (lhs_lo >> 48) as u16 != (rhs_lo >> 48) as u16,
                                )) << 48),
                            u64::from(mask16(lhs_hi as u16 != rhs_hi as u16))
                                | (u64::from(mask16(
                                    (lhs_hi >> 16) as u16 != (rhs_hi >> 16) as u16,
                                )) << 16)
                                | (u64::from(mask16(
                                    (lhs_hi >> 32) as u16 != (rhs_hi >> 32) as u16,
                                )) << 32)
                                | (u64::from(mask16(
                                    (lhs_hi >> 48) as u16 != (rhs_hi >> 48) as u16,
                                )) << 48),
                        ),
                        V128CmpType::I16x8LtS => (
                            u64::from(mask16((lhs_lo as i16) < (rhs_lo as i16)))
                                | (u64::from(mask16(
                                    ((lhs_lo >> 16) as i16) < ((rhs_lo >> 16) as i16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 32) as i16) < ((rhs_lo >> 32) as i16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 48) as i16) < ((rhs_lo >> 48) as i16),
                                )) << 48),
                            u64::from(mask16((lhs_hi as i16) < (rhs_hi as i16)))
                                | (u64::from(mask16(
                                    ((lhs_hi >> 16) as i16) < ((rhs_hi >> 16) as i16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 32) as i16) < ((rhs_hi >> 32) as i16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 48) as i16) < ((rhs_hi >> 48) as i16),
                                )) << 48),
                        ),
                        V128CmpType::I16x8LtU => (
                            u64::from(mask16((lhs_lo as u16) < (rhs_lo as u16)))
                                | (u64::from(mask16(
                                    ((lhs_lo >> 16) as u16) < ((rhs_lo >> 16) as u16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 32) as u16) < ((rhs_lo >> 32) as u16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 48) as u16) < ((rhs_lo >> 48) as u16),
                                )) << 48),
                            u64::from(mask16((lhs_hi as u16) < (rhs_hi as u16)))
                                | (u64::from(mask16(
                                    ((lhs_hi >> 16) as u16) < ((rhs_hi >> 16) as u16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 32) as u16) < ((rhs_hi >> 32) as u16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 48) as u16) < ((rhs_hi >> 48) as u16),
                                )) << 48),
                        ),
                        V128CmpType::I16x8GtS => (
                            u64::from(mask16((lhs_lo as i16) > (rhs_lo as i16)))
                                | (u64::from(mask16(
                                    ((lhs_lo >> 16) as i16) > ((rhs_lo >> 16) as i16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 32) as i16) > ((rhs_lo >> 32) as i16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 48) as i16) > ((rhs_lo >> 48) as i16),
                                )) << 48),
                            u64::from(mask16((lhs_hi as i16) > (rhs_hi as i16)))
                                | (u64::from(mask16(
                                    ((lhs_hi >> 16) as i16) > ((rhs_hi >> 16) as i16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 32) as i16) > ((rhs_hi >> 32) as i16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 48) as i16) > ((rhs_hi >> 48) as i16),
                                )) << 48),
                        ),
                        V128CmpType::I16x8GtU => (
                            u64::from(mask16((lhs_lo as u16) > (rhs_lo as u16)))
                                | (u64::from(mask16(
                                    ((lhs_lo >> 16) as u16) > ((rhs_lo >> 16) as u16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 32) as u16) > ((rhs_lo >> 32) as u16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 48) as u16) > ((rhs_lo >> 48) as u16),
                                )) << 48),
                            u64::from(mask16((lhs_hi as u16) > (rhs_hi as u16)))
                                | (u64::from(mask16(
                                    ((lhs_hi >> 16) as u16) > ((rhs_hi >> 16) as u16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 32) as u16) > ((rhs_hi >> 32) as u16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 48) as u16) > ((rhs_hi >> 48) as u16),
                                )) << 48),
                        ),
                        V128CmpType::I16x8LeS => (
                            u64::from(mask16((lhs_lo as i16) <= (rhs_lo as i16)))
                                | (u64::from(mask16(
                                    ((lhs_lo >> 16) as i16) <= ((rhs_lo >> 16) as i16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 32) as i16) <= ((rhs_lo >> 32) as i16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 48) as i16) <= ((rhs_lo >> 48) as i16),
                                )) << 48),
                            u64::from(mask16((lhs_hi as i16) <= (rhs_hi as i16)))
                                | (u64::from(mask16(
                                    ((lhs_hi >> 16) as i16) <= ((rhs_hi >> 16) as i16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 32) as i16) <= ((rhs_hi >> 32) as i16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 48) as i16) <= ((rhs_hi >> 48) as i16),
                                )) << 48),
                        ),
                        V128CmpType::I16x8LeU => (
                            u64::from(mask16((lhs_lo as u16) <= (rhs_lo as u16)))
                                | (u64::from(mask16(
                                    ((lhs_lo >> 16) as u16) <= ((rhs_lo >> 16) as u16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 32) as u16) <= ((rhs_lo >> 32) as u16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 48) as u16) <= ((rhs_lo >> 48) as u16),
                                )) << 48),
                            u64::from(mask16((lhs_hi as u16) <= (rhs_hi as u16)))
                                | (u64::from(mask16(
                                    ((lhs_hi >> 16) as u16) <= ((rhs_hi >> 16) as u16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 32) as u16) <= ((rhs_hi >> 32) as u16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 48) as u16) <= ((rhs_hi >> 48) as u16),
                                )) << 48),
                        ),
                        V128CmpType::I16x8GeS => (
                            u64::from(mask16((lhs_lo as i16) >= (rhs_lo as i16)))
                                | (u64::from(mask16(
                                    ((lhs_lo >> 16) as i16) >= ((rhs_lo >> 16) as i16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 32) as i16) >= ((rhs_lo >> 32) as i16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 48) as i16) >= ((rhs_lo >> 48) as i16),
                                )) << 48),
                            u64::from(mask16((lhs_hi as i16) >= (rhs_hi as i16)))
                                | (u64::from(mask16(
                                    ((lhs_hi >> 16) as i16) >= ((rhs_hi >> 16) as i16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 32) as i16) >= ((rhs_hi >> 32) as i16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 48) as i16) >= ((rhs_hi >> 48) as i16),
                                )) << 48),
                        ),
                        V128CmpType::I16x8GeU => (
                            u64::from(mask16((lhs_lo as u16) >= (rhs_lo as u16)))
                                | (u64::from(mask16(
                                    ((lhs_lo >> 16) as u16) >= ((rhs_lo >> 16) as u16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 32) as u16) >= ((rhs_lo >> 32) as u16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_lo >> 48) as u16) >= ((rhs_lo >> 48) as u16),
                                )) << 48),
                            u64::from(mask16((lhs_hi as u16) >= (rhs_hi as u16)))
                                | (u64::from(mask16(
                                    ((lhs_hi >> 16) as u16) >= ((rhs_hi >> 16) as u16),
                                )) << 16)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 32) as u16) >= ((rhs_hi >> 32) as u16),
                                )) << 32)
                                | (u64::from(mask16(
                                    ((lhs_hi >> 48) as u16) >= ((rhs_hi >> 48) as u16),
                                )) << 48),
                        ),
                        V128CmpType::I32x4Eq => (
                            u64::from(mask32(lhs_lo as u32 == rhs_lo as u32))
                                | (u64::from(mask32(
                                    (lhs_lo >> 32) as u32 == (rhs_lo >> 32) as u32,
                                )) << 32),
                            u64::from(mask32(lhs_hi as u32 == rhs_hi as u32))
                                | (u64::from(mask32(
                                    (lhs_hi >> 32) as u32 == (rhs_hi >> 32) as u32,
                                )) << 32),
                        ),
                        V128CmpType::I32x4Ne => (
                            u64::from(mask32(lhs_lo as u32 != rhs_lo as u32))
                                | (u64::from(mask32(
                                    (lhs_lo >> 32) as u32 != (rhs_lo >> 32) as u32,
                                )) << 32),
                            u64::from(mask32(lhs_hi as u32 != rhs_hi as u32))
                                | (u64::from(mask32(
                                    (lhs_hi >> 32) as u32 != (rhs_hi >> 32) as u32,
                                )) << 32),
                        ),
                        V128CmpType::I32x4LtS => (
                            u64::from(mask32((lhs_lo as i32) < (rhs_lo as i32)))
                                | (u64::from(mask32(
                                    ((lhs_lo >> 32) as i32) < ((rhs_lo >> 32) as i32),
                                )) << 32),
                            u64::from(mask32((lhs_hi as i32) < (rhs_hi as i32)))
                                | (u64::from(mask32(
                                    ((lhs_hi >> 32) as i32) < ((rhs_hi >> 32) as i32),
                                )) << 32),
                        ),
                        V128CmpType::I32x4LtU => (
                            u64::from(mask32((lhs_lo as u32) < (rhs_lo as u32)))
                                | (u64::from(mask32(
                                    ((lhs_lo >> 32) as u32) < ((rhs_lo >> 32) as u32),
                                )) << 32),
                            u64::from(mask32((lhs_hi as u32) < (rhs_hi as u32)))
                                | (u64::from(mask32(
                                    ((lhs_hi >> 32) as u32) < ((rhs_hi >> 32) as u32),
                                )) << 32),
                        ),
                        V128CmpType::I32x4GtS => (
                            u64::from(mask32((lhs_lo as i32) > (rhs_lo as i32)))
                                | (u64::from(mask32(
                                    ((lhs_lo >> 32) as i32) > ((rhs_lo >> 32) as i32),
                                )) << 32),
                            u64::from(mask32((lhs_hi as i32) > (rhs_hi as i32)))
                                | (u64::from(mask32(
                                    ((lhs_hi >> 32) as i32) > ((rhs_hi >> 32) as i32),
                                )) << 32),
                        ),
                        V128CmpType::I32x4GtU => (
                            u64::from(mask32((lhs_lo as u32) > (rhs_lo as u32)))
                                | (u64::from(mask32(
                                    ((lhs_lo >> 32) as u32) > ((rhs_lo >> 32) as u32),
                                )) << 32),
                            u64::from(mask32((lhs_hi as u32) > (rhs_hi as u32)))
                                | (u64::from(mask32(
                                    ((lhs_hi >> 32) as u32) > ((rhs_hi >> 32) as u32),
                                )) << 32),
                        ),
                        V128CmpType::I32x4LeS => (
                            u64::from(mask32((lhs_lo as i32) <= (rhs_lo as i32)))
                                | (u64::from(mask32(
                                    ((lhs_lo >> 32) as i32) <= ((rhs_lo >> 32) as i32),
                                )) << 32),
                            u64::from(mask32((lhs_hi as i32) <= (rhs_hi as i32)))
                                | (u64::from(mask32(
                                    ((lhs_hi >> 32) as i32) <= ((rhs_hi >> 32) as i32),
                                )) << 32),
                        ),
                        V128CmpType::I32x4LeU => (
                            u64::from(mask32((lhs_lo as u32) <= (rhs_lo as u32)))
                                | (u64::from(mask32(
                                    ((lhs_lo >> 32) as u32) <= ((rhs_lo >> 32) as u32),
                                )) << 32),
                            u64::from(mask32((lhs_hi as u32) <= (rhs_hi as u32)))
                                | (u64::from(mask32(
                                    ((lhs_hi >> 32) as u32) <= ((rhs_hi >> 32) as u32),
                                )) << 32),
                        ),
                        V128CmpType::I32x4GeS => (
                            u64::from(mask32((lhs_lo as i32) >= (rhs_lo as i32)))
                                | (u64::from(mask32(
                                    ((lhs_lo >> 32) as i32) >= ((rhs_lo >> 32) as i32),
                                )) << 32),
                            u64::from(mask32((lhs_hi as i32) >= (rhs_hi as i32)))
                                | (u64::from(mask32(
                                    ((lhs_hi >> 32) as i32) >= ((rhs_hi >> 32) as i32),
                                )) << 32),
                        ),
                        V128CmpType::I32x4GeU => (
                            u64::from(mask32((lhs_lo as u32) >= (rhs_lo as u32)))
                                | (u64::from(mask32(
                                    ((lhs_lo >> 32) as u32) >= ((rhs_lo >> 32) as u32),
                                )) << 32),
                            u64::from(mask32((lhs_hi as u32) >= (rhs_hi as u32)))
                                | (u64::from(mask32(
                                    ((lhs_hi >> 32) as u32) >= ((rhs_hi >> 32) as u32),
                                )) << 32),
                        ),
                        V128CmpType::I64x2Eq => {
                            (mask64(lhs_lo == rhs_lo), mask64(lhs_hi == rhs_hi))
                        }
                        V128CmpType::I64x2Ne => {
                            (mask64(lhs_lo != rhs_lo), mask64(lhs_hi != rhs_hi))
                        }
                        V128CmpType::I64x2LtS => (
                            mask64((lhs_lo as i64) < (rhs_lo as i64)),
                            mask64((lhs_hi as i64) < (rhs_hi as i64)),
                        ),
                        V128CmpType::I64x2GtS => (
                            mask64((lhs_lo as i64) > (rhs_lo as i64)),
                            mask64((lhs_hi as i64) > (rhs_hi as i64)),
                        ),
                        V128CmpType::I64x2LeS => (
                            mask64((lhs_lo as i64) <= (rhs_lo as i64)),
                            mask64((lhs_hi as i64) <= (rhs_hi as i64)),
                        ),
                        V128CmpType::I64x2GeS => (
                            mask64((lhs_lo as i64) >= (rhs_lo as i64)),
                            mask64((lhs_hi as i64) >= (rhs_hi as i64)),
                        ),
                        V128CmpType::F32x4Eq => (
                            u64::from(mask32(
                                f32::from_bits(lhs_lo as u32) == f32::from_bits(rhs_lo as u32),
                            )) | (u64::from(mask32(
                                f32::from_bits((lhs_lo >> 32) as u32)
                                    == f32::from_bits((rhs_lo >> 32) as u32),
                            )) << 32),
                            u64::from(mask32(
                                f32::from_bits(lhs_hi as u32) == f32::from_bits(rhs_hi as u32),
                            )) | (u64::from(mask32(
                                f32::from_bits((lhs_hi >> 32) as u32)
                                    == f32::from_bits((rhs_hi >> 32) as u32),
                            )) << 32),
                        ),
                        V128CmpType::F32x4Ne => (
                            u64::from(mask32(
                                f32::from_bits(lhs_lo as u32) != f32::from_bits(rhs_lo as u32),
                            )) | (u64::from(mask32(
                                f32::from_bits((lhs_lo >> 32) as u32)
                                    != f32::from_bits((rhs_lo >> 32) as u32),
                            )) << 32),
                            u64::from(mask32(
                                f32::from_bits(lhs_hi as u32) != f32::from_bits(rhs_hi as u32),
                            )) | (u64::from(mask32(
                                f32::from_bits((lhs_hi >> 32) as u32)
                                    != f32::from_bits((rhs_hi >> 32) as u32),
                            )) << 32),
                        ),
                        V128CmpType::F32x4Lt => (
                            u64::from(mask32(
                                f32::from_bits(lhs_lo as u32) < f32::from_bits(rhs_lo as u32),
                            )) | (u64::from(mask32(
                                f32::from_bits((lhs_lo >> 32) as u32)
                                    < f32::from_bits((rhs_lo >> 32) as u32),
                            )) << 32),
                            u64::from(mask32(
                                f32::from_bits(lhs_hi as u32) < f32::from_bits(rhs_hi as u32),
                            )) | (u64::from(mask32(
                                f32::from_bits((lhs_hi >> 32) as u32)
                                    < f32::from_bits((rhs_hi >> 32) as u32),
                            )) << 32),
                        ),
                        V128CmpType::F32x4Gt => (
                            u64::from(mask32(
                                f32::from_bits(lhs_lo as u32) > f32::from_bits(rhs_lo as u32),
                            )) | (u64::from(mask32(
                                f32::from_bits((lhs_lo >> 32) as u32)
                                    > f32::from_bits((rhs_lo >> 32) as u32),
                            )) << 32),
                            u64::from(mask32(
                                f32::from_bits(lhs_hi as u32) > f32::from_bits(rhs_hi as u32),
                            )) | (u64::from(mask32(
                                f32::from_bits((lhs_hi >> 32) as u32)
                                    > f32::from_bits((rhs_hi >> 32) as u32),
                            )) << 32),
                        ),
                        V128CmpType::F32x4Le => (
                            u64::from(mask32(
                                f32::from_bits(lhs_lo as u32) <= f32::from_bits(rhs_lo as u32),
                            )) | (u64::from(mask32(
                                f32::from_bits((lhs_lo >> 32) as u32)
                                    <= f32::from_bits((rhs_lo >> 32) as u32),
                            )) << 32),
                            u64::from(mask32(
                                f32::from_bits(lhs_hi as u32) <= f32::from_bits(rhs_hi as u32),
                            )) | (u64::from(mask32(
                                f32::from_bits((lhs_hi >> 32) as u32)
                                    <= f32::from_bits((rhs_hi >> 32) as u32),
                            )) << 32),
                        ),
                        V128CmpType::F32x4Ge => (
                            u64::from(mask32(
                                f32::from_bits(lhs_lo as u32) >= f32::from_bits(rhs_lo as u32),
                            )) | (u64::from(mask32(
                                f32::from_bits((lhs_lo >> 32) as u32)
                                    >= f32::from_bits((rhs_lo >> 32) as u32),
                            )) << 32),
                            u64::from(mask32(
                                f32::from_bits(lhs_hi as u32) >= f32::from_bits(rhs_hi as u32),
                            )) | (u64::from(mask32(
                                f32::from_bits((lhs_hi >> 32) as u32)
                                    >= f32::from_bits((rhs_hi >> 32) as u32),
                            )) << 32),
                        ),
                        V128CmpType::F64x2Eq => (
                            mask64(f64::from_bits(lhs_lo) == f64::from_bits(rhs_lo)),
                            mask64(f64::from_bits(lhs_hi) == f64::from_bits(rhs_hi)),
                        ),
                        V128CmpType::F64x2Ne => (
                            mask64(f64::from_bits(lhs_lo) != f64::from_bits(rhs_lo)),
                            mask64(f64::from_bits(lhs_hi) != f64::from_bits(rhs_hi)),
                        ),
                        V128CmpType::F64x2Lt => (
                            mask64(f64::from_bits(lhs_lo) < f64::from_bits(rhs_lo)),
                            mask64(f64::from_bits(lhs_hi) < f64::from_bits(rhs_hi)),
                        ),
                        V128CmpType::F64x2Gt => (
                            mask64(f64::from_bits(lhs_lo) > f64::from_bits(rhs_lo)),
                            mask64(f64::from_bits(lhs_hi) > f64::from_bits(rhs_hi)),
                        ),
                        V128CmpType::F64x2Le => (
                            mask64(f64::from_bits(lhs_lo) <= f64::from_bits(rhs_lo)),
                            mask64(f64::from_bits(lhs_hi) <= f64::from_bits(rhs_hi)),
                        ),
                        V128CmpType::F64x2Ge => (
                            mask64(f64::from_bits(lhs_lo) >= f64::from_bits(rhs_lo)),
                            mask64(f64::from_bits(lhs_hi) >= f64::from_bits(rhs_hi)),
                        ),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128ExtAddPairwise => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let signed = op.b3;
                    let (ret_lo, ret_hi) = match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            let mut ret_lo = 0_u64;
                            let mut ret_hi = 0_u64;
                            for i in 0..8 {
                                let (v1, v2) = if i < 4 {
                                    (byte_at(lo, i * 2), byte_at(lo, i * 2 + 1))
                                } else {
                                    (byte_at(hi, (i - 4) * 2), byte_at(hi, (i - 4) * 2 + 1))
                                };
                                let lane = if signed {
                                    (i16::from(v1 as i8) + i16::from(v2 as i8)) as u16
                                } else {
                                    u16::from(v1) + u16::from(v2)
                                };
                                if i < 4 {
                                    ret_lo |= u64::from(lane) << (i * 16);
                                } else {
                                    ret_hi |= u64::from(lane) << ((i - 4) * 16);
                                }
                            }
                            (ret_lo, ret_hi)
                        }
                        x if x == Shape::I16x8 as u8 => {
                            let mut ret_lo = 0_u64;
                            let mut ret_hi = 0_u64;
                            for i in 0..4 {
                                let (v1, v2) = if i < 2 {
                                    (half_at(lo, i * 2), half_at(lo, i * 2 + 1))
                                } else {
                                    (half_at(hi, (i - 2) * 2), half_at(hi, (i - 2) * 2 + 1))
                                };
                                let lane = if signed {
                                    (i32::from(v1 as i16) + i32::from(v2 as i16)) as u32
                                } else {
                                    u32::from(v1) + u32::from(v2)
                                };
                                if i < 2 {
                                    ret_lo |= u64::from(lane) << (i * 32);
                                } else {
                                    ret_hi |= u64::from(lane) << ((i - 2) * 32);
                                }
                            }
                            (ret_lo, ret_hi)
                        }
                        _ => return Err(Trap::new("unsupported v128 extadd pairwise shape")),
                    };
                    self.push_value(ret_lo);
                    self.push_value(ret_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128ExtMul => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let (lhs, rhs) = if op.b3 {
                        (lhs_lo, rhs_lo)
                    } else {
                        (lhs_hi, rhs_hi)
                    };
                    let signed = op.b2 == 1;
                    let (ret_lo, ret_hi) = match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            let mut ret_lo = 0_u64;
                            let mut ret_hi = 0_u64;
                            for i in 0..8 {
                                let v1 = byte_at(lhs, i);
                                let v2 = byte_at(rhs, i);
                                let lane = if signed {
                                    (i16::from(v1 as i8) * i16::from(v2 as i8)) as u16
                                } else {
                                    u16::from(v1) * u16::from(v2)
                                };
                                if i < 4 {
                                    ret_lo |= u64::from(lane) << (i * 16);
                                } else {
                                    ret_hi |= u64::from(lane) << ((i - 4) * 16);
                                }
                            }
                            (ret_lo, ret_hi)
                        }
                        x if x == Shape::I16x8 as u8 => {
                            let mut ret_lo = 0_u64;
                            let mut ret_hi = 0_u64;
                            for i in 0..4 {
                                let v1 = half_at(lhs, i);
                                let v2 = half_at(rhs, i);
                                let lane = if signed {
                                    (i32::from(v1 as i16) * i32::from(v2 as i16)) as u32
                                } else {
                                    u32::from(v1) * u32::from(v2)
                                };
                                if i < 2 {
                                    ret_lo |= u64::from(lane) << (i * 32);
                                } else {
                                    ret_hi |= u64::from(lane) << ((i - 2) * 32);
                                }
                            }
                            (ret_lo, ret_hi)
                        }
                        x if x == Shape::I32x4 as u8 => {
                            let lhs_lo32 = lhs as u32;
                            let rhs_lo32 = rhs as u32;
                            let lhs_hi32 = (lhs >> 32) as u32;
                            let rhs_hi32 = (rhs >> 32) as u32;
                            if signed {
                                (
                                    (i64::from(lhs_lo32 as i32) * i64::from(rhs_lo32 as i32))
                                        as u64,
                                    (i64::from(lhs_hi32 as i32) * i64::from(rhs_hi32 as i32))
                                        as u64,
                                )
                            } else {
                                (
                                    u64::from(lhs_lo32) * u64::from(rhs_lo32),
                                    u64::from(lhs_hi32) * u64::from(rhs_hi32),
                                )
                            }
                        }
                        _ => return Err(Trap::new("unsupported v128 extmul shape")),
                    };
                    self.push_value(ret_lo);
                    self.push_value(ret_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Q15mulrSatS => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let mut ret_lo = 0_u64;
                    let mut ret_hi = 0_u64;
                    for i in 0..8 {
                        let (lhs, rhs) = if i < 4 {
                            (half_at(lhs_lo, i) as i16, half_at(rhs_lo, i) as i16)
                        } else {
                            (half_at(lhs_hi, i - 4) as i16, half_at(rhs_hi, i - 4) as i16)
                        };
                        let calc = ((i32::from(lhs) * i32::from(rhs)) + 0x4000) >> 15;
                        let lane = if calc < i32::from(i16::MIN) {
                            0x8000_u64
                        } else if calc > i32::from(i16::MAX) {
                            0x7fff_u64
                        } else {
                            u64::from((calc as i16) as u16)
                        };
                        if i < 4 {
                            ret_lo |= lane << (i * 16);
                        } else {
                            ret_hi |= lane << ((i - 4) * 16);
                        }
                    }
                    self.push_value(ret_lo);
                    self.push_value(ret_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128AddSat => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let signed = op.b3;
                    let (ret_lo, ret_hi) = match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            let mut ret_lo = 0_u64;
                            let mut ret_hi = 0_u64;
                            for i in 0..16 {
                                let (lhs, rhs) = if i < 8 {
                                    (byte_at(lhs_lo, i), byte_at(rhs_lo, i))
                                } else {
                                    (byte_at(lhs_hi, i - 8), byte_at(rhs_hi, i - 8))
                                };
                                let lane = if signed {
                                    let added = i16::from(lhs as i8) + i16::from(rhs as i8);
                                    if added < i16::from(i8::MIN) {
                                        0x80_u64
                                    } else if added > i16::from(i8::MAX) {
                                        0x7f_u64
                                    } else {
                                        u64::from((added as i8) as u8)
                                    }
                                } else {
                                    u64::from(lhs.saturating_add(rhs))
                                };
                                if i < 8 {
                                    ret_lo |= lane << (i * 8);
                                } else {
                                    ret_hi |= lane << ((i - 8) * 8);
                                }
                            }
                            (ret_lo, ret_hi)
                        }
                        x if x == Shape::I16x8 as u8 => {
                            let mut ret_lo = 0_u64;
                            let mut ret_hi = 0_u64;
                            for i in 0..8 {
                                let (lhs, rhs) = if i < 4 {
                                    (half_at(lhs_lo, i), half_at(rhs_lo, i))
                                } else {
                                    (half_at(lhs_hi, i - 4), half_at(rhs_hi, i - 4))
                                };
                                let lane = if signed {
                                    let added = i32::from(lhs as i16) + i32::from(rhs as i16);
                                    if added < i32::from(i16::MIN) {
                                        0x8000_u64
                                    } else if added > i32::from(i16::MAX) {
                                        0x7fff_u64
                                    } else {
                                        u64::from((added as i16) as u16)
                                    }
                                } else {
                                    u64::from(lhs.saturating_add(rhs))
                                };
                                if i < 4 {
                                    ret_lo |= lane << (i * 16);
                                } else {
                                    ret_hi |= lane << ((i - 4) * 16);
                                }
                            }
                            (ret_lo, ret_hi)
                        }
                        _ => return Err(Trap::new("unsupported v128 add_sat shape")),
                    };
                    self.push_value(ret_lo);
                    self.push_value(ret_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128SubSat => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let signed = op.b3;
                    let (ret_lo, ret_hi) = match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            let mut ret_lo = 0_u64;
                            let mut ret_hi = 0_u64;
                            for i in 0..16 {
                                let (lhs, rhs) = if i < 8 {
                                    (byte_at(lhs_lo, i), byte_at(rhs_lo, i))
                                } else {
                                    (byte_at(lhs_hi, i - 8), byte_at(rhs_hi, i - 8))
                                };
                                let lane = if signed {
                                    let subbed = i16::from(lhs as i8) - i16::from(rhs as i8);
                                    if subbed < i16::from(i8::MIN) {
                                        0x80_u64
                                    } else if subbed > i16::from(i8::MAX) {
                                        0x7f_u64
                                    } else {
                                        u64::from((subbed as i8) as u8)
                                    }
                                } else {
                                    u64::from(lhs.saturating_sub(rhs))
                                };
                                if i < 8 {
                                    ret_lo |= lane << (i * 8);
                                } else {
                                    ret_hi |= lane << ((i - 8) * 8);
                                }
                            }
                            (ret_lo, ret_hi)
                        }
                        x if x == Shape::I16x8 as u8 => {
                            let mut ret_lo = 0_u64;
                            let mut ret_hi = 0_u64;
                            for i in 0..8 {
                                let (lhs, rhs) = if i < 4 {
                                    (half_at(lhs_lo, i), half_at(rhs_lo, i))
                                } else {
                                    (half_at(lhs_hi, i - 4), half_at(rhs_hi, i - 4))
                                };
                                let lane = if signed {
                                    let subbed = i32::from(lhs as i16) - i32::from(rhs as i16);
                                    if subbed < i32::from(i16::MIN) {
                                        0x8000_u64
                                    } else if subbed > i32::from(i16::MAX) {
                                        0x7fff_u64
                                    } else {
                                        u64::from((subbed as i16) as u16)
                                    }
                                } else {
                                    u64::from(lhs.saturating_sub(rhs))
                                };
                                if i < 4 {
                                    ret_lo |= lane << (i * 16);
                                } else {
                                    ret_hi |= lane << ((i - 4) * 16);
                                }
                            }
                            (ret_lo, ret_hi)
                        }
                        _ => return Err(Trap::new("unsupported v128 sub_sat shape")),
                    };
                    self.push_value(ret_lo);
                    self.push_value(ret_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Dot => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let r1 =
                        i32::from(half_at(lhs_lo, 0) as i16) * i32::from(half_at(rhs_lo, 0) as i16);
                    let r2 =
                        i32::from(half_at(lhs_lo, 1) as i16) * i32::from(half_at(rhs_lo, 1) as i16);
                    let r3 =
                        i32::from(half_at(lhs_lo, 2) as i16) * i32::from(half_at(rhs_lo, 2) as i16);
                    let r4 =
                        i32::from(half_at(lhs_lo, 3) as i16) * i32::from(half_at(rhs_lo, 3) as i16);
                    let r5 =
                        i32::from(half_at(lhs_hi, 0) as i16) * i32::from(half_at(rhs_hi, 0) as i16);
                    let r6 =
                        i32::from(half_at(lhs_hi, 1) as i16) * i32::from(half_at(rhs_hi, 1) as i16);
                    let r7 =
                        i32::from(half_at(lhs_hi, 2) as i16) * i32::from(half_at(rhs_hi, 2) as i16);
                    let r8 =
                        i32::from(half_at(lhs_hi, 3) as i16) * i32::from(half_at(rhs_hi, 3) as i16);
                    let lo = u64::from(r1.wrapping_add(r2) as u32)
                        | (u64::from(r3.wrapping_add(r4) as u32) << 32);
                    let hi = u64::from(r5.wrapping_add(r6) as u32)
                        | (u64::from(r7.wrapping_add(r8) as u32) << 32);
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Add => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::I8x16 as u8 => (
                            u64::from((lhs_lo as u8).wrapping_add(rhs_lo as u8))
                                | (u64::from(
                                    ((lhs_lo >> 8) as u8).wrapping_add((rhs_lo >> 8) as u8),
                                ) << 8)
                                | (u64::from(
                                    ((lhs_lo >> 16) as u8).wrapping_add((rhs_lo >> 16) as u8),
                                ) << 16)
                                | (u64::from(
                                    ((lhs_lo >> 24) as u8).wrapping_add((rhs_lo >> 24) as u8),
                                ) << 24)
                                | (u64::from(
                                    ((lhs_lo >> 32) as u8).wrapping_add((rhs_lo >> 32) as u8),
                                ) << 32)
                                | (u64::from(
                                    ((lhs_lo >> 40) as u8).wrapping_add((rhs_lo >> 40) as u8),
                                ) << 40)
                                | (u64::from(
                                    ((lhs_lo >> 48) as u8).wrapping_add((rhs_lo >> 48) as u8),
                                ) << 48)
                                | (u64::from(
                                    ((lhs_lo >> 56) as u8).wrapping_add((rhs_lo >> 56) as u8),
                                ) << 56),
                            u64::from((lhs_hi as u8).wrapping_add(rhs_hi as u8))
                                | (u64::from(
                                    ((lhs_hi >> 8) as u8).wrapping_add((rhs_hi >> 8) as u8),
                                ) << 8)
                                | (u64::from(
                                    ((lhs_hi >> 16) as u8).wrapping_add((rhs_hi >> 16) as u8),
                                ) << 16)
                                | (u64::from(
                                    ((lhs_hi >> 24) as u8).wrapping_add((rhs_hi >> 24) as u8),
                                ) << 24)
                                | (u64::from(
                                    ((lhs_hi >> 32) as u8).wrapping_add((rhs_hi >> 32) as u8),
                                ) << 32)
                                | (u64::from(
                                    ((lhs_hi >> 40) as u8).wrapping_add((rhs_hi >> 40) as u8),
                                ) << 40)
                                | (u64::from(
                                    ((lhs_hi >> 48) as u8).wrapping_add((rhs_hi >> 48) as u8),
                                ) << 48)
                                | (u64::from(
                                    ((lhs_hi >> 56) as u8).wrapping_add((rhs_hi >> 56) as u8),
                                ) << 56),
                        ),
                        x if x == Shape::I16x8 as u8 => (
                            u64::from((lhs_lo as u16).wrapping_add(rhs_lo as u16))
                                | (u64::from(
                                    ((lhs_lo >> 16) as u16).wrapping_add((rhs_lo >> 16) as u16),
                                ) << 16)
                                | (u64::from(
                                    ((lhs_lo >> 32) as u16).wrapping_add((rhs_lo >> 32) as u16),
                                ) << 32)
                                | (u64::from(
                                    ((lhs_lo >> 48) as u16).wrapping_add((rhs_lo >> 48) as u16),
                                ) << 48),
                            u64::from((lhs_hi as u16).wrapping_add(rhs_hi as u16))
                                | (u64::from(
                                    ((lhs_hi >> 16) as u16).wrapping_add((rhs_hi >> 16) as u16),
                                ) << 16)
                                | (u64::from(
                                    ((lhs_hi >> 32) as u16).wrapping_add((rhs_hi >> 32) as u16),
                                ) << 32)
                                | (u64::from(
                                    ((lhs_hi >> 48) as u16).wrapping_add((rhs_hi >> 48) as u16),
                                ) << 48),
                        ),
                        x if x == Shape::I32x4 as u8 => (
                            u64::from((lhs_lo as u32).wrapping_add(rhs_lo as u32))
                                | (u64::from(
                                    ((lhs_lo >> 32) as u32).wrapping_add((rhs_lo >> 32) as u32),
                                ) << 32),
                            u64::from((lhs_hi as u32).wrapping_add(rhs_hi as u32))
                                | (u64::from(
                                    ((lhs_hi >> 32) as u32).wrapping_add((rhs_hi >> 32) as u32),
                                ) << 32),
                        ),
                        x if x == Shape::I64x2 as u8 => {
                            (lhs_lo.wrapping_add(rhs_lo), lhs_hi.wrapping_add(rhs_hi))
                        }
                        x if x == Shape::F32x4 as u8 => (
                            u64::from(
                                (f32::from_bits(lhs_lo as u32) + f32::from_bits(rhs_lo as u32))
                                    .to_bits(),
                            ) | (u64::from(
                                (f32::from_bits((lhs_lo >> 32) as u32)
                                    + f32::from_bits((rhs_lo >> 32) as u32))
                                .to_bits(),
                            ) << 32),
                            u64::from(
                                (f32::from_bits(lhs_hi as u32) + f32::from_bits(rhs_hi as u32))
                                    .to_bits(),
                            ) | (u64::from(
                                (f32::from_bits((lhs_hi >> 32) as u32)
                                    + f32::from_bits((rhs_hi >> 32) as u32))
                                .to_bits(),
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            (f64::from_bits(lhs_lo) + f64::from_bits(rhs_lo)).to_bits(),
                            (f64::from_bits(lhs_hi) + f64::from_bits(rhs_hi)).to_bits(),
                        ),
                        _ => return Err(Trap::new("unsupported v128 add shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Sub => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::I8x16 as u8 => (
                            u64::from((lhs_lo as u8).wrapping_sub(rhs_lo as u8))
                                | (u64::from(
                                    ((lhs_lo >> 8) as u8).wrapping_sub((rhs_lo >> 8) as u8),
                                ) << 8)
                                | (u64::from(
                                    ((lhs_lo >> 16) as u8).wrapping_sub((rhs_lo >> 16) as u8),
                                ) << 16)
                                | (u64::from(
                                    ((lhs_lo >> 24) as u8).wrapping_sub((rhs_lo >> 24) as u8),
                                ) << 24)
                                | (u64::from(
                                    ((lhs_lo >> 32) as u8).wrapping_sub((rhs_lo >> 32) as u8),
                                ) << 32)
                                | (u64::from(
                                    ((lhs_lo >> 40) as u8).wrapping_sub((rhs_lo >> 40) as u8),
                                ) << 40)
                                | (u64::from(
                                    ((lhs_lo >> 48) as u8).wrapping_sub((rhs_lo >> 48) as u8),
                                ) << 48)
                                | (u64::from(
                                    ((lhs_lo >> 56) as u8).wrapping_sub((rhs_lo >> 56) as u8),
                                ) << 56),
                            u64::from((lhs_hi as u8).wrapping_sub(rhs_hi as u8))
                                | (u64::from(
                                    ((lhs_hi >> 8) as u8).wrapping_sub((rhs_hi >> 8) as u8),
                                ) << 8)
                                | (u64::from(
                                    ((lhs_hi >> 16) as u8).wrapping_sub((rhs_hi >> 16) as u8),
                                ) << 16)
                                | (u64::from(
                                    ((lhs_hi >> 24) as u8).wrapping_sub((rhs_hi >> 24) as u8),
                                ) << 24)
                                | (u64::from(
                                    ((lhs_hi >> 32) as u8).wrapping_sub((rhs_hi >> 32) as u8),
                                ) << 32)
                                | (u64::from(
                                    ((lhs_hi >> 40) as u8).wrapping_sub((rhs_hi >> 40) as u8),
                                ) << 40)
                                | (u64::from(
                                    ((lhs_hi >> 48) as u8).wrapping_sub((rhs_hi >> 48) as u8),
                                ) << 48)
                                | (u64::from(
                                    ((lhs_hi >> 56) as u8).wrapping_sub((rhs_hi >> 56) as u8),
                                ) << 56),
                        ),
                        x if x == Shape::I16x8 as u8 => (
                            u64::from((lhs_lo as u16).wrapping_sub(rhs_lo as u16))
                                | (u64::from(
                                    ((lhs_lo >> 16) as u16).wrapping_sub((rhs_lo >> 16) as u16),
                                ) << 16)
                                | (u64::from(
                                    ((lhs_lo >> 32) as u16).wrapping_sub((rhs_lo >> 32) as u16),
                                ) << 32)
                                | (u64::from(
                                    ((lhs_lo >> 48) as u16).wrapping_sub((rhs_lo >> 48) as u16),
                                ) << 48),
                            u64::from((lhs_hi as u16).wrapping_sub(rhs_hi as u16))
                                | (u64::from(
                                    ((lhs_hi >> 16) as u16).wrapping_sub((rhs_hi >> 16) as u16),
                                ) << 16)
                                | (u64::from(
                                    ((lhs_hi >> 32) as u16).wrapping_sub((rhs_hi >> 32) as u16),
                                ) << 32)
                                | (u64::from(
                                    ((lhs_hi >> 48) as u16).wrapping_sub((rhs_hi >> 48) as u16),
                                ) << 48),
                        ),
                        x if x == Shape::I32x4 as u8 => (
                            u64::from((lhs_lo as u32).wrapping_sub(rhs_lo as u32))
                                | (u64::from(
                                    ((lhs_lo >> 32) as u32).wrapping_sub((rhs_lo >> 32) as u32),
                                ) << 32),
                            u64::from((lhs_hi as u32).wrapping_sub(rhs_hi as u32))
                                | (u64::from(
                                    ((lhs_hi >> 32) as u32).wrapping_sub((rhs_hi >> 32) as u32),
                                ) << 32),
                        ),
                        x if x == Shape::I64x2 as u8 => {
                            (lhs_lo.wrapping_sub(rhs_lo), lhs_hi.wrapping_sub(rhs_hi))
                        }
                        x if x == Shape::F32x4 as u8 => (
                            u64::from(
                                (f32::from_bits(lhs_lo as u32) - f32::from_bits(rhs_lo as u32))
                                    .to_bits(),
                            ) | (u64::from(
                                (f32::from_bits((lhs_lo >> 32) as u32)
                                    - f32::from_bits((rhs_lo >> 32) as u32))
                                .to_bits(),
                            ) << 32),
                            u64::from(
                                (f32::from_bits(lhs_hi as u32) - f32::from_bits(rhs_hi as u32))
                                    .to_bits(),
                            ) | (u64::from(
                                (f32::from_bits((lhs_hi >> 32) as u32)
                                    - f32::from_bits((rhs_hi >> 32) as u32))
                                .to_bits(),
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            (f64::from_bits(lhs_lo) - f64::from_bits(rhs_lo)).to_bits(),
                            (f64::from_bits(lhs_hi) - f64::from_bits(rhs_hi)).to_bits(),
                        ),
                        _ => return Err(Trap::new("unsupported v128 sub shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Mul => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::I16x8 as u8 => (
                            u64::from((lhs_lo as u16).wrapping_mul(rhs_lo as u16))
                                | (u64::from(
                                    ((lhs_lo >> 16) as u16).wrapping_mul((rhs_lo >> 16) as u16),
                                ) << 16)
                                | (u64::from(
                                    ((lhs_lo >> 32) as u16).wrapping_mul((rhs_lo >> 32) as u16),
                                ) << 32)
                                | (u64::from(
                                    ((lhs_lo >> 48) as u16).wrapping_mul((rhs_lo >> 48) as u16),
                                ) << 48),
                            u64::from((lhs_hi as u16).wrapping_mul(rhs_hi as u16))
                                | (u64::from(
                                    ((lhs_hi >> 16) as u16).wrapping_mul((rhs_hi >> 16) as u16),
                                ) << 16)
                                | (u64::from(
                                    ((lhs_hi >> 32) as u16).wrapping_mul((rhs_hi >> 32) as u16),
                                ) << 32)
                                | (u64::from(
                                    ((lhs_hi >> 48) as u16).wrapping_mul((rhs_hi >> 48) as u16),
                                ) << 48),
                        ),
                        x if x == Shape::I32x4 as u8 => (
                            u64::from((lhs_lo as u32).wrapping_mul(rhs_lo as u32))
                                | (u64::from(
                                    ((lhs_lo >> 32) as u32).wrapping_mul((rhs_lo >> 32) as u32),
                                ) << 32),
                            u64::from((lhs_hi as u32).wrapping_mul(rhs_hi as u32))
                                | (u64::from(
                                    ((lhs_hi >> 32) as u32).wrapping_mul((rhs_hi >> 32) as u32),
                                ) << 32),
                        ),
                        x if x == Shape::I64x2 as u8 => {
                            (lhs_lo.wrapping_mul(rhs_lo), lhs_hi.wrapping_mul(rhs_hi))
                        }
                        x if x == Shape::F32x4 as u8 => (
                            u64::from(
                                (f32::from_bits(lhs_lo as u32) * f32::from_bits(rhs_lo as u32))
                                    .to_bits(),
                            ) | (u64::from(
                                (f32::from_bits((lhs_lo >> 32) as u32)
                                    * f32::from_bits((rhs_lo >> 32) as u32))
                                .to_bits(),
                            ) << 32),
                            u64::from(
                                (f32::from_bits(lhs_hi as u32) * f32::from_bits(rhs_hi as u32))
                                    .to_bits(),
                            ) | (u64::from(
                                (f32::from_bits((lhs_hi >> 32) as u32)
                                    * f32::from_bits((rhs_hi >> 32) as u32))
                                .to_bits(),
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            (f64::from_bits(lhs_lo) * f64::from_bits(rhs_lo)).to_bits(),
                            (f64::from_bits(lhs_hi) * f64::from_bits(rhs_hi)).to_bits(),
                        ),
                        _ => return Err(Trap::new("unsupported v128 mul shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Div => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::F32x4 as u8 => (
                            u64::from(
                                (f32::from_bits(lhs_lo as u32) / f32::from_bits(rhs_lo as u32))
                                    .to_bits(),
                            ) | (u64::from(
                                (f32::from_bits((lhs_lo >> 32) as u32)
                                    / f32::from_bits((rhs_lo >> 32) as u32))
                                .to_bits(),
                            ) << 32),
                            u64::from(
                                (f32::from_bits(lhs_hi as u32) / f32::from_bits(rhs_hi as u32))
                                    .to_bits(),
                            ) | (u64::from(
                                (f32::from_bits((lhs_hi >> 32) as u32)
                                    / f32::from_bits((rhs_hi >> 32) as u32))
                                .to_bits(),
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            (f64::from_bits(lhs_lo) / f64::from_bits(rhs_lo)).to_bits(),
                            (f64::from_bits(lhs_hi) / f64::from_bits(rhs_hi)).to_bits(),
                        ),
                        _ => return Err(Trap::new("unsupported v128 div shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Neg => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::I8x16 as u8 => (
                            u64::from((0u8).wrapping_sub(lo as u8))
                                | (u64::from((0u8).wrapping_sub((lo >> 8) as u8)) << 8)
                                | (u64::from((0u8).wrapping_sub((lo >> 16) as u8)) << 16)
                                | (u64::from((0u8).wrapping_sub((lo >> 24) as u8)) << 24)
                                | (u64::from((0u8).wrapping_sub((lo >> 32) as u8)) << 32)
                                | (u64::from((0u8).wrapping_sub((lo >> 40) as u8)) << 40)
                                | (u64::from((0u8).wrapping_sub((lo >> 48) as u8)) << 48)
                                | (u64::from((0u8).wrapping_sub((lo >> 56) as u8)) << 56),
                            u64::from((0u8).wrapping_sub(hi as u8))
                                | (u64::from((0u8).wrapping_sub((hi >> 8) as u8)) << 8)
                                | (u64::from((0u8).wrapping_sub((hi >> 16) as u8)) << 16)
                                | (u64::from((0u8).wrapping_sub((hi >> 24) as u8)) << 24)
                                | (u64::from((0u8).wrapping_sub((hi >> 32) as u8)) << 32)
                                | (u64::from((0u8).wrapping_sub((hi >> 40) as u8)) << 40)
                                | (u64::from((0u8).wrapping_sub((hi >> 48) as u8)) << 48)
                                | (u64::from((0u8).wrapping_sub((hi >> 56) as u8)) << 56),
                        ),
                        x if x == Shape::I16x8 as u8 => (
                            u64::from((0u16).wrapping_sub(lo as u16))
                                | (u64::from((0u16).wrapping_sub((lo >> 16) as u16)) << 16)
                                | (u64::from((0u16).wrapping_sub((lo >> 32) as u16)) << 32)
                                | (u64::from((0u16).wrapping_sub((lo >> 48) as u16)) << 48),
                            u64::from((0u16).wrapping_sub(hi as u16))
                                | (u64::from((0u16).wrapping_sub((hi >> 16) as u16)) << 16)
                                | (u64::from((0u16).wrapping_sub((hi >> 32) as u16)) << 32)
                                | (u64::from((0u16).wrapping_sub((hi >> 48) as u16)) << 48),
                        ),
                        x if x == Shape::I32x4 as u8 => (
                            u64::from((0u32).wrapping_sub(lo as u32))
                                | (u64::from((0u32).wrapping_sub((lo >> 32) as u32)) << 32),
                            u64::from((0u32).wrapping_sub(hi as u32))
                                | (u64::from((0u32).wrapping_sub((hi >> 32) as u32)) << 32),
                        ),
                        x if x == Shape::I64x2 as u8 => {
                            ((0u64).wrapping_sub(lo), (0u64).wrapping_sub(hi))
                        }
                        x if x == Shape::F32x4 as u8 => (
                            execute_unary_float(
                                OperationKind::Neg,
                                FloatKind::F32,
                                lo as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Neg,
                                FloatKind::F32,
                                (lo >> 32) as u32 as u64,
                            ) << 32),
                            execute_unary_float(
                                OperationKind::Neg,
                                FloatKind::F32,
                                hi as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Neg,
                                FloatKind::F32,
                                (hi >> 32) as u32 as u64,
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            execute_unary_float(OperationKind::Neg, FloatKind::F64, lo),
                            execute_unary_float(OperationKind::Neg, FloatKind::F64, hi),
                        ),
                        _ => return Err(Trap::new("unsupported v128 neg shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Sqrt => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::F32x4 as u8 => (
                            execute_unary_float(
                                OperationKind::Sqrt,
                                FloatKind::F32,
                                lo as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Sqrt,
                                FloatKind::F32,
                                (lo >> 32) as u32 as u64,
                            ) << 32),
                            execute_unary_float(
                                OperationKind::Sqrt,
                                FloatKind::F32,
                                hi as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Sqrt,
                                FloatKind::F32,
                                (hi >> 32) as u32 as u64,
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            execute_unary_float(OperationKind::Sqrt, FloatKind::F64, lo),
                            execute_unary_float(OperationKind::Sqrt, FloatKind::F64, hi),
                        ),
                        _ => return Err(Trap::new("unsupported v128 sqrt shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Abs => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::I8x16 as u8 => (
                            u64::from((lo as i8).unsigned_abs())
                                | (u64::from(((lo >> 8) as i8).unsigned_abs()) << 8)
                                | (u64::from(((lo >> 16) as i8).unsigned_abs()) << 16)
                                | (u64::from(((lo >> 24) as i8).unsigned_abs()) << 24)
                                | (u64::from(((lo >> 32) as i8).unsigned_abs()) << 32)
                                | (u64::from(((lo >> 40) as i8).unsigned_abs()) << 40)
                                | (u64::from(((lo >> 48) as i8).unsigned_abs()) << 48)
                                | (u64::from(((lo >> 56) as i8).unsigned_abs()) << 56),
                            u64::from((hi as i8).unsigned_abs())
                                | (u64::from(((hi >> 8) as i8).unsigned_abs()) << 8)
                                | (u64::from(((hi >> 16) as i8).unsigned_abs()) << 16)
                                | (u64::from(((hi >> 24) as i8).unsigned_abs()) << 24)
                                | (u64::from(((hi >> 32) as i8).unsigned_abs()) << 32)
                                | (u64::from(((hi >> 40) as i8).unsigned_abs()) << 40)
                                | (u64::from(((hi >> 48) as i8).unsigned_abs()) << 48)
                                | (u64::from(((hi >> 56) as i8).unsigned_abs()) << 56),
                        ),
                        x if x == Shape::I16x8 as u8 => (
                            u64::from((lo as i16).unsigned_abs())
                                | (u64::from(((lo >> 16) as i16).unsigned_abs()) << 16)
                                | (u64::from(((lo >> 32) as i16).unsigned_abs()) << 32)
                                | (u64::from(((lo >> 48) as i16).unsigned_abs()) << 48),
                            u64::from((hi as i16).unsigned_abs())
                                | (u64::from(((hi >> 16) as i16).unsigned_abs()) << 16)
                                | (u64::from(((hi >> 32) as i16).unsigned_abs()) << 32)
                                | (u64::from(((hi >> 48) as i16).unsigned_abs()) << 48),
                        ),
                        x if x == Shape::I32x4 as u8 => (
                            u64::from((lo as i32).unsigned_abs())
                                | (u64::from(((lo >> 32) as i32).unsigned_abs()) << 32),
                            u64::from((hi as i32).unsigned_abs())
                                | (u64::from(((hi >> 32) as i32).unsigned_abs()) << 32),
                        ),
                        x if x == Shape::I64x2 as u8 => {
                            ((lo as i64).unsigned_abs(), (hi as i64).unsigned_abs())
                        }
                        x if x == Shape::F32x4 as u8 => (
                            execute_unary_float(
                                OperationKind::Abs,
                                FloatKind::F32,
                                lo as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Abs,
                                FloatKind::F32,
                                (lo >> 32) as u32 as u64,
                            ) << 32),
                            execute_unary_float(
                                OperationKind::Abs,
                                FloatKind::F32,
                                hi as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Abs,
                                FloatKind::F32,
                                (hi >> 32) as u32 as u64,
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            execute_unary_float(OperationKind::Abs, FloatKind::F64, lo),
                            execute_unary_float(OperationKind::Abs, FloatKind::F64, hi),
                        ),
                        _ => return Err(Trap::new("unsupported v128 abs shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Popcnt => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let mut ret_lo = 0_u64;
                    let mut ret_hi = 0_u64;
                    match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            for i in 0..16 {
                                let v = if i < 8 {
                                    byte_at(lo, i)
                                } else {
                                    byte_at(hi, i - 8)
                                };
                                let count = u64::from(v.count_ones());
                                if i < 8 {
                                    ret_lo |= count << (i * 8);
                                } else {
                                    ret_hi |= count << ((i - 8) * 8);
                                }
                            }
                        }
                        _ => return Err(Trap::new("unsupported v128 popcnt shape")),
                    }
                    self.push_value(ret_lo);
                    self.push_value(ret_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Min => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::I16x8 as u8 => {
                            let min = |lhs: u16, rhs: u16| {
                                if op.b3 {
                                    if (lhs as i16) > (rhs as i16) {
                                        rhs
                                    } else {
                                        lhs
                                    }
                                } else if lhs > rhs {
                                    rhs
                                } else {
                                    lhs
                                }
                            };
                            (
                                u64::from(min(lhs_lo as u16, rhs_lo as u16))
                                    | (u64::from(min(
                                        (lhs_lo >> 16) as u16,
                                        (rhs_lo >> 16) as u16,
                                    )) << 16)
                                    | (u64::from(min(
                                        (lhs_lo >> 32) as u16,
                                        (rhs_lo >> 32) as u16,
                                    )) << 32)
                                    | (u64::from(min(
                                        (lhs_lo >> 48) as u16,
                                        (rhs_lo >> 48) as u16,
                                    )) << 48),
                                u64::from(min(lhs_hi as u16, rhs_hi as u16))
                                    | (u64::from(min(
                                        (lhs_hi >> 16) as u16,
                                        (rhs_hi >> 16) as u16,
                                    )) << 16)
                                    | (u64::from(min(
                                        (lhs_hi >> 32) as u16,
                                        (rhs_hi >> 32) as u16,
                                    )) << 32)
                                    | (u64::from(min(
                                        (lhs_hi >> 48) as u16,
                                        (rhs_hi >> 48) as u16,
                                    )) << 48),
                            )
                        }
                        x if x == Shape::I8x16 as u8 => {
                            let min = |lhs: u8, rhs: u8| {
                                if op.b3 {
                                    if (lhs as i8) > (rhs as i8) {
                                        rhs
                                    } else {
                                        lhs
                                    }
                                } else if lhs > rhs {
                                    rhs
                                } else {
                                    lhs
                                }
                            };
                            (
                                u64::from(min(lhs_lo as u8, rhs_lo as u8))
                                    | (u64::from(min((lhs_lo >> 8) as u8, (rhs_lo >> 8) as u8))
                                        << 8)
                                    | (u64::from(min((lhs_lo >> 16) as u8, (rhs_lo >> 16) as u8))
                                        << 16)
                                    | (u64::from(min((lhs_lo >> 24) as u8, (rhs_lo >> 24) as u8))
                                        << 24)
                                    | (u64::from(min((lhs_lo >> 32) as u8, (rhs_lo >> 32) as u8))
                                        << 32)
                                    | (u64::from(min((lhs_lo >> 40) as u8, (rhs_lo >> 40) as u8))
                                        << 40)
                                    | (u64::from(min((lhs_lo >> 48) as u8, (rhs_lo >> 48) as u8))
                                        << 48)
                                    | (u64::from(min((lhs_lo >> 56) as u8, (rhs_lo >> 56) as u8))
                                        << 56),
                                u64::from(min(lhs_hi as u8, rhs_hi as u8))
                                    | (u64::from(min((lhs_hi >> 8) as u8, (rhs_hi >> 8) as u8))
                                        << 8)
                                    | (u64::from(min((lhs_hi >> 16) as u8, (rhs_hi >> 16) as u8))
                                        << 16)
                                    | (u64::from(min((lhs_hi >> 24) as u8, (rhs_hi >> 24) as u8))
                                        << 24)
                                    | (u64::from(min((lhs_hi >> 32) as u8, (rhs_hi >> 32) as u8))
                                        << 32)
                                    | (u64::from(min((lhs_hi >> 40) as u8, (rhs_hi >> 40) as u8))
                                        << 40)
                                    | (u64::from(min((lhs_hi >> 48) as u8, (rhs_hi >> 48) as u8))
                                        << 48)
                                    | (u64::from(min((lhs_hi >> 56) as u8, (rhs_hi >> 56) as u8))
                                        << 56),
                            )
                        }
                        x if x == Shape::I32x4 as u8 => {
                            let min = |lhs: u32, rhs: u32| {
                                if op.b3 {
                                    if (lhs as i32) > (rhs as i32) {
                                        rhs
                                    } else {
                                        lhs
                                    }
                                } else if lhs > rhs {
                                    rhs
                                } else {
                                    lhs
                                }
                            };
                            (
                                u64::from(min(lhs_lo as u32, rhs_lo as u32))
                                    | (u64::from(min(
                                        (lhs_lo >> 32) as u32,
                                        (rhs_lo >> 32) as u32,
                                    )) << 32),
                                u64::from(min(lhs_hi as u32, rhs_hi as u32))
                                    | (u64::from(min(
                                        (lhs_hi >> 32) as u32,
                                        (rhs_hi >> 32) as u32,
                                    )) << 32),
                            )
                        }
                        x if x == Shape::F32x4 as u8 => (
                            u64::from(
                                wasm_min_f32(
                                    f32::from_bits(lhs_lo as u32),
                                    f32::from_bits(rhs_lo as u32),
                                )
                                .to_bits(),
                            ) | (u64::from(
                                wasm_min_f32(
                                    f32::from_bits((lhs_lo >> 32) as u32),
                                    f32::from_bits((rhs_lo >> 32) as u32),
                                )
                                .to_bits(),
                            ) << 32),
                            u64::from(
                                wasm_min_f32(
                                    f32::from_bits(lhs_hi as u32),
                                    f32::from_bits(rhs_hi as u32),
                                )
                                .to_bits(),
                            ) | (u64::from(
                                wasm_min_f32(
                                    f32::from_bits((lhs_hi >> 32) as u32),
                                    f32::from_bits((rhs_hi >> 32) as u32),
                                )
                                .to_bits(),
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            wasm_min_f64(f64::from_bits(lhs_lo), f64::from_bits(rhs_lo)).to_bits(),
                            wasm_min_f64(f64::from_bits(lhs_hi), f64::from_bits(rhs_hi)).to_bits(),
                        ),
                        _ => return Err(Trap::new("unsupported v128 min shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Max => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::I16x8 as u8 => {
                            let max = |lhs: u16, rhs: u16| {
                                if op.b3 {
                                    if (lhs as i16) < (rhs as i16) {
                                        rhs
                                    } else {
                                        lhs
                                    }
                                } else if lhs < rhs {
                                    rhs
                                } else {
                                    lhs
                                }
                            };
                            (
                                u64::from(max(lhs_lo as u16, rhs_lo as u16))
                                    | (u64::from(max(
                                        (lhs_lo >> 16) as u16,
                                        (rhs_lo >> 16) as u16,
                                    )) << 16)
                                    | (u64::from(max(
                                        (lhs_lo >> 32) as u16,
                                        (rhs_lo >> 32) as u16,
                                    )) << 32)
                                    | (u64::from(max(
                                        (lhs_lo >> 48) as u16,
                                        (rhs_lo >> 48) as u16,
                                    )) << 48),
                                u64::from(max(lhs_hi as u16, rhs_hi as u16))
                                    | (u64::from(max(
                                        (lhs_hi >> 16) as u16,
                                        (rhs_hi >> 16) as u16,
                                    )) << 16)
                                    | (u64::from(max(
                                        (lhs_hi >> 32) as u16,
                                        (rhs_hi >> 32) as u16,
                                    )) << 32)
                                    | (u64::from(max(
                                        (lhs_hi >> 48) as u16,
                                        (rhs_hi >> 48) as u16,
                                    )) << 48),
                            )
                        }
                        x if x == Shape::I8x16 as u8 => {
                            let max = |lhs: u8, rhs: u8| {
                                if op.b3 {
                                    if (lhs as i8) < (rhs as i8) {
                                        rhs
                                    } else {
                                        lhs
                                    }
                                } else if lhs < rhs {
                                    rhs
                                } else {
                                    lhs
                                }
                            };
                            (
                                u64::from(max(lhs_lo as u8, rhs_lo as u8))
                                    | (u64::from(max((lhs_lo >> 8) as u8, (rhs_lo >> 8) as u8))
                                        << 8)
                                    | (u64::from(max((lhs_lo >> 16) as u8, (rhs_lo >> 16) as u8))
                                        << 16)
                                    | (u64::from(max((lhs_lo >> 24) as u8, (rhs_lo >> 24) as u8))
                                        << 24)
                                    | (u64::from(max((lhs_lo >> 32) as u8, (rhs_lo >> 32) as u8))
                                        << 32)
                                    | (u64::from(max((lhs_lo >> 40) as u8, (rhs_lo >> 40) as u8))
                                        << 40)
                                    | (u64::from(max((lhs_lo >> 48) as u8, (rhs_lo >> 48) as u8))
                                        << 48)
                                    | (u64::from(max((lhs_lo >> 56) as u8, (rhs_lo >> 56) as u8))
                                        << 56),
                                u64::from(max(lhs_hi as u8, rhs_hi as u8))
                                    | (u64::from(max((lhs_hi >> 8) as u8, (rhs_hi >> 8) as u8))
                                        << 8)
                                    | (u64::from(max((lhs_hi >> 16) as u8, (rhs_hi >> 16) as u8))
                                        << 16)
                                    | (u64::from(max((lhs_hi >> 24) as u8, (rhs_hi >> 24) as u8))
                                        << 24)
                                    | (u64::from(max((lhs_hi >> 32) as u8, (rhs_hi >> 32) as u8))
                                        << 32)
                                    | (u64::from(max((lhs_hi >> 40) as u8, (rhs_hi >> 40) as u8))
                                        << 40)
                                    | (u64::from(max((lhs_hi >> 48) as u8, (rhs_hi >> 48) as u8))
                                        << 48)
                                    | (u64::from(max((lhs_hi >> 56) as u8, (rhs_hi >> 56) as u8))
                                        << 56),
                            )
                        }
                        x if x == Shape::I32x4 as u8 => {
                            let max = |lhs: u32, rhs: u32| {
                                if op.b3 {
                                    if (lhs as i32) < (rhs as i32) {
                                        rhs
                                    } else {
                                        lhs
                                    }
                                } else if lhs < rhs {
                                    rhs
                                } else {
                                    lhs
                                }
                            };
                            (
                                u64::from(max(lhs_lo as u32, rhs_lo as u32))
                                    | (u64::from(max(
                                        (lhs_lo >> 32) as u32,
                                        (rhs_lo >> 32) as u32,
                                    )) << 32),
                                u64::from(max(lhs_hi as u32, rhs_hi as u32))
                                    | (u64::from(max(
                                        (lhs_hi >> 32) as u32,
                                        (rhs_hi >> 32) as u32,
                                    )) << 32),
                            )
                        }
                        x if x == Shape::F32x4 as u8 => (
                            u64::from(
                                wasm_max_f32(
                                    f32::from_bits(lhs_lo as u32),
                                    f32::from_bits(rhs_lo as u32),
                                )
                                .to_bits(),
                            ) | (u64::from(
                                wasm_max_f32(
                                    f32::from_bits((lhs_lo >> 32) as u32),
                                    f32::from_bits((rhs_lo >> 32) as u32),
                                )
                                .to_bits(),
                            ) << 32),
                            u64::from(
                                wasm_max_f32(
                                    f32::from_bits(lhs_hi as u32),
                                    f32::from_bits(rhs_hi as u32),
                                )
                                .to_bits(),
                            ) | (u64::from(
                                wasm_max_f32(
                                    f32::from_bits((lhs_hi >> 32) as u32),
                                    f32::from_bits((rhs_hi >> 32) as u32),
                                )
                                .to_bits(),
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            wasm_max_f64(f64::from_bits(lhs_lo), f64::from_bits(rhs_lo)).to_bits(),
                            wasm_max_f64(f64::from_bits(lhs_hi), f64::from_bits(rhs_hi)).to_bits(),
                        ),
                        _ => return Err(Trap::new("unsupported v128 max shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Pmin => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::F32x4 as u8 => {
                            let lo0 = if f32_lt(
                                f32::from_bits(rhs_lo as u32),
                                f32::from_bits(lhs_lo as u32),
                            ) {
                                rhs_lo & 0x0000_0000_ffff_ffff
                            } else {
                                lhs_lo & 0x0000_0000_ffff_ffff
                            };
                            let lo1 = if f32_lt(
                                f32::from_bits((rhs_lo >> 32) as u32),
                                f32::from_bits((lhs_lo >> 32) as u32),
                            ) {
                                rhs_lo & 0xffff_ffff_0000_0000
                            } else {
                                lhs_lo & 0xffff_ffff_0000_0000
                            };
                            let hi0 = if f32_lt(
                                f32::from_bits(rhs_hi as u32),
                                f32::from_bits(lhs_hi as u32),
                            ) {
                                rhs_hi & 0x0000_0000_ffff_ffff
                            } else {
                                lhs_hi & 0x0000_0000_ffff_ffff
                            };
                            let hi1 = if f32_lt(
                                f32::from_bits((rhs_hi >> 32) as u32),
                                f32::from_bits((lhs_hi >> 32) as u32),
                            ) {
                                rhs_hi & 0xffff_ffff_0000_0000
                            } else {
                                lhs_hi & 0xffff_ffff_0000_0000
                            };
                            (lo0 | lo1, hi0 | hi1)
                        }
                        x if x == Shape::F64x2 as u8 => (
                            if f64_lt(f64::from_bits(rhs_lo), f64::from_bits(lhs_lo)) {
                                rhs_lo
                            } else {
                                lhs_lo
                            },
                            if f64_lt(f64::from_bits(rhs_hi), f64::from_bits(lhs_hi)) {
                                rhs_hi
                            } else {
                                lhs_hi
                            },
                        ),
                        _ => return Err(Trap::new("unsupported v128 pmin shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Pmax => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::F32x4 as u8 => {
                            let lo0 = if f32_lt(
                                f32::from_bits(lhs_lo as u32),
                                f32::from_bits(rhs_lo as u32),
                            ) {
                                rhs_lo & 0x0000_0000_ffff_ffff
                            } else {
                                lhs_lo & 0x0000_0000_ffff_ffff
                            };
                            let lo1 = if f32_lt(
                                f32::from_bits((lhs_lo >> 32) as u32),
                                f32::from_bits((rhs_lo >> 32) as u32),
                            ) {
                                rhs_lo & 0xffff_ffff_0000_0000
                            } else {
                                lhs_lo & 0xffff_ffff_0000_0000
                            };
                            let hi0 = if f32_lt(
                                f32::from_bits(lhs_hi as u32),
                                f32::from_bits(rhs_hi as u32),
                            ) {
                                rhs_hi & 0x0000_0000_ffff_ffff
                            } else {
                                lhs_hi & 0x0000_0000_ffff_ffff
                            };
                            let hi1 = if f32_lt(
                                f32::from_bits((lhs_hi >> 32) as u32),
                                f32::from_bits((rhs_hi >> 32) as u32),
                            ) {
                                rhs_hi & 0xffff_ffff_0000_0000
                            } else {
                                lhs_hi & 0xffff_ffff_0000_0000
                            };
                            (lo0 | lo1, hi0 | hi1)
                        }
                        x if x == Shape::F64x2 as u8 => (
                            if f64_lt(f64::from_bits(lhs_lo), f64::from_bits(rhs_lo)) {
                                rhs_lo
                            } else {
                                lhs_lo
                            },
                            if f64_lt(f64::from_bits(lhs_hi), f64::from_bits(rhs_hi)) {
                                rhs_hi
                            } else {
                                lhs_hi
                            },
                        ),
                        _ => return Err(Trap::new("unsupported v128 pmax shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128AvgrU => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::I8x16 as u8 => (
                            u64::from((u16::from(lhs_lo as u8) + u16::from(rhs_lo as u8) + 1) / 2)
                                | (u64::from(
                                    (u16::from((lhs_lo >> 8) as u8)
                                        + u16::from((rhs_lo >> 8) as u8)
                                        + 1)
                                        / 2,
                                ) << 8)
                                | (u64::from(
                                    (u16::from((lhs_lo >> 16) as u8)
                                        + u16::from((rhs_lo >> 16) as u8)
                                        + 1)
                                        / 2,
                                ) << 16)
                                | (u64::from(
                                    (u16::from((lhs_lo >> 24) as u8)
                                        + u16::from((rhs_lo >> 24) as u8)
                                        + 1)
                                        / 2,
                                ) << 24)
                                | (u64::from(
                                    (u16::from((lhs_lo >> 32) as u8)
                                        + u16::from((rhs_lo >> 32) as u8)
                                        + 1)
                                        / 2,
                                ) << 32)
                                | (u64::from(
                                    (u16::from((lhs_lo >> 40) as u8)
                                        + u16::from((rhs_lo >> 40) as u8)
                                        + 1)
                                        / 2,
                                ) << 40)
                                | (u64::from(
                                    (u16::from((lhs_lo >> 48) as u8)
                                        + u16::from((rhs_lo >> 48) as u8)
                                        + 1)
                                        / 2,
                                ) << 48)
                                | (u64::from(
                                    (u16::from((lhs_lo >> 56) as u8)
                                        + u16::from((rhs_lo >> 56) as u8)
                                        + 1)
                                        / 2,
                                ) << 56),
                            u64::from((u16::from(lhs_hi as u8) + u16::from(rhs_hi as u8) + 1) / 2)
                                | (u64::from(
                                    (u16::from((lhs_hi >> 8) as u8)
                                        + u16::from((rhs_hi >> 8) as u8)
                                        + 1)
                                        / 2,
                                ) << 8)
                                | (u64::from(
                                    (u16::from((lhs_hi >> 16) as u8)
                                        + u16::from((rhs_hi >> 16) as u8)
                                        + 1)
                                        / 2,
                                ) << 16)
                                | (u64::from(
                                    (u16::from((lhs_hi >> 24) as u8)
                                        + u16::from((rhs_hi >> 24) as u8)
                                        + 1)
                                        / 2,
                                ) << 24)
                                | (u64::from(
                                    (u16::from((lhs_hi >> 32) as u8)
                                        + u16::from((rhs_hi >> 32) as u8)
                                        + 1)
                                        / 2,
                                ) << 32)
                                | (u64::from(
                                    (u16::from((lhs_hi >> 40) as u8)
                                        + u16::from((rhs_hi >> 40) as u8)
                                        + 1)
                                        / 2,
                                ) << 40)
                                | (u64::from(
                                    (u16::from((lhs_hi >> 48) as u8)
                                        + u16::from((rhs_hi >> 48) as u8)
                                        + 1)
                                        / 2,
                                ) << 48)
                                | (u64::from(
                                    (u16::from((lhs_hi >> 56) as u8)
                                        + u16::from((rhs_hi >> 56) as u8)
                                        + 1)
                                        / 2,
                                ) << 56),
                        ),
                        x if x == Shape::I16x8 as u8 => (
                            u64::from(((lhs_lo as u16 as u32) + (rhs_lo as u16 as u32) + 1) / 2)
                                | (u64::from(
                                    (((lhs_lo >> 16) as u16 as u32)
                                        + ((rhs_lo >> 16) as u16 as u32)
                                        + 1)
                                        / 2,
                                ) << 16)
                                | (u64::from(
                                    (((lhs_lo >> 32) as u16 as u32)
                                        + ((rhs_lo >> 32) as u16 as u32)
                                        + 1)
                                        / 2,
                                ) << 32)
                                | (u64::from(
                                    (((lhs_lo >> 48) as u16 as u32)
                                        + ((rhs_lo >> 48) as u16 as u32)
                                        + 1)
                                        / 2,
                                ) << 48),
                            u64::from(((lhs_hi as u16 as u32) + (rhs_hi as u16 as u32) + 1) / 2)
                                | (u64::from(
                                    (((lhs_hi >> 16) as u16 as u32)
                                        + ((rhs_hi >> 16) as u16 as u32)
                                        + 1)
                                        / 2,
                                ) << 16)
                                | (u64::from(
                                    (((lhs_hi >> 32) as u16 as u32)
                                        + ((rhs_hi >> 32) as u16 as u32)
                                        + 1)
                                        / 2,
                                ) << 32)
                                | (u64::from(
                                    (((lhs_hi >> 48) as u16 as u32)
                                        + ((rhs_hi >> 48) as u16 as u32)
                                        + 1)
                                        / 2,
                                ) << 48),
                        ),
                        _ => return Err(Trap::new("unsupported v128 avgr_u shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Ceil => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::F32x4 as u8 => (
                            execute_unary_float(
                                OperationKind::Ceil,
                                FloatKind::F32,
                                lo as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Ceil,
                                FloatKind::F32,
                                (lo >> 32) as u32 as u64,
                            ) << 32),
                            execute_unary_float(
                                OperationKind::Ceil,
                                FloatKind::F32,
                                hi as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Ceil,
                                FloatKind::F32,
                                (hi >> 32) as u32 as u64,
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            execute_unary_float(OperationKind::Ceil, FloatKind::F64, lo),
                            execute_unary_float(OperationKind::Ceil, FloatKind::F64, hi),
                        ),
                        _ => return Err(Trap::new("unsupported v128 ceil shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Floor => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::F32x4 as u8 => (
                            execute_unary_float(
                                OperationKind::Floor,
                                FloatKind::F32,
                                lo as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Floor,
                                FloatKind::F32,
                                (lo >> 32) as u32 as u64,
                            ) << 32),
                            execute_unary_float(
                                OperationKind::Floor,
                                FloatKind::F32,
                                hi as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Floor,
                                FloatKind::F32,
                                (hi >> 32) as u32 as u64,
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            execute_unary_float(OperationKind::Floor, FloatKind::F64, lo),
                            execute_unary_float(OperationKind::Floor, FloatKind::F64, hi),
                        ),
                        _ => return Err(Trap::new("unsupported v128 floor shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Trunc => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::F32x4 as u8 => (
                            execute_unary_float(
                                OperationKind::Trunc,
                                FloatKind::F32,
                                lo as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Trunc,
                                FloatKind::F32,
                                (lo >> 32) as u32 as u64,
                            ) << 32),
                            execute_unary_float(
                                OperationKind::Trunc,
                                FloatKind::F32,
                                hi as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Trunc,
                                FloatKind::F32,
                                (hi >> 32) as u32 as u64,
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            execute_unary_float(OperationKind::Trunc, FloatKind::F64, lo),
                            execute_unary_float(OperationKind::Trunc, FloatKind::F64, hi),
                        ),
                        _ => return Err(Trap::new("unsupported v128 trunc shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Nearest => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let (lo, hi) = match op.b1 {
                        x if x == Shape::F32x4 as u8 => (
                            execute_unary_float(
                                OperationKind::Nearest,
                                FloatKind::F32,
                                lo as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Nearest,
                                FloatKind::F32,
                                (lo >> 32) as u32 as u64,
                            ) << 32),
                            execute_unary_float(
                                OperationKind::Nearest,
                                FloatKind::F32,
                                hi as u32 as u64,
                            ) | (execute_unary_float(
                                OperationKind::Nearest,
                                FloatKind::F32,
                                (hi >> 32) as u32 as u64,
                            ) << 32),
                        ),
                        x if x == Shape::F64x2 as u8 => (
                            execute_unary_float(OperationKind::Nearest, FloatKind::F64, lo),
                            execute_unary_float(OperationKind::Nearest, FloatKind::F64, hi),
                        ),
                        _ => return Err(Trap::new("unsupported v128 nearest shape")),
                    };
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128FloatPromote => {
                    let _ = self.pop_value();
                    let to_promote = self.pop_value();
                    self.push_value(f64::from(f32::from_bits(to_promote as u32)).to_bits());
                    self.push_value(f64::from(f32::from_bits((to_promote >> 32) as u32)).to_bits());
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128FloatDemote => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    self.push_value(
                        u64::from((f64::from_bits(lo) as f32).to_bits())
                            | (u64::from((f64::from_bits(hi) as f32).to_bits()) << 32),
                    );
                    self.push_value(0);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128FConvertFromI => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let v1 = lo as u32;
                    let v2 = (lo >> 32) as u32;
                    let v3 = hi as u32;
                    let v4 = (hi >> 32) as u32;
                    let signed = op.b3;
                    let (ret_lo, ret_hi) = match op.b1 {
                        x if x == Shape::F32x4 as u8 => {
                            if signed {
                                (
                                    u64::from((v1 as i32 as f32).to_bits())
                                        | (u64::from((v2 as i32 as f32).to_bits()) << 32),
                                    u64::from((v3 as i32 as f32).to_bits())
                                        | (u64::from((v4 as i32 as f32).to_bits()) << 32),
                                )
                            } else {
                                (
                                    u64::from((v1 as f32).to_bits())
                                        | (u64::from((v2 as f32).to_bits()) << 32),
                                    u64::from((v3 as f32).to_bits())
                                        | (u64::from((v4 as f32).to_bits()) << 32),
                                )
                            }
                        }
                        x if x == Shape::F64x2 as u8 => {
                            if signed {
                                (
                                    f64::from(v1 as i32).to_bits(),
                                    f64::from(v2 as i32).to_bits(),
                                )
                            } else {
                                (f64::from(v1).to_bits(), f64::from(v2).to_bits())
                            }
                        }
                        _ => {
                            return Err(Trap::new("unsupported v128 int-to-float conversion shape"))
                        }
                    };
                    self.push_value(ret_lo);
                    self.push_value(ret_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Narrow => {
                    let rhs_hi = self.pop_value();
                    let rhs_lo = self.pop_value();
                    let lhs_hi = self.pop_value();
                    let lhs_lo = self.pop_value();
                    let signed = op.b3;
                    let (mut ret_lo, mut ret_hi) = (0_u64, 0_u64);
                    match op.b1 {
                        x if x == Shape::I16x8 as u8 => {
                            for i in 0..8 {
                                let v16 = if i < 4 {
                                    (lhs_lo >> (i * 16)) as u16
                                } else {
                                    (lhs_hi >> ((i - 4) * 16)) as u16
                                };
                                let v = if signed {
                                    (v16 as i16).clamp(i8::MIN as i16, i8::MAX as i16) as i8 as u8
                                } else {
                                    (v16 as i16).clamp(0, u8::MAX as i16) as u8
                                };
                                ret_lo |= u64::from(v) << (i * 8);
                            }
                            for i in 0..8 {
                                let v16 = if i < 4 {
                                    (rhs_lo >> (i * 16)) as u16
                                } else {
                                    (rhs_hi >> ((i - 4) * 16)) as u16
                                };
                                let v = if signed {
                                    (v16 as i16).clamp(i8::MIN as i16, i8::MAX as i16) as i8 as u8
                                } else {
                                    (v16 as i16).clamp(0, u8::MAX as i16) as u8
                                };
                                ret_hi |= u64::from(v) << (i * 8);
                            }
                        }
                        x if x == Shape::I32x4 as u8 => {
                            for i in 0..4 {
                                let v32 = if i < 2 {
                                    (lhs_lo >> (i * 32)) as u32
                                } else {
                                    (lhs_hi >> ((i - 2) * 32)) as u32
                                };
                                let v = if signed {
                                    (v32 as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16
                                        as u16
                                } else {
                                    (v32 as i32).clamp(0, u16::MAX as i32) as u16
                                };
                                ret_lo |= u64::from(v) << (i * 16);
                            }
                            for i in 0..4 {
                                let v32 = if i < 2 {
                                    (rhs_lo >> (i * 32)) as u32
                                } else {
                                    (rhs_hi >> ((i - 2) * 32)) as u32
                                };
                                let v = if signed {
                                    (v32 as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16
                                        as u16
                                } else {
                                    (v32 as i32).clamp(0, u16::MAX as i32) as u16
                                };
                                ret_hi |= u64::from(v) << (i * 16);
                            }
                        }
                        _ => return Err(Trap::new("unsupported v128 narrow shape")),
                    }
                    self.push_value(ret_lo);
                    self.push_value(ret_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Extend => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let origin = if op.b3 { lo } else { hi };
                    let signed = op.b2 == 1;
                    let (mut ret_lo, mut ret_hi) = (0_u64, 0_u64);
                    match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            for i in 0..8 {
                                let v8 = (origin >> (i * 8)) as u8;
                                let v16 = if signed {
                                    (v8 as i8 as i16) as u16
                                } else {
                                    u16::from(v8)
                                };
                                if i < 4 {
                                    ret_lo |= u64::from(v16) << (i * 16);
                                } else {
                                    ret_hi |= u64::from(v16) << ((i - 4) * 16);
                                }
                            }
                        }
                        x if x == Shape::I16x8 as u8 => {
                            for i in 0..4 {
                                let v16 = (origin >> (i * 16)) as u16;
                                let v32 = if signed {
                                    (v16 as i16 as i32) as u32
                                } else {
                                    u32::from(v16)
                                };
                                if i < 2 {
                                    ret_lo |= u64::from(v32) << (i * 32);
                                } else {
                                    ret_hi |= u64::from(v32) << ((i - 2) * 32);
                                }
                            }
                        }
                        x if x == Shape::I32x4 as u8 => {
                            let v32_lo = origin as u32;
                            let v32_hi = (origin >> 32) as u32;
                            if signed {
                                ret_lo = (v32_lo as i32 as i64) as u64;
                                ret_hi = (v32_hi as i32 as i64) as u64;
                            } else {
                                ret_lo = u64::from(v32_lo);
                                ret_hi = u64::from(v32_hi);
                            }
                        }
                        _ => return Err(Trap::new("unsupported v128 extend shape")),
                    }
                    self.push_value(ret_lo);
                    self.push_value(ret_hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128ITruncSatFromF => {
                    let hi = self.pop_value();
                    let lo = self.pop_value();
                    let signed = op.b3;
                    let (mut ret_lo, ret_hi) = (0_u64, 0_u64);
                    match op.b1 {
                        x if x == Shape::F32x4 as u8 => {
                            let values = [
                                f64::from(f32::from_bits(lo as u32)).trunc(),
                                f64::from(f32::from_bits((lo >> 32) as u32)).trunc(),
                                f64::from(f32::from_bits(hi as u32)).trunc(),
                                f64::from(f32::from_bits((hi >> 32) as u32)).trunc(),
                            ];
                            let mut hi_out = 0_u64;
                            for (i, mut f) in values.into_iter().enumerate() {
                                let v = if f.is_nan() {
                                    0
                                } else if signed {
                                    f = f.clamp(i32::MIN as f64, i32::MAX as f64);
                                    (f as i32) as u32
                                } else {
                                    f = f.clamp(0.0, u32::MAX as f64);
                                    f as u32
                                };
                                if i < 2 {
                                    ret_lo |= u64::from(v) << (i * 32);
                                } else {
                                    hi_out |= u64::from(v) << ((i - 2) * 32);
                                }
                            }
                            self.push_value(ret_lo);
                            self.push_value(hi_out);
                        }
                        x if x == Shape::F64x2 as u8 => {
                            for (i, mut f) in
                                [f64::from_bits(lo).trunc(), f64::from_bits(hi).trunc()]
                                    .into_iter()
                                    .enumerate()
                            {
                                let v = if f.is_nan() {
                                    0
                                } else if signed {
                                    f = f.clamp(i32::MIN as f64, i32::MAX as f64);
                                    (f as i32) as u32
                                } else {
                                    f = f.clamp(0.0, u32::MAX as f64);
                                    f as u32
                                };
                                ret_lo |= u64::from(v) << (i * 32);
                            }
                            self.push_value(ret_lo);
                            self.push_value(ret_hi);
                        }
                        _ => return Err(Trap::new("unsupported v128 trunc_sat shape")),
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Shl => {
                    let mut shift = self.pop_value();
                    let mut hi = self.pop_value();
                    let mut lo = self.pop_value();
                    match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            shift %= 8;
                            lo = u64::from((lo << shift) as u8)
                                | (u64::from(((lo >> 8) << shift) as u8) << 8)
                                | (u64::from(((lo >> 16) << shift) as u8) << 16)
                                | (u64::from(((lo >> 24) << shift) as u8) << 24)
                                | (u64::from(((lo >> 32) << shift) as u8) << 32)
                                | (u64::from(((lo >> 40) << shift) as u8) << 40)
                                | (u64::from(((lo >> 48) << shift) as u8) << 48)
                                | (u64::from(((lo >> 56) << shift) as u8) << 56);
                            hi = u64::from((hi << shift) as u8)
                                | (u64::from(((hi >> 8) << shift) as u8) << 8)
                                | (u64::from(((hi >> 16) << shift) as u8) << 16)
                                | (u64::from(((hi >> 24) << shift) as u8) << 24)
                                | (u64::from(((hi >> 32) << shift) as u8) << 32)
                                | (u64::from(((hi >> 40) << shift) as u8) << 40)
                                | (u64::from(((hi >> 48) << shift) as u8) << 48)
                                | (u64::from(((hi >> 56) << shift) as u8) << 56);
                        }
                        x if x == Shape::I16x8 as u8 => {
                            shift %= 16;
                            lo = u64::from((lo << shift) as u16)
                                | (u64::from(((lo >> 16) << shift) as u16) << 16)
                                | (u64::from(((lo >> 32) << shift) as u16) << 32)
                                | (u64::from(((lo >> 48) << shift) as u16) << 48);
                            hi = u64::from((hi << shift) as u16)
                                | (u64::from(((hi >> 16) << shift) as u16) << 16)
                                | (u64::from(((hi >> 32) << shift) as u16) << 32)
                                | (u64::from(((hi >> 48) << shift) as u16) << 48);
                        }
                        x if x == Shape::I32x4 as u8 => {
                            shift %= 32;
                            lo = u64::from((lo << shift) as u32)
                                | (u64::from(((lo >> 32) << shift) as u32) << 32);
                            hi = u64::from((hi << shift) as u32)
                                | (u64::from(((hi >> 32) << shift) as u32) << 32);
                        }
                        x if x == Shape::I64x2 as u8 => {
                            shift %= 64;
                            lo <<= shift;
                            hi <<= shift;
                        }
                        _ => return Err(Trap::new("unsupported v128 shift shape")),
                    }
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::V128Shr => {
                    let mut shift = self.pop_value();
                    let mut hi = self.pop_value();
                    let mut lo = self.pop_value();
                    match op.b1 {
                        x if x == Shape::I8x16 as u8 => {
                            shift %= 8;
                            if op.b3 {
                                lo = u64::from((lo as i8 >> shift) as u8)
                                    | (u64::from(((lo >> 8) as i8 >> shift) as u8) << 8)
                                    | (u64::from(((lo >> 16) as i8 >> shift) as u8) << 16)
                                    | (u64::from(((lo >> 24) as i8 >> shift) as u8) << 24)
                                    | (u64::from(((lo >> 32) as i8 >> shift) as u8) << 32)
                                    | (u64::from(((lo >> 40) as i8 >> shift) as u8) << 40)
                                    | (u64::from(((lo >> 48) as i8 >> shift) as u8) << 48)
                                    | (u64::from(((lo >> 56) as i8 >> shift) as u8) << 56);
                                hi = u64::from((hi as i8 >> shift) as u8)
                                    | (u64::from(((hi >> 8) as i8 >> shift) as u8) << 8)
                                    | (u64::from(((hi >> 16) as i8 >> shift) as u8) << 16)
                                    | (u64::from(((hi >> 24) as i8 >> shift) as u8) << 24)
                                    | (u64::from(((hi >> 32) as i8 >> shift) as u8) << 32)
                                    | (u64::from(((hi >> 40) as i8 >> shift) as u8) << 40)
                                    | (u64::from(((hi >> 48) as i8 >> shift) as u8) << 48)
                                    | (u64::from(((hi >> 56) as i8 >> shift) as u8) << 56);
                            } else {
                                lo = u64::from((lo as u8) >> shift)
                                    | (u64::from(((lo >> 8) as u8) >> shift) << 8)
                                    | (u64::from(((lo >> 16) as u8) >> shift) << 16)
                                    | (u64::from(((lo >> 24) as u8) >> shift) << 24)
                                    | (u64::from(((lo >> 32) as u8) >> shift) << 32)
                                    | (u64::from(((lo >> 40) as u8) >> shift) << 40)
                                    | (u64::from(((lo >> 48) as u8) >> shift) << 48)
                                    | (u64::from(((lo >> 56) as u8) >> shift) << 56);
                                hi = u64::from((hi as u8) >> shift)
                                    | (u64::from(((hi >> 8) as u8) >> shift) << 8)
                                    | (u64::from(((hi >> 16) as u8) >> shift) << 16)
                                    | (u64::from(((hi >> 24) as u8) >> shift) << 24)
                                    | (u64::from(((hi >> 32) as u8) >> shift) << 32)
                                    | (u64::from(((hi >> 40) as u8) >> shift) << 40)
                                    | (u64::from(((hi >> 48) as u8) >> shift) << 48)
                                    | (u64::from(((hi >> 56) as u8) >> shift) << 56);
                            }
                        }
                        x if x == Shape::I16x8 as u8 => {
                            shift %= 16;
                            if op.b3 {
                                lo = u64::from((lo as i16 >> shift) as u16)
                                    | (u64::from(((lo >> 16) as i16 >> shift) as u16) << 16)
                                    | (u64::from(((lo >> 32) as i16 >> shift) as u16) << 32)
                                    | (u64::from(((lo >> 48) as i16 >> shift) as u16) << 48);
                                hi = u64::from((hi as i16 >> shift) as u16)
                                    | (u64::from(((hi >> 16) as i16 >> shift) as u16) << 16)
                                    | (u64::from(((hi >> 32) as i16 >> shift) as u16) << 32)
                                    | (u64::from(((hi >> 48) as i16 >> shift) as u16) << 48);
                            } else {
                                lo = u64::from((lo as u16) >> shift)
                                    | (u64::from(((lo >> 16) as u16) >> shift) << 16)
                                    | (u64::from(((lo >> 32) as u16) >> shift) << 32)
                                    | (u64::from(((lo >> 48) as u16) >> shift) << 48);
                                hi = u64::from((hi as u16) >> shift)
                                    | (u64::from(((hi >> 16) as u16) >> shift) << 16)
                                    | (u64::from(((hi >> 32) as u16) >> shift) << 32)
                                    | (u64::from(((hi >> 48) as u16) >> shift) << 48);
                            }
                        }
                        x if x == Shape::I32x4 as u8 => {
                            shift %= 32;
                            if op.b3 {
                                lo = u64::from((lo as i32 >> shift) as u32)
                                    | (u64::from(((lo >> 32) as i32 >> shift) as u32) << 32);
                                hi = u64::from((hi as i32 >> shift) as u32)
                                    | (u64::from(((hi >> 32) as i32 >> shift) as u32) << 32);
                            } else {
                                lo = u64::from((lo as u32) >> shift)
                                    | (u64::from(((lo >> 32) as u32) >> shift) << 32);
                                hi = u64::from((hi as u32) >> shift)
                                    | (u64::from(((hi >> 32) as u32) >> shift) << 32);
                            }
                        }
                        x if x == Shape::I64x2 as u8 => {
                            shift %= 64;
                            if op.b3 {
                                lo = (lo as i64 >> shift) as u64;
                                hi = (hi as i64 >> shift) as u64;
                            } else {
                                lo >>= shift;
                                hi >>= shift;
                            }
                        }
                        _ => return Err(Trap::new("unsupported v128 shift shape")),
                    }
                    self.push_value(lo);
                    self.push_value(hi);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Eq => {
                    let v2 = self.pop_value();
                    let v1 = self.pop_value();
                    let matches = match decode_unsigned_type(op.b1) {
                        UnsignedType::I32 => (v1 as u32) == (v2 as u32),
                        UnsignedType::I64 => v1 == v2,
                        UnsignedType::F32 => f32::from_bits(v1 as u32) == f32::from_bits(v2 as u32),
                        UnsignedType::F64 => f64::from_bits(v1) == f64::from_bits(v2),
                        _ => return Err(Trap::new("unsupported eq type")),
                    };
                    self.push_value(u64::from(matches));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Ne => {
                    let v2 = self.pop_value();
                    let v1 = self.pop_value();
                    let differs = match decode_unsigned_type(op.b1) {
                        UnsignedType::I32 => (v1 as u32) != (v2 as u32),
                        UnsignedType::I64 => v1 != v2,
                        UnsignedType::F32 => f32::from_bits(v1 as u32) != f32::from_bits(v2 as u32),
                        UnsignedType::F64 => f64::from_bits(v1) != f64::from_bits(v2),
                        _ => return Err(Trap::new("unsupported ne type")),
                    };
                    self.push_value(u64::from(differs));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Eqz => {
                    let value = self.pop_value();
                    self.push_value(u64::from(value == 0));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Lt | OperationKind::Gt | OperationKind::Le | OperationKind::Ge => {
                    let v2 = self.pop_value();
                    let v1 = self.pop_value();
                    let compare = match decode_signed_type(op.b1) {
                        SignedType::Int32 => {
                            compare_i32(v1 as u32 as i32, v2 as u32 as i32, op.kind)
                        }
                        SignedType::Uint32 => compare_u32(v1 as u32, v2 as u32, op.kind),
                        SignedType::Int64 => compare_i64(v1 as i64, v2 as i64, op.kind),
                        SignedType::Uint64 => compare_u64(v1, v2, op.kind),
                        SignedType::Float32 => compare_f32(
                            f32::from_bits(v1 as u32),
                            f32::from_bits(v2 as u32),
                            op.kind,
                        ),
                        SignedType::Float64 => {
                            compare_f64(f64::from_bits(v1), f64::from_bits(v2), op.kind)
                        }
                    };
                    self.push_value(u64::from(compare));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Add | OperationKind::Sub | OperationKind::Mul => {
                    let v2 = self.pop_value();
                    let v1 = self.pop_value();
                    self.push_value(execute_binary_numeric(
                        op.kind,
                        decode_unsigned_type(op.b1),
                        v1,
                        v2,
                    )?);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Clz => {
                    let value = self.pop_value();
                    let result = match decode_unsigned_type(op.b1) {
                        UnsignedType::I32 => (value as u32).leading_zeros() as u64,
                        UnsignedType::I64 => value.leading_zeros() as u64,
                        _ => return Err(Trap::new("unsupported clz type")),
                    };
                    self.push_value(result);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Ctz => {
                    let value = self.pop_value();
                    let result = match decode_unsigned_type(op.b1) {
                        UnsignedType::I32 => (value as u32).trailing_zeros() as u64,
                        UnsignedType::I64 => value.trailing_zeros() as u64,
                        _ => return Err(Trap::new("unsupported ctz type")),
                    };
                    self.push_value(result);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Popcnt => {
                    let value = self.pop_value();
                    let result = match decode_unsigned_type(op.b1) {
                        UnsignedType::I32 => (value as u32).count_ones() as u64,
                        UnsignedType::I64 => value.count_ones() as u64,
                        _ => return Err(Trap::new("unsupported popcnt type")),
                    };
                    self.push_value(result);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Div => {
                    let v2 = self.pop_value();
                    let v1 = self.pop_value();
                    self.push_value(execute_div(decode_signed_type(op.b1), v1, v2)?);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Rem => {
                    let v2 = self.pop_value();
                    let v1 = self.pop_value();
                    self.push_value(execute_rem(decode_signed_type(op.b1), v1, v2)?);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::And => {
                    let v2 = self.pop_value();
                    let v1 = self.pop_value();
                    self.push_value(v1 & v2);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Or => {
                    let v2 = self.pop_value();
                    let v1 = self.pop_value();
                    self.push_value(v1 | v2);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Xor => {
                    let v2 = self.pop_value();
                    let v1 = self.pop_value();
                    self.push_value(v1 ^ v2);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Shl
                | OperationKind::Shr
                | OperationKind::Rotl
                | OperationKind::Rotr => {
                    let v2 = self.pop_value();
                    let v1 = self.pop_value();
                    self.push_value(execute_shift(op.kind, op.b1, v1, v2));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Abs
                | OperationKind::Neg
                | OperationKind::Ceil
                | OperationKind::Floor
                | OperationKind::Trunc
                | OperationKind::Nearest
                | OperationKind::Sqrt => {
                    let value = self.pop_value();
                    self.push_value(execute_unary_float(
                        op.kind,
                        decode_float_kind(op.b1),
                        value,
                    ));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Min | OperationKind::Max | OperationKind::Copysign => {
                    let v2 = self.pop_value();
                    let v1 = self.pop_value();
                    self.push_value(execute_binary_float(
                        op.kind,
                        decode_float_kind(op.b1),
                        v1,
                        v2,
                    ));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::I32WrapFromI64 => {
                    let value = self.pop_value();
                    self.push_value(u64::from(value as u32));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::ITruncFromF => {
                    let value = self.pop_value();
                    let result = execute_trunc_from_float(
                        decode_float_kind(op.b1),
                        decode_signed_int(op.b2),
                        op.b3,
                        value,
                    )?;
                    self.push_value(result);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::FConvertFromI => {
                    let value = self.pop_value();
                    self.push_value(execute_float_convert(
                        decode_signed_int(op.b1),
                        decode_float_kind(op.b2),
                        value,
                    ));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::F32DemoteFromF64 => {
                    let value = self.pop_value();
                    self.push_value(u64::from((f64::from_bits(value) as f32).to_bits()));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::F64PromoteFromF32 => {
                    let value = self.pop_value();
                    self.push_value((f32::from_bits(value as u32) as f64).to_bits());
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::I32ReinterpretFromF32 => {
                    let value = self.pop_value();
                    self.push_value(u64::from(value as u32));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::I64ReinterpretFromF64 => {
                    self.frames.last_mut().expect("frame").pc += 1
                }
                OperationKind::F32ReinterpretFromI32 => {
                    let value = self.pop_value();
                    self.push_value(u64::from(value as u32));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::F64ReinterpretFromI64 => {
                    self.frames.last_mut().expect("frame").pc += 1
                }
                OperationKind::Extend => {
                    let value = self.pop_value() as u32;
                    self.push_value(if op.b1 == 1 {
                        value as i32 as i64 as u64
                    } else {
                        u64::from(value)
                    });
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::SignExtend32From8 => {
                    let value = self.pop_value();
                    self.push_value(u64::from((value as u8 as i8 as i32) as u32));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::SignExtend32From16 => {
                    let value = self.pop_value();
                    self.push_value(u64::from((value as u16 as i16 as i32) as u32));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::SignExtend64From8 => {
                    let value = self.pop_value();
                    self.push_value(value as u8 as i8 as i64 as u64);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::SignExtend64From16 => {
                    let value = self.pop_value();
                    self.push_value(value as u16 as i16 as i64 as u64);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::SignExtend64From32 => {
                    let value = self.pop_value();
                    self.push_value(value as u32 as i32 as i64 as u64);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::MemoryInit => {
                    let len = self.pop_value() as usize;
                    let src_offset = self.pop_value() as usize;
                    let dst_offset = self.pop_value() as usize;
                    let data = interpreter
                        .module
                        .data_instances
                        .get(op.u1 as usize)
                        .and_then(|segment| segment.as_deref())
                        .unwrap_or(&[]);
                    let end = src_offset
                        .checked_add(len)
                        .filter(|end| *end <= data.len())
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    let bytes = data[src_offset..end].to_vec();
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let target = dst_offset
                        .checked_add(len)
                        .filter(|end| *end <= memory.bytes().len())
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    memory.bytes_mut()[dst_offset..target].copy_from_slice(&bytes);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::DataDrop => {
                    if let Some(data) = interpreter.module.data_instances.get_mut(op.u1 as usize) {
                        *data = None;
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::MemoryCopy => {
                    let len = self.pop_value() as usize;
                    let src = self.pop_value() as usize;
                    let dst = self.pop_value() as usize;
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let src_end = src
                        .checked_add(len)
                        .filter(|end| *end <= memory.bytes().len())
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    let dst_end = dst
                        .checked_add(len)
                        .filter(|end| *end <= memory.bytes().len())
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    memory.bytes_mut().copy_within(src..src_end, dst);
                    let _ = dst_end;
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::MemoryFill => {
                    let len = self.pop_value() as usize;
                    let value = self.pop_value() as u8;
                    let dst = self.pop_value() as usize;
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let end = dst
                        .checked_add(len)
                        .filter(|end| *end <= memory.bytes().len())
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    memory.bytes_mut()[dst..end].fill(value);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::TableInit => {
                    let len = self.pop_value() as usize;
                    let src_offset = self.pop_value() as usize;
                    let dst_offset = self.pop_value() as usize;
                    let elements = interpreter
                        .module
                        .element_instances
                        .get(op.u1 as usize)
                        .and_then(|segment| segment.as_deref())
                        .unwrap_or(&[]);
                    let src_end = src_offset
                        .checked_add(len)
                        .filter(|end| *end <= elements.len())
                        .ok_or_else(|| Trap::new("invalid table access"))?;
                    let table = interpreter
                        .module
                        .tables
                        .get(op.u2 as usize)
                        .ok_or_else(|| Trap::new(format!("table[{}] is undefined", op.u2)))?;
                    let dst_end = dst_offset
                        .checked_add(len)
                        .filter(|end| *end <= table.len())
                        .ok_or_else(|| Trap::new("invalid table access"))?;
                    let _ = dst_end;
                    if len != 0 {
                        table.write_range(dst_offset, &elements[src_offset..src_end]);
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::ElemDrop => {
                    if let Some(elements) =
                        interpreter.module.element_instances.get_mut(op.u1 as usize)
                    {
                        *elements = None;
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::TableCopy => {
                    let len = self.pop_value() as usize;
                    let src_offset = self.pop_value() as usize;
                    let dst_offset = self.pop_value() as usize;
                    let src_index = op.u1 as usize;
                    let dst_index = op.u2 as usize;
                    let src_table = interpreter
                        .module
                        .tables
                        .get(src_index)
                        .ok_or_else(|| Trap::new(format!("table[{src_index}] is undefined")))?;
                    let src_end = src_offset
                        .checked_add(len)
                        .filter(|end| *end <= src_table.len())
                        .ok_or_else(|| Trap::new("invalid table access"))?;
                    let dst_table = interpreter
                        .module
                        .tables
                        .get(dst_index)
                        .ok_or_else(|| Trap::new(format!("table[{dst_index}] is undefined")))?;
                    let dst_end = dst_offset
                        .checked_add(len)
                        .filter(|end| *end <= dst_table.len())
                        .ok_or_else(|| Trap::new("invalid table access"))?;
                    let _ = (src_end, dst_end);
                    if len != 0 {
                        if src_index == dst_index {
                            dst_table.copy_within(src_offset, dst_offset, len);
                        } else {
                            let source = src_table.elements();
                            dst_table.write_range(dst_offset, &source[src_offset..src_end]);
                        }
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::RefFunc => {
                    self.push_value(op.u1 + 1);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::TableGet => {
                    let offset = self.pop_value() as usize;
                    let table = interpreter
                        .module
                        .tables
                        .get(op.u1 as usize)
                        .ok_or_else(|| Trap::new(format!("table[{}] is undefined", op.u1)))?;
                    let value = table
                        .get(offset)
                        .ok_or_else(|| Trap::new("invalid table access"))?;
                    self.push_value(table.stack_value(value));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::TableSet => {
                    let value = self.pop_value();
                    let offset = self.pop_value() as usize;
                    let table = interpreter
                        .module
                        .tables
                        .get(op.u1 as usize)
                        .ok_or_else(|| Trap::new(format!("table[{}] is undefined", op.u1)))?;
                    if offset >= table.len() {
                        return Err(Trap::new("invalid table access"));
                    }
                    table.write_range(offset, &[table.reference_from_stack(value)]);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::TableSize => {
                    let table = interpreter
                        .module
                        .tables
                        .get(op.u1 as usize)
                        .ok_or_else(|| Trap::new(format!("table[{}] is undefined", op.u1)))?;
                    self.push_value(table.len() as u64);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::TableGrow => {
                    let delta = self.pop_value() as u32;
                    let value = self.pop_value();
                    let table = interpreter
                        .module
                        .tables
                        .get(op.u1 as usize)
                        .ok_or_else(|| Trap::new(format!("table[{}] is undefined", op.u1)))?;
                    self.push_value(u64::from(
                        table.grow(delta, table.reference_from_stack(value)),
                    ));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::TableFill => {
                    let len = self.pop_value() as usize;
                    let value = self.pop_value();
                    let offset = self.pop_value() as usize;
                    let table = interpreter
                        .module
                        .tables
                        .get(op.u1 as usize)
                        .ok_or_else(|| Trap::new(format!("table[{}] is undefined", op.u1)))?;
                    let end = offset
                        .checked_add(len)
                        .filter(|end| *end <= table.len())
                        .ok_or_else(|| Trap::new("invalid table access"))?;
                    let _ = end;
                    if len != 0 {
                        table.fill(offset, len, table.reference_from_stack(value));
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicMemoryWait => {
                    let _timeout = self.pop_value() as i64;
                    let expected = self.pop_value();
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_ref()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    if !memory.shared {
                        return Err(Trap::new("expected shared memory"));
                    }
                    let result = match decode_unsigned_type(op.b1) {
                        UnsignedType::I32 => {
                            ensure_atomic_alignment(offset, 4)?;
                            let actual = memory
                                .read_u32_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            if actual == expected as u32 {
                                2
                            } else {
                                1
                            }
                        }
                        UnsignedType::I64 => {
                            ensure_atomic_alignment(offset, 8)?;
                            let actual = memory
                                .read_u64_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            if actual == expected {
                                2
                            } else {
                                1
                            }
                        }
                        _ => return Err(Trap::new("unsupported atomic wait type")),
                    };
                    self.push_value(result);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicMemoryNotify => {
                    let _count = self.pop_value();
                    let offset = self.pop_memory_offset(&op)?;
                    ensure_atomic_alignment(offset, 4)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_ref()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    if offset >= memory.bytes().len() as u32 {
                        return Err(Trap::new("out of bounds memory access"));
                    }
                    self.push_value(0);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicFence => {
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicLoad => {
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_ref()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let value = match decode_unsigned_type(op.b1) {
                        UnsignedType::I32 => {
                            ensure_atomic_alignment(offset, 4)?;
                            u64::from(
                                memory
                                    .read_u32_le(offset)
                                    .ok_or_else(|| Trap::new("out of bounds memory access"))?,
                            )
                        }
                        UnsignedType::I64 => {
                            ensure_atomic_alignment(offset, 8)?;
                            memory
                                .read_u64_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?
                        }
                        _ => return Err(Trap::new("unsupported atomic load type")),
                    };
                    self.push_value(value);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicLoad8 => {
                    let offset = self.pop_memory_offset(&op)?;
                    let value = interpreter
                        .module
                        .memory
                        .as_ref()
                        .ok_or_else(|| Trap::new("memory is undefined"))?
                        .read_byte(offset)
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    self.push_value(u64::from(value));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicLoad16 => {
                    let offset = self.pop_memory_offset(&op)?;
                    ensure_atomic_alignment(offset, 2)?;
                    let value = interpreter
                        .module
                        .memory
                        .as_ref()
                        .ok_or_else(|| Trap::new("memory is undefined"))?
                        .read_u16_le(offset)
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    self.push_value(u64::from(value));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicStore => {
                    let value = self.pop_value();
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let ok = match decode_unsigned_type(op.b1) {
                        UnsignedType::I32 => {
                            ensure_atomic_alignment(offset, 4)?;
                            memory.write_u32_le(offset, value as u32)
                        }
                        UnsignedType::I64 => {
                            ensure_atomic_alignment(offset, 8)?;
                            memory.write_u64_le(offset, value)
                        }
                        _ => return Err(Trap::new("unsupported atomic store type")),
                    };
                    if !ok {
                        return Err(Trap::new("out of bounds memory access"));
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicStore8 => {
                    let value = self.pop_value() as u8;
                    let offset = self.pop_memory_offset(&op)?;
                    if !interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?
                        .write_byte(offset, value)
                    {
                        return Err(Trap::new("out of bounds memory access"));
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicStore16 => {
                    let value = self.pop_value() as u16;
                    let offset = self.pop_memory_offset(&op)?;
                    ensure_atomic_alignment(offset, 2)?;
                    if !interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?
                        .write_u16_le(offset, value)
                    {
                        return Err(Trap::new("out of bounds memory access"));
                    }
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicRMW => {
                    let value = self.pop_value();
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let result = match decode_unsigned_type(op.b1) {
                        UnsignedType::I32 => {
                            ensure_atomic_alignment(offset, 4)?;
                            let old = memory
                                .read_u32_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            let new = apply_atomic_rmw_u32(old, value as u32, op.b2);
                            memory.write_u32_le(offset, new);
                            u64::from(old)
                        }
                        UnsignedType::I64 => {
                            ensure_atomic_alignment(offset, 8)?;
                            let old = memory
                                .read_u64_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            let new = apply_atomic_rmw_u64(old, value, op.b2);
                            memory.write_u64_le(offset, new);
                            old
                        }
                        _ => return Err(Trap::new("unsupported atomic rmw type")),
                    };
                    self.push_value(result);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicRMW8 => {
                    let value = self.pop_value() as u8;
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let old = memory
                        .read_byte(offset)
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    let new = apply_atomic_rmw_u8(old, value, op.b2);
                    memory.write_byte(offset, new);
                    self.push_value(u64::from(old));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicRMW16 => {
                    let value = self.pop_value() as u16;
                    let offset = self.pop_memory_offset(&op)?;
                    ensure_atomic_alignment(offset, 2)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let old = memory
                        .read_u16_le(offset)
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    let new = apply_atomic_rmw_u16(old, value, op.b2);
                    memory.write_u16_le(offset, new);
                    self.push_value(u64::from(old));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicRMWCmpxchg => {
                    let replacement = self.pop_value();
                    let expected = self.pop_value();
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let result = match decode_unsigned_type(op.b1) {
                        UnsignedType::I32 => {
                            ensure_atomic_alignment(offset, 4)?;
                            let old = memory
                                .read_u32_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            if old == expected as u32 {
                                memory.write_u32_le(offset, replacement as u32);
                            }
                            u64::from(old)
                        }
                        UnsignedType::I64 => {
                            ensure_atomic_alignment(offset, 8)?;
                            let old = memory
                                .read_u64_le(offset)
                                .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                            if old == expected {
                                memory.write_u64_le(offset, replacement);
                            }
                            old
                        }
                        _ => return Err(Trap::new("unsupported atomic cmpxchg type")),
                    };
                    self.push_value(result);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicRMW8Cmpxchg => {
                    let replacement = self.pop_value() as u8;
                    let expected = self.pop_value() as u8;
                    let offset = self.pop_memory_offset(&op)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let old = memory
                        .read_byte(offset)
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    if old == expected {
                        memory.write_byte(offset, replacement);
                    }
                    self.push_value(u64::from(old));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::AtomicRMW16Cmpxchg => {
                    let replacement = self.pop_value() as u16;
                    let expected = self.pop_value() as u16;
                    let offset = self.pop_memory_offset(&op)?;
                    ensure_atomic_alignment(offset, 2)?;
                    let memory = interpreter
                        .module
                        .memory
                        .as_mut()
                        .ok_or_else(|| Trap::new("memory is undefined"))?;
                    let old = memory
                        .read_u16_le(offset)
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    if old == expected {
                        memory.write_u16_le(offset, replacement);
                    }
                    self.push_value(u64::from(old));
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                _ => return Err(Trap::new(format!("operation {} not implemented", op.kind))),
            }
        }
        Ok(())
    }

    fn pop_memory_offset(&mut self, op: &Instruction) -> RuntimeResult<u32> {
        let offset = op
            .u2
            .checked_add(self.pop_value())
            .ok_or_else(|| Trap::new("out of bounds memory access"))?;
        if offset > u32::MAX as u64 {
            return Err(Trap::new("out of bounds memory access"));
        }
        Ok(offset as u32)
    }
}

fn resolve_labels(body: &mut [Instruction]) -> RuntimeResult<()> {
    let mut addresses = HashMap::<Label, usize>::new();
    for (index, op) in body.iter().enumerate() {
        if op.kind == OperationKind::Label {
            addresses.insert(Label::from_raw(op.u1), index);
        }
    }

    for op in body.iter_mut() {
        match op.kind {
            OperationKind::Br => resolve_label_address(&mut op.u1, &addresses)?,
            OperationKind::BrIf => {
                resolve_label_address(&mut op.u1, &addresses)?;
                resolve_label_address(&mut op.u2, &addresses)?;
            }
            OperationKind::BrTable => {
                for index in (0..op.us.len()).step_by(2) {
                    resolve_label_address(&mut op.us[index], &addresses)?;
                }
            }
            OperationKind::TailCallReturnCallIndirect => {
                if op.us.len() > 1 {
                    resolve_label_address(&mut op.us[1], &addresses)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn resolve_label_address(raw: &mut u64, addresses: &HashMap<Label, usize>) -> RuntimeResult<()> {
    let label = Label::from_raw(*raw);
    if label.is_return_target() {
        *raw = u64::MAX;
        return Ok(());
    }

    *raw = addresses
        .get(&label)
        .copied()
        .map(|index| index as u64)
        .ok_or_else(|| Trap::new(format!("unresolved label {label}")))?;
    Ok(())
}

fn decode_unsigned_type(raw: u8) -> UnsignedType {
    match raw {
        x if x == UnsignedType::I32 as u8 => UnsignedType::I32,
        x if x == UnsignedType::I64 as u8 => UnsignedType::I64,
        x if x == UnsignedType::F32 as u8 => UnsignedType::F32,
        x if x == UnsignedType::F64 as u8 => UnsignedType::F64,
        x if x == UnsignedType::V128 as u8 => UnsignedType::V128,
        _ => UnsignedType::Unknown,
    }
}

fn decode_signed_int(raw: u8) -> SignedInt {
    match raw {
        x if x == SignedInt::Int32 as u8 => SignedInt::Int32,
        x if x == SignedInt::Int64 as u8 => SignedInt::Int64,
        x if x == SignedInt::Uint32 as u8 => SignedInt::Uint32,
        _ => SignedInt::Uint64,
    }
}

fn decode_unsigned_int(raw: u8) -> UnsignedInt {
    match raw {
        x if x == UnsignedInt::I32 as u8 => UnsignedInt::I32,
        _ => UnsignedInt::I64,
    }
}

fn decode_signed_type(raw: u8) -> SignedType {
    match raw {
        x if x == SignedType::Int32 as u8 => SignedType::Int32,
        x if x == SignedType::Uint32 as u8 => SignedType::Uint32,
        x if x == SignedType::Int64 as u8 => SignedType::Int64,
        x if x == SignedType::Uint64 as u8 => SignedType::Uint64,
        x if x == SignedType::Float32 as u8 => SignedType::Float32,
        _ => SignedType::Float64,
    }
}

fn decode_float_kind(raw: u8) -> FloatKind {
    match raw {
        x if x == FloatKind::F32 as u8 => FloatKind::F32,
        _ => FloatKind::F64,
    }
}

fn ensure_atomic_alignment(offset: u32, alignment: u32) -> RuntimeResult<()> {
    if offset % alignment == 0 {
        Ok(())
    } else {
        Err(Trap::new("unaligned atomic"))
    }
}

fn apply_atomic_rmw_u8(old: u8, value: u8, raw_op: u8) -> u8 {
    match raw_op {
        x if x == AtomicArithmeticOp::Add as u8 => old.wrapping_add(value),
        x if x == AtomicArithmeticOp::Sub as u8 => old.wrapping_sub(value),
        x if x == AtomicArithmeticOp::And as u8 => old & value,
        x if x == AtomicArithmeticOp::Or as u8 => old | value,
        x if x == AtomicArithmeticOp::Xor as u8 => old ^ value,
        _ => value,
    }
}

fn apply_atomic_rmw_u16(old: u16, value: u16, raw_op: u8) -> u16 {
    match raw_op {
        x if x == AtomicArithmeticOp::Add as u8 => old.wrapping_add(value),
        x if x == AtomicArithmeticOp::Sub as u8 => old.wrapping_sub(value),
        x if x == AtomicArithmeticOp::And as u8 => old & value,
        x if x == AtomicArithmeticOp::Or as u8 => old | value,
        x if x == AtomicArithmeticOp::Xor as u8 => old ^ value,
        _ => value,
    }
}

fn apply_atomic_rmw_u32(old: u32, value: u32, raw_op: u8) -> u32 {
    match raw_op {
        x if x == AtomicArithmeticOp::Add as u8 => old.wrapping_add(value),
        x if x == AtomicArithmeticOp::Sub as u8 => old.wrapping_sub(value),
        x if x == AtomicArithmeticOp::And as u8 => old & value,
        x if x == AtomicArithmeticOp::Or as u8 => old | value,
        x if x == AtomicArithmeticOp::Xor as u8 => old ^ value,
        _ => value,
    }
}

fn apply_atomic_rmw_u64(old: u64, value: u64, raw_op: u8) -> u64 {
    match raw_op {
        x if x == AtomicArithmeticOp::Add as u8 => old.wrapping_add(value),
        x if x == AtomicArithmeticOp::Sub as u8 => old.wrapping_sub(value),
        x if x == AtomicArithmeticOp::And as u8 => old & value,
        x if x == AtomicArithmeticOp::Or as u8 => old | value,
        x if x == AtomicArithmeticOp::Xor as u8 => old ^ value,
        _ => value,
    }
}

fn compare_i32(v1: i32, v2: i32, kind: OperationKind) -> bool {
    match kind {
        OperationKind::Lt => v1 < v2,
        OperationKind::Gt => v1 > v2,
        OperationKind::Le => v1 <= v2,
        OperationKind::Ge => v1 >= v2,
        _ => false,
    }
}

fn compare_u32(v1: u32, v2: u32, kind: OperationKind) -> bool {
    match kind {
        OperationKind::Lt => v1 < v2,
        OperationKind::Gt => v1 > v2,
        OperationKind::Le => v1 <= v2,
        OperationKind::Ge => v1 >= v2,
        _ => false,
    }
}

fn compare_i64(v1: i64, v2: i64, kind: OperationKind) -> bool {
    match kind {
        OperationKind::Lt => v1 < v2,
        OperationKind::Gt => v1 > v2,
        OperationKind::Le => v1 <= v2,
        OperationKind::Ge => v1 >= v2,
        _ => false,
    }
}

fn compare_u64(v1: u64, v2: u64, kind: OperationKind) -> bool {
    match kind {
        OperationKind::Lt => v1 < v2,
        OperationKind::Gt => v1 > v2,
        OperationKind::Le => v1 <= v2,
        OperationKind::Ge => v1 >= v2,
        _ => false,
    }
}

fn compare_f32(v1: f32, v2: f32, kind: OperationKind) -> bool {
    match kind {
        OperationKind::Lt => v1 < v2,
        OperationKind::Gt => v1 > v2,
        OperationKind::Le => v1 <= v2,
        OperationKind::Ge => v1 >= v2,
        _ => false,
    }
}

fn compare_f64(v1: f64, v2: f64, kind: OperationKind) -> bool {
    match kind {
        OperationKind::Lt => v1 < v2,
        OperationKind::Gt => v1 > v2,
        OperationKind::Le => v1 <= v2,
        OperationKind::Ge => v1 >= v2,
        _ => false,
    }
}

fn execute_binary_numeric(
    kind: OperationKind,
    ty: UnsignedType,
    v1: u64,
    v2: u64,
) -> RuntimeResult<u64> {
    Ok(match ty {
        UnsignedType::I32 => {
            let v1 = v1 as u32;
            let v2 = v2 as u32;
            match kind {
                OperationKind::Add => u64::from(v1.wrapping_add(v2)),
                OperationKind::Sub => u64::from(v1.wrapping_sub(v2)),
                OperationKind::Mul => u64::from(v1.wrapping_mul(v2)),
                _ => unreachable!(),
            }
        }
        UnsignedType::I64 => match kind {
            OperationKind::Add => v1.wrapping_add(v2),
            OperationKind::Sub => v1.wrapping_sub(v2),
            OperationKind::Mul => v1.wrapping_mul(v2),
            _ => unreachable!(),
        },
        UnsignedType::F32 => {
            let v1 = f32::from_bits(v1 as u32);
            let v2 = f32::from_bits(v2 as u32);
            u64::from(
                match kind {
                    OperationKind::Add => v1 + v2,
                    OperationKind::Sub => v1 - v2,
                    OperationKind::Mul => v1 * v2,
                    _ => unreachable!(),
                }
                .to_bits(),
            )
        }
        UnsignedType::F64 => match kind {
            OperationKind::Add => (f64::from_bits(v1) + f64::from_bits(v2)).to_bits(),
            OperationKind::Sub => (f64::from_bits(v1) - f64::from_bits(v2)).to_bits(),
            OperationKind::Mul => (f64::from_bits(v1) * f64::from_bits(v2)).to_bits(),
            _ => unreachable!(),
        },
        _ => return Err(Trap::new("unsupported numeric type")),
    })
}

fn execute_div(ty: SignedType, v1: u64, v2: u64) -> RuntimeResult<u64> {
    match ty {
        SignedType::Int32 => {
            let lhs = v1 as u32 as i32;
            let rhs = v2 as u32 as i32;
            if rhs == 0 {
                return Err(Trap::new("integer divide by zero"));
            }
            lhs.checked_div(rhs)
                .map(|value| u64::from(value as u32))
                .ok_or_else(|| Trap::new("integer overflow"))
        }
        SignedType::Uint32 => {
            let rhs = v2 as u32;
            if rhs == 0 {
                return Err(Trap::new("integer divide by zero"));
            }
            Ok(u64::from((v1 as u32) / rhs))
        }
        SignedType::Int64 => {
            let lhs = v1 as i64;
            let rhs = v2 as i64;
            if rhs == 0 {
                return Err(Trap::new("integer divide by zero"));
            }
            lhs.checked_div(rhs)
                .map(|value| value as u64)
                .ok_or_else(|| Trap::new("integer overflow"))
        }
        SignedType::Uint64 => {
            if v2 == 0 {
                return Err(Trap::new("integer divide by zero"));
            }
            Ok(v1 / v2)
        }
        SignedType::Float32 => Ok(u64::from(
            (f32::from_bits(v1 as u32) / f32::from_bits(v2 as u32)).to_bits(),
        )),
        SignedType::Float64 => Ok((f64::from_bits(v1) / f64::from_bits(v2)).to_bits()),
    }
}

fn execute_rem(ty: SignedType, v1: u64, v2: u64) -> RuntimeResult<u64> {
    match ty {
        SignedType::Int32 => {
            let lhs = v1 as u32 as i32;
            let rhs = v2 as u32 as i32;
            if rhs == 0 {
                return Err(Trap::new("integer divide by zero"));
            }
            if lhs == i32::MIN && rhs == -1 {
                return Ok(0);
            }
            Ok(u64::from((lhs % rhs) as u32))
        }
        SignedType::Uint32 => {
            let rhs = v2 as u32;
            if rhs == 0 {
                return Err(Trap::new("integer divide by zero"));
            }
            Ok(u64::from((v1 as u32) % rhs))
        }
        SignedType::Int64 => {
            let lhs = v1 as i64;
            let rhs = v2 as i64;
            if rhs == 0 {
                return Err(Trap::new("integer divide by zero"));
            }
            if lhs == i64::MIN && rhs == -1 {
                return Ok(0);
            }
            Ok((lhs % rhs) as u64)
        }
        SignedType::Uint64 => {
            if v2 == 0 {
                return Err(Trap::new("integer divide by zero"));
            }
            Ok(v1 % v2)
        }
        SignedType::Float32 | SignedType::Float64 => Err(Trap::new("unsupported remainder type")),
    }
}

fn execute_shift(kind: OperationKind, raw_ty: u8, v1: u64, v2: u64) -> u64 {
    match kind {
        OperationKind::Shr => match decode_signed_type(raw_ty) {
            SignedType::Int32 | SignedType::Uint32 => {
                let rhs = (v2 as u32) & 31;
                let lhs = v1 as u32;
                if matches!(decode_signed_type(raw_ty), SignedType::Int32) {
                    u64::from(((lhs as i32) >> rhs) as u32)
                } else {
                    u64::from(lhs >> rhs)
                }
            }
            SignedType::Int64 | SignedType::Uint64 => {
                let rhs = (v2 as u32) & 63;
                if matches!(decode_signed_type(raw_ty), SignedType::Int64) {
                    ((v1 as i64) >> rhs) as u64
                } else {
                    v1 >> rhs
                }
            }
            SignedType::Float32 | SignedType::Float64 => unreachable!("shift ops are integer-only"),
        },
        OperationKind::Shl | OperationKind::Rotl | OperationKind::Rotr => {
            match decode_unsigned_int(raw_ty) {
                UnsignedInt::I32 => {
                    let rhs = (v2 as u32) & 31;
                    let lhs = v1 as u32;
                    match kind {
                        OperationKind::Shl => u64::from(lhs.wrapping_shl(rhs)),
                        OperationKind::Rotl => u64::from(lhs.rotate_left(rhs)),
                        OperationKind::Rotr => u64::from(lhs.rotate_right(rhs)),
                        _ => unreachable!(),
                    }
                }
                UnsignedInt::I64 => {
                    let rhs = (v2 as u32) & 63;
                    match kind {
                        OperationKind::Shl => v1.wrapping_shl(rhs),
                        OperationKind::Rotl => v1.rotate_left(rhs),
                        OperationKind::Rotr => v1.rotate_right(rhs),
                        _ => unreachable!(),
                    }
                }
            }
        }
        _ => 0,
    }
}

fn execute_unary_float(kind: OperationKind, float_kind: FloatKind, value: u64) -> u64 {
    match float_kind {
        FloatKind::F32 => {
            let bits = value as u32;
            if matches!(
                kind,
                OperationKind::Ceil
                    | OperationKind::Floor
                    | OperationKind::Trunc
                    | OperationKind::Nearest
                    | OperationKind::Sqrt
            ) && is_f32_nan_bits(bits)
            {
                return u64::from(quiet_nan_f32_bits(bits));
            }
            let value = f32::from_bits(value as u32);
            u64::from(
                match kind {
                    OperationKind::Abs => value.abs(),
                    OperationKind::Neg => -value,
                    OperationKind::Ceil => value.ceil(),
                    OperationKind::Floor => value.floor(),
                    OperationKind::Trunc => value.trunc(),
                    OperationKind::Nearest => nearest_f32(value),
                    OperationKind::Sqrt => value.sqrt(),
                    _ => unreachable!(),
                }
                .to_bits(),
            )
        }
        FloatKind::F64 => {
            if matches!(
                kind,
                OperationKind::Ceil
                    | OperationKind::Floor
                    | OperationKind::Trunc
                    | OperationKind::Nearest
                    | OperationKind::Sqrt
            ) && is_f64_nan_bits(value)
            {
                return quiet_nan_f64_bits(value);
            }
            match kind {
                OperationKind::Abs => f64::from_bits(value).abs().to_bits(),
                OperationKind::Neg => (-f64::from_bits(value)).to_bits(),
                OperationKind::Ceil => f64::from_bits(value).ceil().to_bits(),
                OperationKind::Floor => f64::from_bits(value).floor().to_bits(),
                OperationKind::Trunc => f64::from_bits(value).trunc().to_bits(),
                OperationKind::Nearest => nearest_f64(f64::from_bits(value)).to_bits(),
                OperationKind::Sqrt => f64::from_bits(value).sqrt().to_bits(),
                _ => unreachable!(),
            }
        }
    }
}

fn is_f32_nan_bits(bits: u32) -> bool {
    bits & 0x7f80_0000 == 0x7f80_0000 && bits & 0x007f_ffff != 0
}

fn quiet_nan_f32_bits(bits: u32) -> u32 {
    bits | 0x0040_0000
}

fn is_f64_nan_bits(bits: u64) -> bool {
    bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && bits & 0x000f_ffff_ffff_ffff != 0
}

fn quiet_nan_f64_bits(bits: u64) -> u64 {
    bits | 0x0008_0000_0000_0000
}

fn execute_binary_float(kind: OperationKind, float_kind: FloatKind, v1: u64, v2: u64) -> u64 {
    match float_kind {
        FloatKind::F32 => {
            let lhs = f32::from_bits(v1 as u32);
            let rhs = f32::from_bits(v2 as u32);
            u64::from(
                match kind {
                    OperationKind::Min => wasm_min_f32(lhs, rhs),
                    OperationKind::Max => wasm_max_f32(lhs, rhs),
                    OperationKind::Copysign => lhs.copysign(rhs),
                    _ => unreachable!(),
                }
                .to_bits(),
            )
        }
        FloatKind::F64 => {
            let lhs = f64::from_bits(v1);
            let rhs = f64::from_bits(v2);
            match kind {
                OperationKind::Min => wasm_min_f64(lhs, rhs).to_bits(),
                OperationKind::Max => wasm_max_f64(lhs, rhs).to_bits(),
                OperationKind::Copysign => lhs.copysign(rhs).to_bits(),
                _ => unreachable!(),
            }
        }
    }
}

fn execute_trunc_from_float(
    input_type: FloatKind,
    output_type: SignedInt,
    non_trapping: bool,
    value: u64,
) -> RuntimeResult<u64> {
    match input_type {
        FloatKind::F32 => trunc_from_f64(
            f32::from_bits(value as u32) as f64,
            output_type,
            non_trapping,
        ),
        FloatKind::F64 => trunc_from_f64(f64::from_bits(value), output_type, non_trapping),
    }
}

fn execute_float_convert(input_type: SignedInt, output_type: FloatKind, value: u64) -> u64 {
    match output_type {
        FloatKind::F32 => {
            let converted = match input_type {
                SignedInt::Int32 => value as u32 as i32 as f32,
                SignedInt::Int64 => value as i64 as f32,
                SignedInt::Uint32 => value as u32 as f32,
                SignedInt::Uint64 => value as f32,
            };
            u64::from(converted.to_bits())
        }
        FloatKind::F64 => match input_type {
            SignedInt::Int32 => (value as u32 as i32 as f64).to_bits(),
            SignedInt::Int64 => (value as i64 as f64).to_bits(),
            SignedInt::Uint32 => (value as u32 as f64).to_bits(),
            SignedInt::Uint64 => (value as f64).to_bits(),
        },
    }
}

fn trunc_from_f64(value: f64, output_type: SignedInt, non_trapping: bool) -> RuntimeResult<u64> {
    if value.is_nan() {
        return if non_trapping {
            Ok(0)
        } else {
            Err(Trap::new("invalid conversion to integer"))
        };
    }

    let truncated = value.trunc();
    match output_type {
        SignedInt::Int32 => trunc_signed(truncated, i32::MIN as f64, i32::MAX as f64, non_trapping)
            .map(|value| u64::from((value as i32) as u32)),
        SignedInt::Int64 => {
            trunc_signed_i64(truncated, non_trapping).map(|value| value as i64 as u64)
        }
        SignedInt::Uint32 => trunc_unsigned(truncated, u32::MAX as f64, non_trapping)
            .map(|value| u64::from(value as u32)),
        SignedInt::Uint64 => trunc_unsigned_u64(truncated, non_trapping),
    }
}

fn trunc_signed(value: f64, min: f64, max: f64, non_trapping: bool) -> RuntimeResult<i128> {
    if value < min {
        return if non_trapping {
            Ok(min as i128)
        } else {
            Err(Trap::new("integer overflow"))
        };
    }
    if value > max {
        return if non_trapping {
            Ok(max as i128)
        } else {
            Err(Trap::new("integer overflow"))
        };
    }
    Ok(value as i128)
}

fn trunc_unsigned(value: f64, max: f64, non_trapping: bool) -> RuntimeResult<u64> {
    if value <= -1.0 {
        return if non_trapping {
            Ok(0)
        } else {
            Err(Trap::new("integer overflow"))
        };
    }
    if value > max {
        return if non_trapping {
            Ok(u64::MAX)
        } else {
            Err(Trap::new("integer overflow"))
        };
    }
    Ok(value as u64)
}

fn trunc_signed_i64(value: f64, non_trapping: bool) -> RuntimeResult<i128> {
    const I64_MIN_F64: f64 = -9_223_372_036_854_775_808.0;
    const I64_UPPER_BOUND_F64: f64 = 9_223_372_036_854_775_808.0;
    if value < I64_MIN_F64 || value >= I64_UPPER_BOUND_F64 {
        return if non_trapping {
            Ok(if value < I64_MIN_F64 {
                i64::MIN as i128
            } else {
                i64::MAX as i128
            })
        } else {
            Err(Trap::new("integer overflow"))
        };
    }
    Ok(value as i128)
}

fn trunc_unsigned_u64(value: f64, non_trapping: bool) -> RuntimeResult<u64> {
    const U64_UPPER_BOUND_F64: f64 = 18_446_744_073_709_551_616.0;
    if value <= -1.0 || value >= U64_UPPER_BOUND_F64 {
        return if non_trapping {
            Ok(if value <= -1.0 { 0 } else { u64::MAX })
        } else {
            Err(Trap::new("integer overflow"))
        };
    }
    Ok(value as u64)
}

fn nearest_f32(value: f32) -> f32 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }
    let lower = value.floor();
    let upper = value.ceil();
    let lower_diff = (value - lower).abs();
    let upper_diff = (upper - value).abs();
    if lower_diff < upper_diff {
        lower
    } else if upper_diff < lower_diff {
        upper
    } else if (lower as i64) % 2 == 0 {
        lower
    } else {
        upper
    }
}

fn nearest_f64(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }
    let lower = value.floor();
    let upper = value.ceil();
    let lower_diff = (value - lower).abs();
    let upper_diff = (upper - value).abs();
    if lower_diff < upper_diff {
        lower
    } else if upper_diff < lower_diff {
        upper
    } else if (lower as i128) % 2 == 0 {
        lower
    } else {
        upper
    }
}

fn wasm_min_f32(lhs: f32, rhs: f32) -> f32 {
    if lhs.is_nan() || rhs.is_nan() {
        return f32::NAN;
    }
    if lhs == rhs && lhs == 0.0 {
        if lhs.is_sign_negative() || rhs.is_sign_negative() {
            -0.0
        } else {
            0.0
        }
    } else {
        lhs.min(rhs)
    }
}

fn wasm_max_f32(lhs: f32, rhs: f32) -> f32 {
    if lhs.is_nan() || rhs.is_nan() {
        return f32::NAN;
    }
    if lhs == rhs && lhs == 0.0 {
        if lhs.is_sign_positive() || rhs.is_sign_positive() {
            0.0
        } else {
            -0.0
        }
    } else {
        lhs.max(rhs)
    }
}

fn wasm_min_f64(lhs: f64, rhs: f64) -> f64 {
    if lhs.is_nan() || rhs.is_nan() {
        return f64::NAN;
    }
    if lhs == rhs && lhs == 0.0 {
        if lhs.is_sign_negative() || rhs.is_sign_negative() {
            -0.0
        } else {
            0.0
        }
    } else {
        lhs.min(rhs)
    }
}

fn wasm_max_f64(lhs: f64, rhs: f64) -> f64 {
    if lhs.is_nan() || rhs.is_nan() {
        return f64::NAN;
    }
    if lhs == rhs && lhs == 0.0 {
        if lhs.is_sign_positive() || rhs.is_sign_positive() {
            0.0
        } else {
            -0.0
        }
    } else {
        lhs.max(rhs)
    }
}

fn mask32(value: bool) -> u32 {
    if value {
        u32::MAX
    } else {
        0
    }
}

fn mask8(value: bool) -> u8 {
    if value {
        u8::MAX
    } else {
        0
    }
}

fn mask16(value: bool) -> u16 {
    if value {
        u16::MAX
    } else {
        0
    }
}

fn byte_at(value: u64, index: usize) -> u8 {
    ((value >> (index * 8)) & 0xff) as u8
}

fn half_at(value: u64, index: usize) -> u16 {
    ((value >> (index * 16)) & 0xffff) as u16
}

fn mask64(value: bool) -> u64 {
    if value {
        u64::MAX
    } else {
        0
    }
}

fn f32_lt(lhs: f32, rhs: f32) -> bool {
    if lhs.is_nan() || rhs.is_nan() || lhs == rhs {
        return false;
    }
    if lhs == f32::INFINITY {
        return false;
    }
    if lhs == f32::NEG_INFINITY {
        return true;
    }
    if rhs == f32::INFINITY {
        return true;
    }
    if rhs == f32::NEG_INFINITY {
        return false;
    }
    lhs < rhs
}

fn f64_lt(lhs: f64, rhs: f64) -> bool {
    if lhs.is_nan() || rhs.is_nan() || lhs == rhs {
        return false;
    }
    if lhs == f64::INFINITY {
        return false;
    }
    if lhs == f64::NEG_INFINITY {
        return true;
    }
    if rhs == f64::INFINITY {
        return true;
    }
    if rhs == f64::NEG_INFINITY {
        return false;
    }
    lhs < rhs
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    };

    use super::{
        host_function, with_active_fuel_remaining, CallEngine, CallFrame, Function, GlobalValue,
        Interpreter, Memory, Module, Table, Trap, WASM_PAGE_SIZE,
    };
    use crate::compiler::{CompileConfig, Compiler, FunctionType, ValueType};
    use crate::operations::{
        AtomicArithmeticOp, FloatKind, InclusiveRange, Instruction, Label, LabelKind, MemoryArg,
        OperationKind, UnsignedType,
    };
    use crate::signature::Signature;
    use razero_decoder::decoder::decode_module;
    use razero_features::CoreFeatures;
    use razero_wasm::module::ExternType;

    fn label(kind: LabelKind, frame_id: u32) -> Label {
        Label::new(kind, frame_id)
    }

    fn i32_i32() -> FunctionType {
        FunctionType::new(vec![ValueType::I32], vec![ValueType::I32])
    }

    fn interp_value_type(value_type: razero_wasm::module::ValueType) -> ValueType {
        match value_type.0 {
            0x7f => ValueType::I32,
            0x7e => ValueType::I64,
            0x7d => ValueType::F32,
            0x7c => ValueType::F64,
            0x7b => ValueType::V128,
            0x70 => ValueType::FuncRef,
            0x6f => ValueType::ExternRef,
            other => panic!("unsupported value type 0x{other:x}"),
        }
    }

    fn fac_interpreter() -> (Interpreter, usize, usize, usize) {
        let module = decode_module(include_bytes!("../../testdata/fac.wasm"), CoreFeatures::V2)
            .expect("fac.wasm should decode");
        let types = module
            .type_section
            .iter()
            .map(|ty| {
                FunctionType::new(
                    ty.params.iter().map(|v| interp_value_type(*v)).collect(),
                    ty.results.iter().map(|v| interp_value_type(*v)).collect(),
                )
            })
            .collect::<Vec<_>>();
        let mut functions = Vec::with_capacity(module.function_section.len());
        for (local_index, type_index) in module.function_section.iter().copied().enumerate() {
            let wasm_ty = &module.type_section[type_index as usize];
            let code = &module.code_section[local_index];
            let signature = FunctionType::new(
                wasm_ty
                    .params
                    .iter()
                    .map(|ty| interp_value_type(*ty))
                    .collect(),
                wasm_ty
                    .results
                    .iter()
                    .map(|ty| interp_value_type(*ty))
                    .collect(),
            );
            let local_types = code
                .local_types
                .iter()
                .map(|ty| interp_value_type(*ty))
                .collect::<Vec<_>>();
            let lowered = Compiler
                .lower_with_config(CompileConfig {
                    body: &code.body,
                    signature: signature.clone(),
                    local_types: &local_types,
                    functions: &module.function_section,
                    types: &types,
                    ..CompileConfig::new(&[])
                })
                .expect("function should lower");
            functions.push(
                Function::new_native(Signature::from(&signature), lowered.operations)
                    .expect("lowered function should resolve labels"),
            );
        }
        let fac_index = module
            .export_section
            .iter()
            .find(|export| export.ty == ExternType::FUNC && export.name == "fac-ssa")
            .expect("fac-ssa export")
            .index as usize;
        (
            Interpreter::new(Module {
                functions,
                ..Module::default()
            }),
            0,
            1,
            fac_index,
        )
    }

    #[test]
    fn peek_values_returns_stack_tail() {
        let mut engine = CallEngine::default();
        engine.stack = vec![5, 4, 3, 2, 1];

        assert!(engine.peek_values(0).is_empty());
        assert_eq!(&[2, 1], engine.peek_values(2));
    }

    #[test]
    fn push_frame_enforces_stack_ceiling() {
        let mut engine = CallEngine::default();

        engine
            .push_frame(
                CallFrame {
                    pc: 0,
                    function_index: 0,
                    base: 0,
                },
                1,
            )
            .unwrap();

        let err = engine
            .push_frame(
                CallFrame {
                    pc: 0,
                    function_index: 1,
                    base: 0,
                },
                1,
            )
            .unwrap_err();
        assert_eq!("stack overflow", err.message());
    }

    #[test]
    fn host_call_dispatch_uses_result_window() {
        let signature = Signature::new(vec![ValueType::I32, ValueType::I32], vec![ValueType::I32]);
        let mut interpreter = Interpreter::new(Module {
            globals: vec![GlobalValue::default()],
            functions: vec![Function::new_host(
                signature,
                host_function(|module, stack| {
                    module.globals[0].lo = stack[0] + stack[1];
                    stack[0] = module.globals[0].lo;
                    Ok(())
                }),
            )],
            ..Module::default()
        });

        let results = interpreter.call(0, &[20, 22]).unwrap();
        assert_eq!(vec![42], results);
        assert_eq!(42, interpreter.module.globals[0].lo);
    }

    #[test]
    fn executes_lowered_identity_function() {
        let ty = i32_i32();
        let lowered = Compiler
            .lower_with_config(CompileConfig {
                body: &[0x20, 0x00, 0x0b],
                signature: ty.clone(),
                ..CompileConfig::new(&[])
            })
            .unwrap();

        let mut interpreter = Interpreter::new(Module {
            functions: vec![
                Function::new_native(Signature::from(&ty), lowered.operations).unwrap(),
            ],
            ..Module::default()
        });

        assert_eq!(vec![7], interpreter.call(0, &[7]).unwrap());
    }

    #[test]
    fn native_function_dispatches_to_host_call() {
        let entry = Function::new_native(
            Signature::new(vec![ValueType::I32, ValueType::I32], vec![ValueType::I32]),
            vec![
                Instruction::call(1),
                Instruction::br(Label::new(LabelKind::Return, 0)),
            ],
        )
        .unwrap();
        let host = Function::new_host(
            Signature::new(vec![ValueType::I32, ValueType::I32], vec![ValueType::I32]),
            host_function(|_, stack| {
                stack[0] = stack[0].wrapping_add(stack[1]);
                Ok(())
            }),
        );
        let mut interpreter = Interpreter::new(Module {
            functions: vec![entry, host],
            ..Module::default()
        });

        assert_eq!(vec![42], interpreter.call(0, &[19, 23]).unwrap());
    }

    #[test]
    fn host_calls_do_not_consume_extra_fuel() {
        let entry = Function::new_native(
            Signature::new(vec![ValueType::I32], vec![ValueType::I32]),
            vec![
                Instruction::call(1),
                Instruction::br(Label::new(LabelKind::Return, 0)),
            ],
        )
        .unwrap();
        let host = Function::new_host(
            Signature::new(vec![ValueType::I32], vec![ValueType::I32]),
            host_function(|_, stack| {
                stack[0] = stack[0].wrapping_add(1);
                Ok(())
            }),
        );
        let mut interpreter = Interpreter::new(Module {
            functions: vec![entry, host],
            ..Module::default()
        });
        let fuel = Arc::new(AtomicI64::new(1));

        let results =
            with_active_fuel_remaining(Some(fuel.clone()), || interpreter.call(0, &[41])).unwrap();

        assert_eq!(vec![42], results);
        assert_eq!(0, fuel.load(Ordering::SeqCst));
    }

    #[test]
    fn backward_branch_exhausts_fuel() {
        let lowered = Compiler
            .lower_with_config(CompileConfig {
                body: &[0x03, 0x40, 0x0c, 0x00, 0x0b, 0x0b],
                signature: FunctionType::default(),
                ..CompileConfig::new(&[])
            })
            .expect("loop body should lower");
        let mut interpreter = Interpreter::new(Module {
            functions: vec![
                Function::new_native(Signature::default(), lowered.operations).unwrap(),
            ],
            ..Module::default()
        });
        let fuel = Arc::new(AtomicI64::new(1));

        let err = with_active_fuel_remaining(Some(fuel.clone()), || interpreter.call(0, &[]))
            .unwrap_err();

        assert_eq!("fuel exhausted", err.message());
        assert!(fuel.load(Ordering::SeqCst) < 0);
    }

    #[test]
    fn supports_indirect_host_calls() {
        let entry = Function::new_native(
            Signature::new(vec![ValueType::I32], vec![ValueType::I32]),
            vec![
                Instruction::const_i32(0),
                Instruction::call_indirect(0, 0),
                Instruction::br(Label::new(LabelKind::Return, 0)),
            ],
        )
        .unwrap();
        let host = Function::new_host(
            Signature::new(vec![ValueType::I32], vec![ValueType::I32]),
            host_function(|_, stack| {
                stack[0] = stack[0].wrapping_mul(2);
                Ok(())
            }),
        );
        let signature = Signature::new(vec![ValueType::I32], vec![ValueType::I32]);
        let mut interpreter = Interpreter::new(Module {
            functions: vec![entry, host],
            tables: vec![Table::from_elements(vec![Some(1)])],
            types: vec![signature],
            ..Module::default()
        });

        assert_eq!(vec![18], interpreter.call(0, &[9]).unwrap());
    }

    #[test]
    fn stores_and_loads_from_memory() {
        let function = Function::new_native(
            Signature::new(vec![], vec![ValueType::I32]),
            vec![
                Instruction::const_i32(0),
                Instruction::const_i32(42),
                Instruction::store(UnsignedType::I32, MemoryArg::default()),
                Instruction::const_i32(0),
                Instruction::load(UnsignedType::I32, MemoryArg::default()),
                Instruction::br(Label::new(LabelKind::Return, 0)),
            ],
        )
        .unwrap();
        let mut interpreter = Interpreter::new(Module {
            functions: vec![function],
            memory: Some(Memory::new(1, Some(2))),
            ..Module::default()
        });

        assert_eq!(vec![42], interpreter.call(0, &[]).unwrap());
    }

    #[test]
    fn saturating_float_to_int_conversion_matches_runtime_behavior() {
        let function = Function::new_native(
            Signature::new(vec![], vec![ValueType::I32]),
            vec![
                Instruction::const_f64(2_147_483_648.0),
                Instruction::i_trunc_from_f(
                    FloatKind::F64,
                    crate::operations::SignedInt::Int32,
                    true,
                ),
                Instruction::br(Label::new(LabelKind::Return, 0)),
            ],
        )
        .unwrap();
        let mut interpreter = Interpreter::new(Module {
            functions: vec![function],
            ..Module::default()
        });

        assert_eq!(
            vec![i32::MAX as u32 as u64],
            interpreter.call(0, &[]).unwrap()
        );
    }

    #[test]
    fn executes_compiler_emitted_control_flow() {
        let ty = i32_i32();
        let lowered = Compiler
            .lower_with_config(CompileConfig {
                body: &[
                    0x41, 0x01, 0x20, 0x00, 0x04, 0x00, 0x41, 0x02, 0x6a, 0x05, 0x41, 0x7e, 0x6b,
                    0x0b, 0x0b,
                ],
                signature: ty.clone(),
                types: &[ty.clone()],
                ..CompileConfig::new(&[])
            })
            .unwrap();
        let mut interpreter = Interpreter::new(Module {
            functions: vec![
                Function::new_native(Signature::from(&ty), lowered.operations).unwrap(),
            ],
            ..Module::default()
        });

        assert_eq!(vec![3], interpreter.call(0, &[9]).unwrap());
    }

    #[test]
    fn executes_multivalue_loop_with_inner_return() {
        let ty = FunctionType::new(vec![ValueType::I64, ValueType::I64], vec![ValueType::I64]);
        let lowered = Compiler
            .lower_with_config(CompileConfig {
                body: &[0x03, 0x00, 0x1a, 0x0f, 0x0b, 0x0b],
                signature: ty.clone(),
                types: &[ty.clone()],
                ..CompileConfig::new(&[])
            })
            .expect("loop body should lower");
        let mut interpreter = Interpreter::new(Module {
            functions: vec![
                Function::new_native(Signature::from(&ty), lowered.operations).unwrap(),
            ],
            ..Module::default()
        });

        assert_eq!(vec![7], interpreter.call(0, &[7, 99]).unwrap());
    }

    #[test]
    fn executes_fac_secbench_workload() {
        let (mut interpreter, pick0, pick1, fac) = fac_interpreter();
        assert_eq!(vec![7, 7], interpreter.call(pick0, &[7]).unwrap());
        assert_eq!(vec![7, 9, 7], interpreter.call(pick1, &[7, 9]).unwrap());
        assert_eq!(
            vec![2_432_902_008_176_640_000],
            interpreter.call(fac, &[20]).unwrap()
        );
    }

    #[test]
    fn traps_on_indirect_signature_mismatch() {
        let entry = Function::new_native(
            Signature::new(vec![ValueType::I32], vec![ValueType::I32]),
            vec![
                Instruction::const_i32(0),
                Instruction::call_indirect(0, 0),
                Instruction::br(Label::new(LabelKind::Return, 0)),
            ],
        )
        .unwrap();
        let host = Function::new_host(
            Signature::new(vec![ValueType::I64], vec![ValueType::I64]),
            host_function(|_, _| Ok(())),
        );
        let mut interpreter = Interpreter::new(Module {
            functions: vec![entry, host],
            tables: vec![Table::from_elements(vec![Some(1)])],
            types: vec![Signature::new(vec![ValueType::I32], vec![ValueType::I32])],
            ..Module::default()
        });

        let err = interpreter.call(0, &[1]).unwrap_err();
        assert_eq!("indirect call type mismatch", err.message());
    }

    #[test]
    fn drop_range_handles_vector_tail() {
        let function = Function::new_native(
            Signature::default(),
            vec![
                Instruction::v128_const(1, 2),
                Instruction::drop(InclusiveRange::new(0, 1)),
                Instruction::br(label(LabelKind::Return, 0)),
            ],
        )
        .unwrap();
        let mut interpreter = Interpreter::new(Module {
            functions: vec![function],
            ..Module::default()
        });

        assert!(interpreter.call(0, &[]).unwrap().is_empty());
    }

    #[test]
    fn executes_tail_call_return_call() {
        let callee = Function::new_native(
            Signature::new(vec![ValueType::I32], vec![ValueType::I32]),
            vec![
                Instruction::pick(0, false),
                Instruction::const_i32(1),
                Instruction::new(OperationKind::Add).with_b1(UnsignedType::I32 as u8),
                Instruction::br(label(LabelKind::Return, 0)),
            ],
        )
        .unwrap();
        let caller = Function::new_native(
            Signature::new(vec![ValueType::I32], vec![ValueType::I32]),
            vec![Instruction::tail_call_return_call(1)],
        )
        .unwrap();
        let mut interpreter = Interpreter::new(Module {
            functions: vec![caller, callee],
            ..Module::default()
        });

        assert_eq!(vec![42], interpreter.call(0, &[41]).unwrap());
    }

    #[test]
    fn executes_atomic_rmw_and_wait_ops() {
        let function = Function::new_native(
            Signature::new(vec![], vec![ValueType::I32, ValueType::I32]),
            vec![
                Instruction::const_i32(0),
                Instruction::const_i32(7),
                Instruction::atomic_store(UnsignedType::I32, MemoryArg::default()),
                Instruction::const_i32(0),
                Instruction::const_i32(5),
                Instruction::atomic_rmw(
                    UnsignedType::I32,
                    MemoryArg::default(),
                    AtomicArithmeticOp::Add,
                ),
                Instruction::const_i32(0),
                Instruction::const_i32(0),
                Instruction::const_i64(0),
                Instruction::atomic_memory_wait(UnsignedType::I32, MemoryArg::default()),
                Instruction::br(label(LabelKind::Return, 0)),
            ],
        )
        .unwrap();
        let mut interpreter = Interpreter::new(Module {
            functions: vec![function],
            memory: Some(Memory::from_bytes(vec![0; WASM_PAGE_SIZE], Some(1), true)),
            ..Module::default()
        });

        assert_eq!(vec![7, 1], interpreter.call(0, &[]).unwrap());
        let memory = interpreter.module.memory.as_ref().expect("memory");
        assert_eq!(Some(12), memory.read_u32_le(0));
    }

    #[test]
    fn host_errors_surface_as_traps() {
        let mut interpreter = Interpreter::new(Module {
            functions: vec![Function::new_host(
                Signature::default(),
                host_function(|_, _| Err(Trap::new("boom"))),
            )],
            ..Module::default()
        });

        assert_eq!("boom", interpreter.call(0, &[]).unwrap_err().message());
    }
}
