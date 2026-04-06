#![doc = "Interpreter runtime, eval loop, and host-call dispatch."]

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Debug, Display, Formatter};
use std::sync::Arc;

use crate::operations::{
    FloatKind, InclusiveRange, Instruction, Label, OperationKind, SignedInt, SignedType,
    UnsignedType,
};
use crate::signature::Signature;

pub const DEFAULT_CALL_STACK_CEILING: usize = 2_000;
pub const WASM_PAGE_SIZE: usize = 65_536;

pub type RuntimeResult<T> = Result<T, Trap>;
pub type HostFuncRef = Arc<dyn HostFunction>;

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    pub elements: Vec<Option<usize>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory {
    bytes: Vec<u8>,
    pub max_pages: Option<u32>,
}

impl Memory {
    pub fn new(initial_pages: u32, max_pages: Option<u32>) -> Self {
        Self {
            bytes: vec![0; initial_pages as usize * WASM_PAGE_SIZE],
            max_pages,
        }
    }

    pub fn from_bytes(bytes: Vec<u8>, max_pages: Option<u32>) -> Self {
        Self { bytes, max_pages }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    pub fn pages(&self) -> u32 {
        (self.bytes.len() / WASM_PAGE_SIZE) as u32
    }

    pub fn grow(&mut self, additional_pages: u32) -> Option<u32> {
        let previous = self.pages();
        let new_pages = previous.checked_add(additional_pages)?;
        if self
            .max_pages
            .is_some_and(|max_pages| new_pages > max_pages)
        {
            return None;
        }

        let new_len = new_pages as usize * WASM_PAGE_SIZE;
        self.bytes.resize(new_len, 0);
        Some(previous)
    }

    fn range(&self, offset: u32, len: usize) -> Option<std::ops::Range<usize>> {
        let start = offset as usize;
        let end = start.checked_add(len)?;
        (end <= self.bytes.len()).then_some(start..end)
    }

    pub fn read_byte(&self, offset: u32) -> Option<u8> {
        self.bytes.get(offset as usize).copied()
    }

    pub fn write_byte(&mut self, offset: u32, value: u8) -> bool {
        match self.bytes.get_mut(offset as usize) {
            Some(byte) => {
                *byte = value;
                true
            }
            None => false,
        }
    }

    pub fn read_u16_le(&self, offset: u32) -> Option<u16> {
        let range = self.range(offset, 2)?;
        Some(u16::from_le_bytes(self.bytes[range].try_into().ok()?))
    }

    pub fn write_u16_le(&mut self, offset: u32, value: u16) -> bool {
        let Some(range) = self.range(offset, 2) else {
            return false;
        };
        self.bytes[range].copy_from_slice(&value.to_le_bytes());
        true
    }

    pub fn read_u32_le(&self, offset: u32) -> Option<u32> {
        let range = self.range(offset, 4)?;
        Some(u32::from_le_bytes(self.bytes[range].try_into().ok()?))
    }

    pub fn write_u32_le(&mut self, offset: u32, value: u32) -> bool {
        let Some(range) = self.range(offset, 4) else {
            return false;
        };
        self.bytes[range].copy_from_slice(&value.to_le_bytes());
        true
    }

    pub fn read_u64_le(&self, offset: u32) -> Option<u64> {
        let range = self.range(offset, 8)?;
        Some(u64::from_le_bytes(self.bytes[range].try_into().ok()?))
    }

    pub fn write_u64_le(&mut self, offset: u32, value: u64) -> bool {
        let Some(range) = self.range(offset, 8) else {
            return false;
        };
        self.bytes[range].copy_from_slice(&value.to_le_bytes());
        true
    }
}

#[derive(Clone)]
pub struct Function {
    pub signature: Signature,
    body: Vec<Instruction>,
    host: Option<HostFuncRef>,
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
            body,
            host: None,
        })
    }

    pub fn new_host(signature: impl Into<Signature>, host: HostFuncRef) -> Self {
        Self {
            signature: signature.into(),
            body: Vec::new(),
            host: Some(host),
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
    pub exit_code: Option<u32>,
}

impl Module {
    pub fn fail_if_closed(&self) -> RuntimeResult<()> {
        match self.exit_code {
            Some(exit_code) => Err(Trap::new(format!("module exited with code {exit_code}"))),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Interpreter {
    pub module: Module,
    pub call_stack_ceiling: usize,
}

impl Interpreter {
    pub fn new(module: Module) -> Self {
        Self {
            module,
            call_stack_ceiling: DEFAULT_CALL_STACK_CEILING,
        }
    }

    pub fn call(&mut self, function_index: usize, params: &[u64]) -> RuntimeResult<Vec<u64>> {
        let function = self
            .module
            .functions
            .get(function_index)
            .ok_or_else(|| Trap::new(format!("function[{function_index}] is undefined")))?
            .clone();
        if params.len() != function.signature.param_slots {
            return Err(Trap::new(format!(
                "expected {} params, but passed {}",
                function.signature.param_slots,
                params.len()
            )));
        }

        self.module.fail_if_closed()?;

        let mut engine = CallEngine::default();
        engine.push_values(params);
        engine.call_function(self, function_index)?;

        let mut results = vec![0; function.signature.result_slots];
        engine.pop_values(&mut results);
        Ok(results)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CallFrame {
    pc: usize,
    function_index: usize,
    base: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CallEngine {
    stack: Vec<u64>,
    frames: Vec<CallFrame>,
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
            self.call_native_function(interpreter, function_index, &function)
        }
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
        let result = host.call(&mut interpreter.module, &mut self.stack[start..]);

        self.pop_frame();

        if param_len > result_len {
            self.stack
                .truncate(self.stack.len() - (param_len - result_len));
        }

        result
    }

    fn call_native_function(
        &mut self,
        interpreter: &mut Interpreter,
        function_index: usize,
        function: &Function,
    ) -> RuntimeResult<()> {
        self.push_frame(
            CallFrame {
                pc: 0,
                function_index,
                base: self.stack.len(),
            },
            interpreter.call_stack_ceiling,
        )?;

        let body = &function.body;
        while let Some(frame) = self.frames.last().cloned() {
            if frame.function_index != function_index || frame.pc >= body.len() {
                break;
            }

            let op = body[frame.pc].clone();
            match op.kind {
                OperationKind::BuiltinFunctionCheckExitCode => {
                    interpreter.module.fail_if_closed()?;
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                OperationKind::Unreachable => return Err(Trap::new("unreachable")),
                OperationKind::Label => self.frames.last_mut().expect("frame").pc += 1,
                OperationKind::Br => self.frames.last_mut().expect("frame").pc = op.u1 as usize,
                OperationKind::BrIf => {
                    if self.pop_value() > 0 {
                        self.drop_range(op.u3);
                        self.frames.last_mut().expect("frame").pc = op.u1 as usize;
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
                    self.frames.last_mut().expect("frame").pc = op.us[target_index] as usize;
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
                        .elements
                        .get(table_offset)
                        .and_then(|index| *index)
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
                    self.push_value(execute_shift(op.kind, decode_signed_type(op.b1), v1, v2));
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
                        .and_then(|segment| segment.as_ref())
                        .ok_or_else(|| Trap::new(format!("data[{}] is unavailable", op.u1)))?;
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
                        .filter(|end| *end <= memory.bytes.len())
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    memory.bytes[dst_offset..target].copy_from_slice(&bytes);
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
                        .filter(|end| *end <= memory.bytes.len())
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    let dst_end = dst
                        .checked_add(len)
                        .filter(|end| *end <= memory.bytes.len())
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    memory.bytes.copy_within(src..src_end, dst);
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
                        .filter(|end| *end <= memory.bytes.len())
                        .ok_or_else(|| Trap::new("out of bounds memory access"))?;
                    memory.bytes[dst..end].fill(value);
                    self.frames.last_mut().expect("frame").pc += 1;
                }
                _ => return Err(Trap::new(format!("operation {} not implemented", op.kind))),
            }
        }

        self.pop_frame();
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
            let rhs = v2 as u32 as i32;
            if rhs == 0 {
                return Err(Trap::new("integer divide by zero"));
            }
            Ok(u64::from(((v1 as u32 as i32) % rhs) as u32))
        }
        SignedType::Uint32 => {
            let rhs = v2 as u32;
            if rhs == 0 {
                return Err(Trap::new("integer divide by zero"));
            }
            Ok(u64::from((v1 as u32) % rhs))
        }
        SignedType::Int64 => {
            let rhs = v2 as i64;
            if rhs == 0 {
                return Err(Trap::new("integer divide by zero"));
            }
            Ok(((v1 as i64) % rhs) as u64)
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

fn execute_shift(kind: OperationKind, ty: SignedType, v1: u64, v2: u64) -> u64 {
    match ty {
        SignedType::Int32 | SignedType::Uint32 => {
            let rhs = (v2 as u32) & 31;
            let lhs = v1 as u32;
            match kind {
                OperationKind::Shl => u64::from(lhs.wrapping_shl(rhs)),
                OperationKind::Shr => {
                    if matches!(ty, SignedType::Int32) {
                        u64::from(((lhs as i32) >> rhs) as u32)
                    } else {
                        u64::from(lhs >> rhs)
                    }
                }
                OperationKind::Rotl => u64::from(lhs.rotate_left(rhs)),
                OperationKind::Rotr => u64::from(lhs.rotate_right(rhs)),
                _ => unreachable!(),
            }
        }
        SignedType::Int64 | SignedType::Uint64 => {
            let rhs = (v2 as u32) & 63;
            match kind {
                OperationKind::Shl => v1.wrapping_shl(rhs),
                OperationKind::Shr => {
                    if matches!(ty, SignedType::Int64) {
                        ((v1 as i64) >> rhs) as u64
                    } else {
                        v1 >> rhs
                    }
                }
                OperationKind::Rotl => v1.rotate_left(rhs),
                OperationKind::Rotr => v1.rotate_right(rhs),
                _ => unreachable!(),
            }
        }
        SignedType::Float32 | SignedType::Float64 => 0,
    }
}

fn execute_unary_float(kind: OperationKind, float_kind: FloatKind, value: u64) -> u64 {
    match float_kind {
        FloatKind::F32 => {
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
        FloatKind::F64 => match kind {
            OperationKind::Abs => f64::from_bits(value).abs().to_bits(),
            OperationKind::Neg => (-f64::from_bits(value)).to_bits(),
            OperationKind::Ceil => f64::from_bits(value).ceil().to_bits(),
            OperationKind::Floor => f64::from_bits(value).floor().to_bits(),
            OperationKind::Trunc => f64::from_bits(value).trunc().to_bits(),
            OperationKind::Nearest => nearest_f64(f64::from_bits(value)).to_bits(),
            OperationKind::Sqrt => f64::from_bits(value).sqrt().to_bits(),
            _ => unreachable!(),
        },
    }
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
        SignedInt::Int64 => trunc_signed(truncated, i64::MIN as f64, i64::MAX as f64, non_trapping)
            .map(|value| value as i64 as u64),
        SignedInt::Uint32 => trunc_unsigned(truncated, u32::MAX as f64, non_trapping)
            .map(|value| u64::from(value as u32)),
        SignedInt::Uint64 => trunc_unsigned(truncated, u64::MAX as f64, non_trapping),
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

#[cfg(test)]
mod tests {
    use super::{
        host_function, CallEngine, CallFrame, Function, GlobalValue, Interpreter, Memory, Module,
        Table, Trap,
    };
    use crate::compiler::{CompileConfig, Compiler, FunctionType, ValueType};
    use crate::operations::{
        FloatKind, InclusiveRange, Instruction, Label, LabelKind, MemoryArg, UnsignedType,
    };
    use crate::signature::Signature;

    fn label(kind: LabelKind, frame_id: u32) -> Label {
        Label::new(kind, frame_id)
    }

    fn i32_i32() -> FunctionType {
        FunctionType::new(vec![ValueType::I32], vec![ValueType::I32])
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
            tables: vec![Table {
                elements: vec![Some(1)],
            }],
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
            tables: vec![Table {
                elements: vec![Some(1)],
            }],
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
