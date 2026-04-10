use std::{
    any::Any,
    cell::RefCell,
    collections::BTreeMap,
    fmt::{self, Display, Formatter},
    panic::{self, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
    thread,
};

use crate::{
    api::error::{policy_denied_error, ExitError, Result, RuntimeError},
    ctx_keys::{Context, ContextDoneError, InvocationContext},
    experimental::{
        close_notifier::CloseNotifier,
        fuel::FuelController,
        host_call_policy::HostCallPolicyRequest,
        host_call_policy_observer::{notify_host_call_policy_observer, HostCallPolicyDecision},
        listener::{new_stack_iterator, FunctionListener, StackFrame},
        memory::LinearMemory,
        r#yield::{Resumer, YieldError, Yielder},
        snapshotter::{Snapshot, Snapshotter},
        trap::{get_trap_observer, trap_cause_of, TrapObservation},
        yield_policy::YieldPolicyRequest,
    },
    runtime::RuntimeStore,
};
use razero_interp::{
    engine::{take_suspended_invocation, SuspendedInvocation},
    interpreter::{is_yield_suspend_payload, with_active_fuel_remaining, YieldSuspend},
};
use razero_wasm::{
    module::{FunctionType as WasmFunctionType, Module as WasmModule},
    module_instance_lookup::LookupError,
    store_module_list::ModuleInstanceId,
};

pub type HostCallback =
    Arc<dyn Fn(Context, Module, &[u64]) -> Result<Vec<u64>> + Send + Sync + 'static>;

pub type RuntimeModuleRegistry = Arc<Mutex<BTreeMap<String, Module>>>;

#[derive(Clone, Copy, Debug)]
struct PolicyDeniedPayload(&'static str);

fn policy_denied_reason(payload: &(dyn Any + Send)) -> Option<&'static str> {
    payload
        .downcast_ref::<PolicyDeniedPayload>()
        .map(|payload| payload.0)
}

fn notify_trap_observer(ctx: &Context, module: &Module, err: &RuntimeError) {
    let Some(observer) = get_trap_observer(ctx) else {
        return;
    };
    let Some(cause) = trap_cause_of(err) else {
        return;
    };
    observer.observe_trap(
        ctx,
        TrapObservation {
            module: module.clone(),
            cause,
            err: err.clone(),
        },
    );
}

thread_local! {
    static ACTIVE_INVOCATIONS: RefCell<Vec<(Context, Module)>> = const { RefCell::new(Vec::new()) };
}

static NEXT_RESUMER_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn with_active_invocation<T>(
    ctx: &Context,
    module: &Module,
    f: impl FnOnce() -> T,
) -> T {
    ACTIVE_INVOCATIONS.with(|active| active.borrow_mut().push((ctx.clone(), module.clone())));
    let result = f();
    ACTIVE_INVOCATIONS.with(|active| {
        active.borrow_mut().pop();
    });
    result
}

pub(crate) fn active_invocation() -> Option<(Context, Module)> {
    ACTIVE_INVOCATIONS.with(|active| active.borrow().last().cloned())
}

fn next_resumer_id() -> u64 {
    NEXT_RESUMER_ID.fetch_add(1, Ordering::Relaxed)
}

fn context_done_exit_code(reason: ContextDoneError) -> u32 {
    match reason {
        ContextDoneError::Canceled => crate::api::error::EXIT_CODE_CONTEXT_CANCELED,
        ContextDoneError::DeadlineExceeded => crate::api::error::EXIT_CODE_DEADLINE_EXCEEDED,
    }
}

pub(crate) fn install_close_on_context_done(
    ctx: &Context,
    module: &Module,
    enabled: bool,
) -> Result<Option<Arc<AtomicBool>>> {
    if !enabled {
        return Ok(None);
    }

    if let Some(reason) = ctx.done_error() {
        let exit_code = context_done_exit_code(reason);
        let _ = module.close_with_exit_code(ctx, exit_code);
        return Err(ExitError::new(exit_code).into());
    }

    if !ctx.has_lifecycle() {
        return Ok(None);
    }

    let stop = Arc::new(AtomicBool::new(false));
    let wait_ctx = ctx.clone();
    let wait_module = module.clone();
    let wait_stop = stop.clone();
    thread::spawn(move || {
        if let Some(reason) = wait_ctx.wait_until_done_or_stopped(&wait_stop) {
            let _ = wait_module.close_with_exit_code(&wait_ctx, context_done_exit_code(reason));
        }
    });
    Ok(Some(stop))
}

fn listener_stack_for_call(
    ctx: &Context,
    definition: &FunctionDefinition,
    params: &[u64],
    source_offset: u64,
) -> Vec<StackFrame> {
    let mut stack = ctx
        .invocation
        .as_ref()
        .map(|invocation| invocation.listener_stack.clone())
        .unwrap_or_default();
    stack.reserve(
        1 + ctx
            .invocation
            .as_ref()
            .map(|invocation| invocation.listener_stack.len())
            .unwrap_or_default(),
    );
    stack.push(StackFrame::new(
        definition.clone(),
        params.to_vec(),
        Vec::new(),
        0,
        source_offset,
    ));
    stack
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ExternType {
    Func,
    Table,
    Memory,
    Global,
}

impl ExternType {
    pub fn name(self) -> &'static str {
        match self {
            Self::Func => "func",
            Self::Table => "table",
            Self::Memory => "memory",
            Self::Global => "global",
        }
    }
}

pub fn extern_type_name(extern_type: ExternType) -> &'static str {
    extern_type.name()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ValueType {
    #[default]
    I32,
    I64,
    F32,
    F64,
    V128,
    ExternRef,
    FuncRef,
}

impl ValueType {
    pub fn name(self) -> &'static str {
        match self {
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::V128 => "v128",
            Self::ExternRef => "externref",
            Self::FuncRef => "funcref",
        }
    }
}

pub fn value_type_name(value_type: ValueType) -> &'static str {
    value_type.name()
}

pub fn encode_externref(input: usize) -> u64 {
    input as u64
}

pub fn decode_externref(input: u64) -> usize {
    input as usize
}

pub fn encode_i32(input: i32) -> u64 {
    input as u32 as u64
}

pub fn decode_i32(input: u64) -> i32 {
    input as u32 as i32
}

pub fn encode_u32(input: u32) -> u64 {
    input as u64
}

pub fn decode_u32(input: u64) -> u32 {
    input as u32
}

pub fn encode_i64(input: i64) -> u64 {
    input as u64
}

pub fn encode_f32(input: f32) -> u64 {
    input.to_bits() as u64
}

pub fn decode_f32(input: u64) -> f32 {
    f32::from_bits(input as u32)
}

pub fn encode_f64(input: f64) -> u64 {
    input.to_bits()
}

pub fn decode_f64(input: u64) -> f64 {
    f64::from_bits(input)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FunctionDefinition {
    module_name: Option<String>,
    name: String,
    export_names: Vec<String>,
    param_types: Vec<ValueType>,
    result_types: Vec<ValueType>,
    param_names: Vec<String>,
    result_names: Vec<String>,
    import: Option<(String, String)>,
}

impl FunctionDefinition {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    pub fn with_module_name(mut self, module_name: Option<String>) -> Self {
        self.module_name = module_name;
        self
    }

    pub fn with_export_name(mut self, export_name: impl Into<String>) -> Self {
        self.export_names.push(export_name.into());
        self
    }

    pub fn with_signature(mut self, params: Vec<ValueType>, results: Vec<ValueType>) -> Self {
        self.param_types = params;
        self.result_types = results;
        self
    }

    pub fn with_parameter_names(mut self, names: Vec<String>) -> Self {
        self.param_names = names;
        self
    }

    pub fn with_result_names(mut self, names: Vec<String>) -> Self {
        self.result_names = names;
        self
    }

    pub fn with_import(mut self, module: impl Into<String>, name: impl Into<String>) -> Self {
        self.import = Some((module.into(), name.into()));
        self
    }

    pub fn module_name(&self) -> Option<&str> {
        self.module_name.as_deref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn export_names(&self) -> &[String] {
        &self.export_names
    }

    pub fn param_types(&self) -> &[ValueType] {
        &self.param_types
    }

    pub fn result_types(&self) -> &[ValueType] {
        &self.result_types
    }

    pub fn param_names(&self) -> &[String] {
        &self.param_names
    }

    pub fn result_names(&self) -> &[String] {
        &self.result_names
    }

    pub fn import(&self) -> Option<(&str, &str)> {
        self.import
            .as_ref()
            .map(|(module, name)| (module.as_str(), name.as_str()))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MemoryDefinition {
    module_name: Option<String>,
    minimum_pages: u32,
    maximum_pages: Option<u32>,
    export_names: Vec<String>,
    import: Option<(String, String)>,
}

impl MemoryDefinition {
    pub fn new(minimum_pages: u32, maximum_pages: Option<u32>) -> Self {
        Self {
            minimum_pages,
            maximum_pages,
            ..Self::default()
        }
    }

    pub fn with_module_name(mut self, module_name: Option<String>) -> Self {
        self.module_name = module_name;
        self
    }

    pub fn with_export_name(mut self, export_name: impl Into<String>) -> Self {
        self.export_names.push(export_name.into());
        self
    }

    pub fn with_import(mut self, module: impl Into<String>, name: impl Into<String>) -> Self {
        self.import = Some((module.into(), name.into()));
        self
    }

    pub fn minimum_pages(&self) -> u32 {
        self.minimum_pages
    }

    pub fn maximum_pages(&self) -> Option<u32> {
        self.maximum_pages
    }

    pub fn module_name(&self) -> Option<&str> {
        self.module_name.as_deref()
    }

    pub fn export_names(&self) -> &[String] {
        &self.export_names
    }

    pub fn import(&self) -> Option<(&str, &str)> {
        self.import
            .as_ref()
            .map(|(module, name)| (module.as_str(), name.as_str()))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GlobalDefinition {
    value_type: ValueType,
    is_mutable: bool,
    export_names: Vec<String>,
    module_name: Option<String>,
    import: Option<(String, String)>,
}

impl GlobalDefinition {
    pub fn new(value_type: ValueType, is_mutable: bool) -> Self {
        Self {
            value_type,
            is_mutable,
            ..Self::default()
        }
    }

    pub fn with_module_name(mut self, module_name: Option<String>) -> Self {
        self.module_name = module_name;
        self
    }

    pub fn with_export_name(mut self, export_name: impl Into<String>) -> Self {
        self.export_names.push(export_name.into());
        self
    }

    pub fn with_import(mut self, module: impl Into<String>, name: impl Into<String>) -> Self {
        self.import = Some((module.into(), name.into()));
        self
    }

    pub fn value_type(&self) -> ValueType {
        self.value_type
    }

    pub fn is_mutable(&self) -> bool {
        self.is_mutable
    }

    pub fn module_name(&self) -> Option<&str> {
        self.module_name.as_deref()
    }

    pub fn export_names(&self) -> &[String] {
        &self.export_names
    }

    pub fn import(&self) -> Option<(&str, &str)> {
        self.import
            .as_ref()
            .map(|(module, name)| (module.as_str(), name.as_str()))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TableDefinition {
    ref_type: ValueType,
    minimum: u32,
    maximum: Option<u32>,
    export_names: Vec<String>,
    module_name: Option<String>,
    import: Option<(String, String)>,
}

impl TableDefinition {
    pub fn new(ref_type: ValueType, minimum: u32, maximum: Option<u32>) -> Self {
        Self {
            ref_type,
            minimum,
            maximum,
            ..Self::default()
        }
    }

    pub fn with_module_name(mut self, module_name: Option<String>) -> Self {
        self.module_name = module_name;
        self
    }

    pub fn with_export_name(mut self, export_name: impl Into<String>) -> Self {
        self.export_names.push(export_name.into());
        self
    }

    pub fn with_import(mut self, module: impl Into<String>, name: impl Into<String>) -> Self {
        self.import = Some((module.into(), name.into()));
        self
    }

    pub fn ref_type(&self) -> ValueType {
        self.ref_type
    }

    pub fn minimum(&self) -> u32 {
        self.minimum
    }

    pub fn maximum(&self) -> Option<u32> {
        self.maximum
    }

    pub fn module_name(&self) -> Option<&str> {
        self.module_name.as_deref()
    }

    pub fn export_names(&self) -> &[String] {
        &self.export_names
    }

    pub fn import(&self) -> Option<(&str, &str)> {
        self.import
            .as_ref()
            .map(|(module, name)| (module.as_str(), name.as_str()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomSection {
    name: String,
    data: Vec<u8>,
}

impl CustomSection {
    pub fn new(name: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            data,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlobalValue {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
}

impl GlobalValue {
    pub fn value_type(self) -> ValueType {
        match self {
            Self::I32(_) => ValueType::I32,
            Self::I64(_) => ValueType::I64,
            Self::F32(_) => ValueType::F32,
            Self::F64(_) => ValueType::F64,
        }
    }
}

trait GlobalAccess: Send + Sync {
    fn get(&self) -> GlobalValue;
    fn set(&self, value: GlobalValue);
    fn is_mutable(&self) -> bool;
}

struct OwnedGlobalAccess {
    value: Arc<Mutex<GlobalValue>>,
    mutable: bool,
}

impl GlobalAccess for OwnedGlobalAccess {
    fn get(&self) -> GlobalValue {
        *self.value.lock().expect("global poisoned")
    }

    fn set(&self, value: GlobalValue) {
        *self.value.lock().expect("global poisoned") = value;
    }

    fn is_mutable(&self) -> bool {
        self.mutable
    }
}

struct DynamicGlobalAccess {
    getter: Arc<dyn Fn() -> GlobalValue + Send + Sync>,
    setter: Option<Arc<dyn Fn(GlobalValue) + Send + Sync>>,
    mutable: bool,
}

impl GlobalAccess for DynamicGlobalAccess {
    fn get(&self) -> GlobalValue {
        (self.getter)()
    }

    fn set(&self, value: GlobalValue) {
        self.setter
            .as_ref()
            .expect("mutable global setter is unavailable")(value);
    }

    fn is_mutable(&self) -> bool {
        self.mutable
    }
}

#[derive(Clone)]
pub struct Global {
    access: Arc<dyn GlobalAccess>,
}

impl Global {
    pub fn new(value: GlobalValue, mutable: bool) -> Self {
        Self {
            access: Arc::new(OwnedGlobalAccess {
                value: Arc::new(Mutex::new(value)),
                mutable,
            }),
        }
    }

    pub(crate) fn dynamic_with_setter(
        mutable: bool,
        getter: impl Fn() -> GlobalValue + Send + Sync + 'static,
        setter: Option<Arc<dyn Fn(GlobalValue) + Send + Sync>>,
    ) -> Self {
        Self {
            access: Arc::new(DynamicGlobalAccess {
                getter: Arc::new(getter),
                setter,
                mutable,
            }),
        }
    }

    pub fn value_type(&self) -> ValueType {
        self.access.get().value_type()
    }

    pub fn is_mutable(&self) -> bool {
        self.access.is_mutable()
    }

    pub fn get(&self) -> GlobalValue {
        self.access.get()
    }

    pub fn set(&self, value: GlobalValue) {
        assert!(self.is_mutable(), "global is immutable");
        assert_eq!(
            self.value_type(),
            value.value_type(),
            "global type mismatch"
        );
        self.access.set(value);
    }
}

trait MemoryAccess: Send + Sync {
    fn size(&self) -> u32;
    fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>>;
    fn write(&self, offset: usize, values: &[u8]) -> bool;
    fn write_u32_le(&self, offset: u32, value: u32) -> bool;
    fn grow(&self, delta_pages: u32, maximum_pages: Option<u32>) -> Option<u32>;
}

struct OwnedMemoryAccess {
    memory: Arc<Mutex<LinearMemory>>,
}

impl MemoryAccess for OwnedMemoryAccess {
    fn size(&self) -> u32 {
        self.memory.lock().expect("memory poisoned").len() as u32
    }

    fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        let memory = self.memory.lock().expect("memory poisoned");
        let end = offset.checked_add(len)?;
        memory.bytes().get(offset..end).map(ToOwned::to_owned)
    }

    fn write(&self, offset: usize, values: &[u8]) -> bool {
        let mut memory = self.memory.lock().expect("memory poisoned");
        let end = match offset.checked_add(values.len()) {
            Some(end) => end,
            None => return false,
        };
        let Some(slice) = memory.bytes_mut().get_mut(offset..end) else {
            return false;
        };
        slice.copy_from_slice(values);
        true
    }

    fn write_u32_le(&self, offset: u32, value: u32) -> bool {
        let mut memory = self.memory.lock().expect("memory poisoned");
        let start = offset as usize;
        let end = match start.checked_add(4) {
            Some(end) => end,
            None => return false,
        };
        let Some(slice) = memory.bytes_mut().get_mut(start..end) else {
            return false;
        };
        slice.copy_from_slice(&value.to_le_bytes());
        true
    }

    fn grow(&self, delta_pages: u32, maximum_pages: Option<u32>) -> Option<u32> {
        let mut memory = self.memory.lock().expect("memory poisoned");
        let previous = (memory.len() / 65_536) as u32;
        let new_pages = previous.checked_add(delta_pages)?;
        if maximum_pages.is_some_and(|maximum| new_pages > maximum) {
            return None;
        }
        memory.reallocate(new_pages as usize * 65_536)?;
        Some(previous)
    }
}

struct DynamicMemoryAccess {
    size: Arc<dyn Fn() -> u32 + Send + Sync>,
    read: Arc<dyn Fn(usize, usize) -> Option<Vec<u8>> + Send + Sync>,
    write: Arc<dyn Fn(usize, &[u8]) -> bool + Send + Sync>,
    write_u32_le: Arc<dyn Fn(u32, u32) -> bool + Send + Sync>,
    grow: Arc<dyn Fn(u32, Option<u32>) -> Option<u32> + Send + Sync>,
}

impl MemoryAccess for DynamicMemoryAccess {
    fn size(&self) -> u32 {
        (self.size)()
    }

    fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        (self.read)(offset, len)
    }

    fn write(&self, offset: usize, values: &[u8]) -> bool {
        (self.write)(offset, values)
    }

    fn write_u32_le(&self, offset: u32, value: u32) -> bool {
        (self.write_u32_le)(offset, value)
    }

    fn grow(&self, delta_pages: u32, maximum_pages: Option<u32>) -> Option<u32> {
        (self.grow)(delta_pages, maximum_pages)
    }
}

#[derive(Clone)]
pub struct Memory {
    definition: MemoryDefinition,
    access: Arc<dyn MemoryAccess>,
}

impl Memory {
    pub fn new(definition: MemoryDefinition, linear_memory: LinearMemory) -> Self {
        Self {
            definition,
            access: Arc::new(OwnedMemoryAccess {
                memory: Arc::new(Mutex::new(linear_memory)),
            }),
        }
    }

    pub(crate) fn dynamic(
        definition: MemoryDefinition,
        size: impl Fn() -> u32 + Send + Sync + 'static,
        read: impl Fn(usize, usize) -> Option<Vec<u8>> + Send + Sync + 'static,
        write: impl Fn(usize, &[u8]) -> bool + Send + Sync + 'static,
        write_u32_le: impl Fn(u32, u32) -> bool + Send + Sync + 'static,
        grow: impl Fn(u32, Option<u32>) -> Option<u32> + Send + Sync + 'static,
    ) -> Self {
        Self {
            definition,
            access: Arc::new(DynamicMemoryAccess {
                size: Arc::new(size),
                read: Arc::new(read),
                write: Arc::new(write),
                write_u32_le: Arc::new(write_u32_le),
                grow: Arc::new(grow),
            }),
        }
    }

    pub fn definition(&self) -> &MemoryDefinition {
        &self.definition
    }

    pub fn size(&self) -> u32 {
        self.access.size()
    }

    pub fn pages(&self) -> u32 {
        self.size() / 65_536
    }

    pub fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        self.access.read(offset, len)
    }

    pub fn write(&self, offset: usize, values: &[u8]) -> bool {
        self.access.write(offset, values)
    }

    pub fn read_u32_le(&self, offset: u32) -> Option<u32> {
        let bytes = self.read(offset as usize, 4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }

    pub fn read_u64_le(&self, offset: u32) -> Option<u64> {
        let bytes = self.read(offset as usize, 8)?;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    }

    pub fn read_f32_le(&self, offset: u32) -> Option<f32> {
        let bytes = self.read(offset as usize, 4)?;
        Some(f32::from_le_bytes(bytes.try_into().ok()?))
    }

    pub fn read_f64_le(&self, offset: u32) -> Option<f64> {
        let bytes = self.read(offset as usize, 8)?;
        Some(f64::from_le_bytes(bytes.try_into().ok()?))
    }

    pub fn write_u32_le(&self, offset: u32, value: u32) -> bool {
        self.access.write_u32_le(offset, value)
    }

    pub fn write_u64_le(&self, offset: u32, value: u64) -> bool {
        self.write(offset as usize, &value.to_le_bytes())
    }

    pub fn write_f32_le(&self, offset: u32, value: f32) -> bool {
        self.write(offset as usize, &value.to_le_bytes())
    }

    pub fn write_f64_le(&self, offset: u32, value: f64) -> bool {
        self.write(offset as usize, &value.to_le_bytes())
    }

    pub fn grow(&self, delta_pages: u32) -> Option<u32> {
        self.access
            .grow(delta_pages, self.definition.maximum_pages())
    }
}

#[derive(Clone)]
pub struct Function {
    inner: Arc<FunctionInner>,
}

struct FunctionInner {
    module: Weak<ModuleInner>,
    definition: FunctionDefinition,
    callback: Option<HostCallback>,
    is_host: bool,
    default_fuel: i64,
    source_offset: u64,
}

impl Function {
    pub(crate) fn new(
        module: Weak<ModuleInner>,
        definition: FunctionDefinition,
        callback: Option<HostCallback>,
        is_host: bool,
        default_fuel: i64,
        source_offset: u64,
    ) -> Self {
        Self {
            inner: Arc::new(FunctionInner {
                module,
                definition,
                callback,
                is_host,
                default_fuel,
                source_offset,
            }),
        }
    }

    pub fn name(&self) -> &str {
        self.inner.definition.name()
    }

    pub fn definition(&self) -> &FunctionDefinition {
        &self.inner.definition
    }

    pub fn source_offset_for_pc(&self, pc: u64) -> u64 {
        if self.inner.source_offset == 0 {
            0
        } else {
            self.inner.source_offset.saturating_add(pc)
        }
    }

    pub fn call(&self, params: &[u64]) -> Result<Vec<u64>> {
        self.call_with_context(&Context::default(), params)
    }

    pub fn call_with_context(&self, ctx: &Context, params: &[u64]) -> Result<Vec<u64>> {
        let module = Module {
            inner: self
                .inner
                .module
                .upgrade()
                .ok_or_else(|| RuntimeError::new("module instance is no longer available"))?,
        };

        if module.is_closed() {
            return Err(ExitError::new(module.exit_code()).into());
        }
        module.ensure_no_suspended_execution()?;

        let callback = self.inner.callback.clone().ok_or_else(|| {
            RuntimeError::new("guest function execution is not yet wired through the public API")
        })?;
        let close_on_context_done =
            install_close_on_context_done(ctx, &module, module.close_on_context_done())?;

        let listener = ctx
            .function_listener_factory
            .as_ref()
            .and_then(|factory| factory.new_listener(&self.inner.definition));
        let budget = ctx
            .fuel_controller
            .as_ref()
            .map(|controller| controller.budget())
            .unwrap_or(self.inner.default_fuel)
            .max(0);
        let fuel_remaining =
            (budget > 0).then(|| Arc::new(std::sync::atomic::AtomicI64::new(budget)));
        if self.inner.is_host {
            if let Some(remaining) = &fuel_remaining {
                remaining.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let restored_results = Arc::new(Mutex::new(None));
        let snapshot_active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let snapshotter = ctx.snapshotter_enabled.then(|| {
            Arc::new(ActiveSnapshotter::new(
                restored_results.clone(),
                snapshot_active.clone(),
            )) as Arc<dyn Snapshotter>
        });

        let listener_stack = listener_stack_for_call(
            ctx,
            &self.inner.definition,
            params,
            self.source_offset_for_pc(0),
        );
        let resumer_id = next_resumer_id();
        let resumer = Arc::new(PendingResumer::new(
            resumer_id,
            module.clone(),
            self.inner.definition.clone(),
            listener.clone(),
            listener_stack.clone(),
            ctx.fuel_controller.clone(),
            fuel_remaining.clone(),
        ));
        let yielder = ctx
            .yielder_enabled
            .then(|| Arc::new(ActiveYielder::new()) as Arc<dyn Yielder>);
        let invocation_ctx = ctx.with_invocation(InvocationContext {
            fuel_remaining: fuel_remaining.clone(),
            snapshotter,
            yielder,
            function_listener: listener.clone(),
            function_definition: Some(self.inner.definition.clone()),
            listener_stack: listener_stack.clone(),
        });

        let mut request = HostCallPolicyRequest::new().with_function(self.inner.definition.clone());
        if let Some(module_name) = module.name() {
            request = request.with_caller_module_name(module_name);
        }
        if let Some(memory) = module.memory() {
            request = request.with_memory(memory.definition().clone());
        }
        let policy = ctx
            .host_call_policy
            .clone()
            .or_else(|| module.host_call_policy());
        let allowed = !self.inner.is_host
            || !policy
                .as_ref()
                .is_some_and(|policy| !policy.allow_host_call(&invocation_ctx, &request));
        if self.inner.is_host {
            notify_host_call_policy_observer(
                &invocation_ctx,
                &module,
                &request,
                if allowed {
                    HostCallPolicyDecision::Allowed
                } else {
                    HostCallPolicyDecision::Denied
                },
            );
        }
        if !allowed {
            if let Some(stop) = &close_on_context_done {
                stop.store(true, Ordering::SeqCst);
            }
            snapshot_active.store(false, std::sync::atomic::Ordering::SeqCst);
            let err = policy_denied_error("host call");
            notify_trap_observer(&invocation_ctx, &module, &err);
            return Err(err);
        }

        if let Some(listener) = &listener {
            let mut stack_iterator = new_stack_iterator(&listener_stack);
            listener.before(
                &invocation_ctx,
                &module,
                &self.inner.definition,
                params,
                &mut stack_iterator,
            );
        }

        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            with_active_invocation(&invocation_ctx, &module, || {
                with_active_fuel_remaining(fuel_remaining.clone(), || {
                    callback(invocation_ctx.clone(), module.clone(), params)
                })
            })
        }));
        if let Some(stop) = close_on_context_done {
            stop.store(true, Ordering::SeqCst);
        }

        let consume_fuel =
            |controller: &Option<Arc<dyn FuelController>>,
             remaining: &Option<Arc<std::sync::atomic::AtomicI64>>| {
                if let (Some(controller), Some(remaining)) = (controller, remaining) {
                    controller.consumed(
                        (budget - remaining.load(std::sync::atomic::Ordering::SeqCst)).max(0),
                    );
                }
            };

        match outcome {
            Ok(Ok(mut results)) => {
                if let Some(restored) = restored_results.lock().expect("snapshot poisoned").take() {
                    results = restored;
                }
                if let Some(listener) = &listener {
                    listener.after(&invocation_ctx, &module, &self.inner.definition, &results);
                }
                consume_fuel(&ctx.fuel_controller, &fuel_remaining);
                if fuel_remaining.as_ref().is_some_and(|remaining| {
                    remaining.load(std::sync::atomic::Ordering::SeqCst) < 0
                }) {
                    snapshot_active.store(false, std::sync::atomic::Ordering::SeqCst);
                    return Err(RuntimeError::new("fuel exhausted"));
                }
                snapshot_active.store(false, std::sync::atomic::Ordering::SeqCst);
                Ok(results)
            }
            Ok(Err(error)) => {
                if let Some(listener) = &listener {
                    listener.abort(&invocation_ctx, &module, &self.inner.definition, &error);
                }
                consume_fuel(&ctx.fuel_controller, &fuel_remaining);
                snapshot_active.store(false, std::sync::atomic::Ordering::SeqCst);
                Err(error)
            }
            Err(payload) => {
                if is_yield_suspend_payload(payload.as_ref()) {
                    consume_fuel(&ctx.fuel_controller, &fuel_remaining);
                    resumer.note_fuel_checkpoint();
                    if let Some(suspended) = take_suspended_invocation() {
                        resumer.install_suspended_invocation(suspended);
                    }
                    module.mark_suspended(resumer_id);
                    snapshot_active.store(false, std::sync::atomic::Ordering::SeqCst);
                    let resumer: Arc<dyn Resumer> = resumer;
                    return Err(RuntimeError::from(YieldError::new(Some(resumer))));
                }
                if let Some(reason) = policy_denied_reason(payload.as_ref()) {
                    snapshot_active.store(false, std::sync::atomic::Ordering::SeqCst);
                    let err = policy_denied_error(reason);
                    notify_trap_observer(&invocation_ctx, &module, &err);
                    return Err(err);
                }
                snapshot_active.store(false, std::sync::atomic::Ordering::SeqCst);
                panic::resume_unwind(payload);
            }
        }
    }
}

#[derive(Clone)]
pub struct Module {
    inner: Arc<ModuleInner>,
}

pub(crate) struct ModuleInner {
    name: Option<String>,
    functions: BTreeMap<String, Function>,
    memory: Option<Memory>,
    globals: BTreeMap<String, Global>,
    all_globals: Vec<Global>,
    close_notifier: Option<Arc<dyn CloseNotifier>>,
    close_hook: Option<Arc<dyn Fn(u32) + Send + Sync>>,
    host_call_policy: Option<Arc<dyn crate::experimental::host_call_policy::HostCallPolicy>>,
    close_on_context_done: bool,
    closed: AtomicBool,
    exit_code: AtomicU32,
    default_fuel: i64,
    yield_policy: Option<Arc<dyn crate::experimental::yield_policy::YieldPolicy>>,
    runtime_registry: Option<Weak<Mutex<BTreeMap<String, Module>>>>,
    runtime_store: Option<Weak<Mutex<RuntimeStore>>>,
    lower_module: Option<WasmModule>,
    store_module_id: Option<ModuleInstanceId>,
    import_aliases: Mutex<Vec<String>>,
    suspension: Mutex<ModuleSuspensionState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModuleSuspensionState {
    Ready,
    Suspended { resumer_id: u64 },
    Resuming { resumer_id: u64 },
}

pub type Instance = Module;

impl Module {
    pub(crate) fn new(
        name: Option<String>,
        exported_functions: BTreeMap<String, FunctionDefinition>,
        host_callbacks: BTreeMap<String, HostCallback>,
        exported_functions_are_host: bool,
        default_fuel: i64,
        memory: Option<Memory>,
        globals: BTreeMap<String, Global>,
        all_globals: Vec<Global>,
        close_notifier: Option<Arc<dyn CloseNotifier>>,
        close_hook: Option<Arc<dyn Fn(u32) + Send + Sync>>,
        host_call_policy: Option<Arc<dyn crate::experimental::host_call_policy::HostCallPolicy>>,
        close_on_context_done: bool,
        yield_policy: Option<Arc<dyn crate::experimental::yield_policy::YieldPolicy>>,
        runtime_registry: Option<Weak<Mutex<BTreeMap<String, Module>>>>,
        runtime_store: Option<Weak<Mutex<RuntimeStore>>>,
        lower_module: Option<WasmModule>,
        function_source_offsets: BTreeMap<String, u64>,
        store_module_id: Option<ModuleInstanceId>,
    ) -> Self {
        Self {
            inner: Arc::new_cyclic(|weak| {
                let functions = exported_functions
                    .iter()
                    .map(|(export_name, definition)| {
                        (
                            export_name.clone(),
                            Function::new(
                                weak.clone(),
                                definition.clone(),
                                host_callbacks.get(export_name).cloned(),
                                exported_functions_are_host,
                                default_fuel,
                                *function_source_offsets.get(export_name).unwrap_or(&0),
                            ),
                        )
                    })
                    .collect();
                ModuleInner {
                    name,
                    functions,
                    memory,
                    globals,
                    all_globals,
                    close_notifier,
                    close_hook,
                    host_call_policy,
                    close_on_context_done,
                    closed: AtomicBool::new(false),
                    exit_code: AtomicU32::new(0),
                    default_fuel,
                    yield_policy,
                    runtime_registry,
                    runtime_store,
                    lower_module,
                    store_module_id,
                    import_aliases: Mutex::new(Vec::new()),
                    suspension: Mutex::new(ModuleSuspensionState::Ready),
                }
            }),
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.inner.name.as_deref()
    }

    pub fn memory(&self) -> Option<Memory> {
        self.inner.memory.clone()
    }

    pub(crate) fn store_module_id(&self) -> Option<ModuleInstanceId> {
        self.inner.store_module_id
    }

    pub(crate) fn runtime_store(&self) -> Option<Arc<Mutex<RuntimeStore>>> {
        self.inner.runtime_store.as_ref().and_then(Weak::upgrade)
    }

    pub(crate) fn lower_module(&self) -> Option<WasmModule> {
        self.inner.lower_module.clone()
    }

    pub fn exported_function(&self, name: &str) -> Option<Function> {
        self.inner.functions.get(name).cloned()
    }

    fn ensure_no_suspended_execution(&self) -> Result<()> {
        match *self
            .inner
            .suspension
            .lock()
            .expect("module suspension state poisoned")
        {
            ModuleSuspensionState::Ready => Ok(()),
            ModuleSuspensionState::Suspended { .. } | ModuleSuspensionState::Resuming { .. } => {
                Err(RuntimeError::new(
                    "cannot call: module has suspended execution; resume or cancel the outstanding Resumer first",
                ))
            }
        }
    }

    fn mark_suspended(&self, resumer_id: u64) {
        *self
            .inner
            .suspension
            .lock()
            .expect("module suspension state poisoned") =
            ModuleSuspensionState::Suspended { resumer_id };
    }

    fn begin_resume(&self, resumer_id: u64) -> Result<()> {
        let mut state = self
            .inner
            .suspension
            .lock()
            .expect("module suspension state poisoned");
        match *state {
            ModuleSuspensionState::Ready => Err(RuntimeError::new(
                "cannot resume: resumer has already been used",
            )),
            ModuleSuspensionState::Suspended {
                resumer_id: active_id,
            } if active_id == resumer_id => {
                *state = ModuleSuspensionState::Resuming { resumer_id };
                Ok(())
            }
            ModuleSuspensionState::Suspended { .. } => Err(RuntimeError::new(
                "cannot resume: resumer has already been used",
            )),
            ModuleSuspensionState::Resuming {
                resumer_id: active_id,
            } if active_id == resumer_id => Err(RuntimeError::new(
                "cannot resume: resumer is already being resumed",
            )),
            ModuleSuspensionState::Resuming { .. } => Err(RuntimeError::new(
                "cannot resume: resumer has already been used",
            )),
        }
    }

    fn finish_resume(&self, resumer_id: u64) {
        let mut state = self
            .inner
            .suspension
            .lock()
            .expect("module suspension state poisoned");
        if matches!(
            *state,
            ModuleSuspensionState::Resuming {
                resumer_id: active_id,
            } if active_id == resumer_id
        ) {
            *state = ModuleSuspensionState::Ready;
        }
    }

    fn cancel_resumer(&self, resumer_id: u64) -> bool {
        let mut state = self
            .inner
            .suspension
            .lock()
            .expect("module suspension state poisoned");
        if matches!(
            *state,
            ModuleSuspensionState::Suspended {
                resumer_id: active_id,
            } if active_id == resumer_id
        ) {
            *state = ModuleSuspensionState::Ready;
            true
        } else {
            false
        }
    }

    pub fn exported_function_definitions(&self) -> BTreeMap<String, FunctionDefinition> {
        self.inner
            .functions
            .iter()
            .map(|(name, function)| (name.clone(), function.definition().clone()))
            .collect()
    }

    pub fn imported_function_definitions(&self) -> Vec<FunctionDefinition> {
        let Some(mut lower_module) = self.inner.lower_module.clone() else {
            return Vec::new();
        };
        lower_module
            .imported_functions()
            .into_iter()
            .map(crate::runtime::convert_function_definition)
            .collect()
    }

    pub fn exported_memory(&self, name: &str) -> Option<Memory> {
        self.inner
            .memory
            .as_ref()
            .filter(|memory| {
                memory
                    .definition()
                    .export_names()
                    .iter()
                    .any(|export| export == name)
            })
            .cloned()
    }

    pub fn exported_memory_definitions(&self) -> BTreeMap<String, MemoryDefinition> {
        let Some(memory) = &self.inner.memory else {
            return BTreeMap::new();
        };
        memory
            .definition()
            .export_names()
            .iter()
            .map(|name| (name.clone(), memory.definition().clone()))
            .collect()
    }

    pub fn imported_memory_definitions(&self) -> Vec<MemoryDefinition> {
        let Some(lower_module) = self.inner.lower_module.clone() else {
            return Vec::new();
        };
        lower_module
            .imported_memories()
            .into_iter()
            .map(crate::runtime::convert_memory_definition)
            .collect()
    }

    pub fn imported_global_definitions(&self) -> Vec<GlobalDefinition> {
        let Some(lower_module) = self.inner.lower_module.as_ref() else {
            return Vec::new();
        };
        lower_module
            .import_section
            .iter()
            .filter_map(|import| match &import.desc {
                razero_wasm::module::ImportDesc::Global(global) => Some((import, global)),
                _ => None,
            })
            .map(|(import, global)| {
                GlobalDefinition::new(
                    crate::runtime::convert_value_type(global.val_type),
                    global.mutable,
                )
                .with_import(import.module.clone(), import.name.clone())
            })
            .collect()
    }

    pub fn imported_table_definitions(&self) -> Vec<TableDefinition> {
        let Some(lower_module) = self.inner.lower_module.as_ref() else {
            return Vec::new();
        };
        lower_module
            .import_section
            .iter()
            .filter_map(|import| match &import.desc {
                razero_wasm::module::ImportDesc::Table(table) => Some((import, table)),
                _ => None,
            })
            .map(|(import, table)| {
                TableDefinition::new(
                    crate::runtime::convert_ref_type(table.ty),
                    table.min,
                    table.max,
                )
                .with_import(import.module.clone(), import.name.clone())
            })
            .collect()
    }

    pub fn exported_global(&self, name: &str) -> Option<Global> {
        self.inner.globals.get(name).cloned()
    }

    pub fn exported_global_definitions(&self) -> BTreeMap<String, GlobalDefinition> {
        let Some(lower_module) = self.inner.lower_module.as_ref() else {
            return BTreeMap::new();
        };

        let mut definitions_by_index = BTreeMap::new();
        let module_name = self.name().map(str::to_owned);
        for export in lower_module
            .export_section
            .iter()
            .filter(|export| export.ty == razero_wasm::module::ExternType::GLOBAL)
        {
            let definition = definitions_by_index.entry(export.index).or_insert_with(|| {
                if export.index < lower_module.import_global_count {
                    lower_module
                        .import_section
                        .iter()
                        .filter_map(|import| match &import.desc {
                            razero_wasm::module::ImportDesc::Global(global) => {
                                Some((import, global))
                            }
                            _ => None,
                        })
                        .nth(export.index as usize)
                        .map(|(import, global)| {
                            GlobalDefinition::new(
                                crate::runtime::convert_value_type(global.val_type),
                                global.mutable,
                            )
                            .with_import(import.module.clone(), import.name.clone())
                        })
                        .unwrap_or_default()
                } else {
                    let local_index = (export.index - lower_module.import_global_count) as usize;
                    lower_module
                        .global_section
                        .get(local_index)
                        .map(|global| {
                            GlobalDefinition::new(
                                crate::runtime::convert_value_type(global.ty.val_type),
                                global.ty.mutable,
                            )
                            .with_module_name(module_name.clone())
                        })
                        .unwrap_or_default()
                }
            });
            *definition = definition.clone().with_export_name(export.name.clone());
        }

        lower_module
            .export_section
            .iter()
            .filter(|export| export.ty == razero_wasm::module::ExternType::GLOBAL)
            .filter_map(|export| {
                definitions_by_index
                    .get(&export.index)
                    .cloned()
                    .map(|definition| (export.name.clone(), definition))
            })
            .collect()
    }

    pub fn exported_table_definitions(&self) -> BTreeMap<String, TableDefinition> {
        let Some(lower_module) = self.inner.lower_module.as_ref() else {
            return BTreeMap::new();
        };

        let mut definitions_by_index = BTreeMap::new();
        let module_name = self.name().map(str::to_owned);
        for export in lower_module
            .export_section
            .iter()
            .filter(|export| export.ty == razero_wasm::module::ExternType::TABLE)
        {
            let definition = definitions_by_index.entry(export.index).or_insert_with(|| {
                if export.index < lower_module.import_table_count {
                    lower_module
                        .import_section
                        .iter()
                        .filter_map(|import| match &import.desc {
                            razero_wasm::module::ImportDesc::Table(table) => Some((import, table)),
                            _ => None,
                        })
                        .nth(export.index as usize)
                        .map(|(import, table)| {
                            TableDefinition::new(
                                crate::runtime::convert_ref_type(table.ty),
                                table.min,
                                table.max,
                            )
                            .with_import(import.module.clone(), import.name.clone())
                        })
                        .unwrap_or_default()
                } else {
                    let local_index = (export.index - lower_module.import_table_count) as usize;
                    lower_module
                        .table_section
                        .get(local_index)
                        .map(|table| {
                            TableDefinition::new(
                                crate::runtime::convert_ref_type(table.ty),
                                table.min,
                                table.max,
                            )
                            .with_module_name(module_name.clone())
                        })
                        .unwrap_or_default()
                }
            });
            *definition = definition.clone().with_export_name(export.name.clone());
        }

        lower_module
            .export_section
            .iter()
            .filter(|export| export.ty == razero_wasm::module::ExternType::TABLE)
            .filter_map(|export| {
                definitions_by_index
                    .get(&export.index)
                    .cloned()
                    .map(|definition| (export.name.clone(), definition))
            })
            .collect()
    }

    pub fn num_global(&self) -> usize {
        self.inner.all_globals.len()
    }

    pub fn global(&self, index: usize) -> Global {
        self.inner.all_globals[index].clone()
    }

    pub(crate) fn default_fuel(&self) -> i64 {
        self.inner.default_fuel
    }

    pub(crate) fn register_import_alias(&self, alias: &str) -> Result<()> {
        if alias.is_empty() {
            return Ok(());
        }
        let Some(module_id) = self.inner.store_module_id else {
            return Err(RuntimeError::new(
                "import resolver requires an instantiated module handle",
            ));
        };
        let Some(store) = self.inner.runtime_store.as_ref().and_then(Weak::upgrade) else {
            return Err(RuntimeError::new(
                "import resolver requires an active runtime store",
            ));
        };

        let mut store = store.lock().expect("runtime store poisoned");
        if let Some(existing) = store.name_to_module.get(alias) {
            if *existing == module_id {
                return Ok(());
            }
        }

        store.name_to_module.insert(alias.to_string(), module_id);
        let mut aliases = self
            .inner
            .import_aliases
            .lock()
            .expect("module alias list poisoned");
        if !aliases.iter().any(|existing| existing == alias) {
            aliases.push(alias.to_string());
        }
        Ok(())
    }

    pub(crate) fn lookup_table_function(
        &self,
        table_index: u32,
        table_offset: u32,
        expected_param_types: &[ValueType],
        expected_result_types: &[ValueType],
    ) -> std::result::Result<Function, LookupError> {
        let runtime_store = self
            .inner
            .runtime_store
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or(LookupError::TableIndexOutOfBounds(table_index))?;
        let module_id = self
            .inner
            .store_module_id
            .ok_or(LookupError::TableIndexOutOfBounds(table_index))?;

        let mut store = runtime_store.lock().expect("runtime store poisoned");
        let mut expected = WasmFunctionType::default();
        expected.params = expected_param_types
            .iter()
            .copied()
            .map(crate::runtime::to_wasm_value_type)
            .collect();
        expected.results = expected_result_types
            .iter()
            .copied()
            .map(crate::runtime::to_wasm_value_type)
            .collect();
        expected.cache_num_in_u64();
        let type_id =
            store
                .get_function_type_id(&mut expected)
                .map_err(|_| LookupError::TypeMismatch {
                    expected: u32::MAX,
                    actual: u32::MAX,
                })?;

        let instance = store
            .instance(module_id)
            .cloned()
            .ok_or(LookupError::TableIndexOutOfBounds(table_index))?;
        let function_index = instance
            .lookup_function(table_index, type_id, table_offset)?
            .function_index;
        let mut source = instance.source.clone();
        let definition =
            crate::runtime::convert_function_definition(source.function_definition(function_index));
        let callback = crate::runtime::guest_callback_for_function_index(
            runtime_store.clone(),
            module_id,
            function_index,
        );

        Ok(Function::new(
            Arc::downgrade(&self.inner),
            definition,
            Some(callback),
            false,
            self.default_fuel(),
            crate::runtime::function_source_offset(&instance.source, function_index),
        ))
    }

    pub fn close_with_exit_code(&self, ctx: &Context, exit_code: u32) -> Result<()> {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.inner.exit_code.store(exit_code, Ordering::SeqCst);
        if let (Some(module_id), Some(store)) = (
            self.inner.store_module_id,
            self.inner.runtime_store.as_ref().and_then(Weak::upgrade),
        ) {
            let aliases = self
                .inner
                .import_aliases
                .lock()
                .expect("module alias list poisoned")
                .clone();
            let mut store = store.lock().expect("runtime store poisoned");
            for alias in aliases {
                if store.name_to_module.get(&alias).copied() == Some(module_id) {
                    store.name_to_module.remove(&alias);
                }
            }
            store
                .close_module_with_exit_code(module_id, exit_code)
                .map_err(|err| RuntimeError::new(err.to_string()))?;
        }
        if let Some(notifier) = &self.inner.close_notifier {
            notifier.close_notify(ctx, exit_code);
        }
        if let Some(hook) = &self.inner.close_hook {
            hook(exit_code);
        }
        if let (Some(name), Some(registry)) = (&self.inner.name, &self.inner.runtime_registry) {
            if let Some(registry) = registry.upgrade() {
                registry
                    .lock()
                    .expect("runtime registry poisoned")
                    .remove(name);
            }
        }
        Ok(())
    }

    pub fn close(&self, ctx: &Context) -> Result<()> {
        self.close_with_exit_code(ctx, 0)
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    pub fn exit_code(&self) -> u32 {
        self.inner.exit_code.load(Ordering::SeqCst)
    }

    pub(crate) fn close_on_context_done(&self) -> bool {
        self.inner.close_on_context_done
    }

    pub(crate) fn host_call_policy(
        &self,
    ) -> Option<Arc<dyn crate::experimental::host_call_policy::HostCallPolicy>> {
        self.inner.host_call_policy.clone()
    }

    pub(crate) fn yield_policy(
        &self,
    ) -> Option<Arc<dyn crate::experimental::yield_policy::YieldPolicy>> {
        self.inner.yield_policy.clone()
    }
}

impl Display for Module {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.name().unwrap_or("<anonymous>"))
    }
}

struct ActiveSnapshotter {
    restored_results: Arc<Mutex<Option<Vec<u64>>>>,
    active: Arc<std::sync::atomic::AtomicBool>,
}

impl ActiveSnapshotter {
    fn new(
        restored_results: Arc<Mutex<Option<Vec<u64>>>>,
        active: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            restored_results,
            active,
        }
    }
}

impl Snapshotter for ActiveSnapshotter {
    fn snapshot(&self) -> Snapshot {
        Snapshot::new(self.restored_results.clone(), self.active.clone())
    }
}

struct ActiveYielder;

impl ActiveYielder {
    fn new() -> Self {
        Self
    }
}

impl Yielder for ActiveYielder {
    fn r#yield(&self) {
        if let Some((ctx, module)) = active_invocation() {
            let mut request = ctx
                .invocation
                .as_ref()
                .and_then(|invocation| invocation.function_definition.clone())
                .map_or_else(YieldPolicyRequest::new, |function| {
                    YieldPolicyRequest::new().with_function(function)
                });
            if let Some(module_name) = module.name() {
                request = request.with_caller_module_name(module_name);
            }
            if let Some(memory) = module.memory() {
                request = request.with_memory(memory.definition().clone());
            }
            let policy = ctx.yield_policy.clone().or_else(|| module.yield_policy());
            if policy
                .as_ref()
                .is_some_and(|policy| !policy.allow_yield(&ctx, &request))
            {
                panic::panic_any(PolicyDeniedPayload("cooperative yield"));
            }
        }
        panic::panic_any(YieldSuspend);
    }
}

struct PendingResumer {
    id: u64,
    module: Module,
    definition: FunctionDefinition,
    listener: Option<Arc<dyn FunctionListener>>,
    listener_stack: Vec<StackFrame>,
    fuel_controller: Option<Arc<dyn FuelController>>,
    fuel_remaining: Option<Arc<std::sync::atomic::AtomicI64>>,
    reported_remaining: std::sync::atomic::AtomicI64,
    state: Mutex<PendingResumerState>,
}

impl PendingResumer {
    fn new(
        id: u64,
        module: Module,
        definition: FunctionDefinition,
        listener: Option<Arc<dyn FunctionListener>>,
        listener_stack: Vec<StackFrame>,
        fuel_controller: Option<Arc<dyn FuelController>>,
        fuel_remaining: Option<Arc<std::sync::atomic::AtomicI64>>,
    ) -> Self {
        let reported_remaining = fuel_remaining
            .as_ref()
            .map(|remaining| remaining.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or_default();
        Self {
            id,
            module,
            definition,
            listener,
            listener_stack,
            fuel_controller,
            fuel_remaining,
            reported_remaining: std::sync::atomic::AtomicI64::new(reported_remaining),
            state: Mutex::new(PendingResumerState::Pending {
                suspended: None,
                cancelled: false,
            }),
        }
    }

    fn install_suspended_invocation(&self, suspended: Arc<dyn SuspendedInvocation>) {
        let mut state = self.state.lock().expect("resumer state poisoned");
        let PendingResumerState::Pending {
            suspended: slot, ..
        } = &mut *state;
        *slot = Some(suspended);
    }

    fn note_fuel_checkpoint(&self) {
        if let Some(remaining) = &self.fuel_remaining {
            self.reported_remaining.store(
                remaining.load(std::sync::atomic::Ordering::SeqCst),
                std::sync::atomic::Ordering::SeqCst,
            );
        }
    }

    fn consume_incremental_fuel(&self) {
        if let (Some(controller), Some(remaining)) = (&self.fuel_controller, &self.fuel_remaining) {
            let current = remaining.load(std::sync::atomic::Ordering::SeqCst);
            let previous = self
                .reported_remaining
                .swap(current, std::sync::atomic::Ordering::SeqCst);
            controller.consumed((previous - current).max(0));
        }
    }

    fn expected_host_result_count(
        &self,
        suspended: Option<&Arc<dyn SuspendedInvocation>>,
    ) -> usize {
        suspended
            .and_then(|suspended| suspended.expected_host_result_count())
            .unwrap_or_else(|| self.definition.result_types().len())
    }
}

impl Resumer for PendingResumer {
    fn resume(&self, ctx: &Context, host_results: &[u64]) -> Result<Vec<u64>> {
        if self.module.is_closed() {
            return Err(ExitError::new(self.module.exit_code()).into());
        }
        let suspended = {
            let state = self.state.lock().expect("resumer state poisoned");
            match &*state {
                PendingResumerState::Pending {
                    suspended,
                    cancelled: false,
                } => suspended.clone(),
                PendingResumerState::Pending {
                    cancelled: true, ..
                } => {
                    return Err(RuntimeError::new(
                        "cannot resume: resumer has been cancelled",
                    ));
                }
            }
        };
        let expected_results = self.expected_host_result_count(suspended.as_ref());
        if host_results.len() != expected_results {
            return Err(RuntimeError::new(format!(
                "cannot resume: expected {} host results, but got {}",
                expected_results,
                host_results.len()
            )));
        }
        self.module.begin_resume(self.id)?;

        if let Some(suspended) = suspended {
            let yielder = ctx
                .yielder_enabled
                .then(|| Arc::new(ActiveYielder::new()) as Arc<dyn Yielder>);
            let invocation_ctx = ctx.with_invocation(InvocationContext {
                fuel_remaining: self.fuel_remaining.clone(),
                snapshotter: None,
                yielder,
                function_listener: self.listener.clone(),
                function_definition: Some(self.definition.clone()),
                listener_stack: self.listener_stack.clone(),
            });
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                with_active_invocation(&invocation_ctx, &self.module, || {
                    with_active_fuel_remaining(self.fuel_remaining.clone(), || {
                        suspended.resume(host_results)
                    })
                })
            }));
            match outcome {
                Ok(Ok(results)) => {
                    self.module.finish_resume(self.id);
                    if let Some(listener) = &self.listener {
                        listener.after(&invocation_ctx, &self.module, &self.definition, &results);
                    }
                    self.consume_incremental_fuel();
                    Ok(results)
                }
                Ok(Err(error)) => {
                    self.module.finish_resume(self.id);
                    let error = RuntimeError::new(error.to_string());
                    if let Some(listener) = &self.listener {
                        listener.abort(&invocation_ctx, &self.module, &self.definition, &error);
                    }
                    self.consume_incremental_fuel();
                    Err(error)
                }
                Err(payload) => {
                    if is_yield_suspend_payload(payload.as_ref()) {
                        self.consume_incremental_fuel();
                        let next = Arc::new(PendingResumer::new(
                            next_resumer_id(),
                            self.module.clone(),
                            self.definition.clone(),
                            self.listener.clone(),
                            self.listener_stack.clone(),
                            self.fuel_controller.clone(),
                            self.fuel_remaining.clone(),
                        ));
                        next.note_fuel_checkpoint();
                        if let Some(suspended) = take_suspended_invocation() {
                            next.install_suspended_invocation(suspended);
                        }
                        self.module.mark_suspended(next.id);
                        let next: Arc<dyn Resumer> = next;
                        return Err(RuntimeError::from(YieldError::new(Some(next))));
                    }
                    if let Some(reason) = policy_denied_reason(payload.as_ref()) {
                        self.module.finish_resume(self.id);
                        self.consume_incremental_fuel();
                        let err = policy_denied_error(reason);
                        notify_trap_observer(&invocation_ctx, &self.module, &err);
                        return Err(err);
                    }
                    self.module.finish_resume(self.id);
                    panic::resume_unwind(payload);
                }
            }
        } else {
            self.module.finish_resume(self.id);
            if let Some(listener) = &self.listener {
                listener.after(ctx, &self.module, &self.definition, host_results);
            }
            self.consume_incremental_fuel();
            Ok(host_results.to_vec())
        }
    }

    fn cancel(&self) {
        if !self.module.cancel_resumer(self.id) {
            return;
        }
        let suspended = {
            let mut state = self.state.lock().expect("resumer state poisoned");
            match std::mem::replace(
                &mut *state,
                PendingResumerState::Pending {
                    suspended: None,
                    cancelled: true,
                },
            ) {
                PendingResumerState::Pending {
                    suspended,
                    cancelled: false,
                } => suspended,
                other => {
                    *state = other;
                    return;
                }
            }
        };
        if let Some(suspended) = suspended {
            suspended.cancel();
        }
    }
}

enum PendingResumerState {
    Pending {
        suspended: Option<Arc<dyn SuspendedInvocation>>,
        cancelled: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        decode_externref, decode_f32, decode_f64, decode_i32, decode_u32, encode_externref,
        encode_f32, encode_f64, encode_i32, encode_i64, encode_u32, extern_type_name,
        value_type_name, ExternType, FunctionDefinition, Global, GlobalDefinition, GlobalValue,
        MemoryDefinition, TableDefinition, ValueType,
    };
    use std::{
        f32, f64,
        panic::{catch_unwind, AssertUnwindSafe},
    };

    #[test]
    fn function_definition_tracks_signature() {
        let definition = FunctionDefinition::new("sum")
            .with_signature(vec![ValueType::I32, ValueType::I32], vec![ValueType::I32])
            .with_export_name("sum");
        assert_eq!("sum", definition.name());
        assert_eq!(2, definition.param_types().len());
        assert_eq!(1, definition.result_types().len());
    }

    #[test]
    fn function_definition_tracks_module_and_import_metadata() {
        let definition = FunctionDefinition::new("host_call")
            .with_module_name(Some("env".to_string()))
            .with_import("env", "call_handler")
            .with_export_name("call");

        assert_eq!(Some("env"), definition.module_name());
        assert_eq!(Some(("env", "call_handler")), definition.import());
        assert_eq!(&["call".to_string()], definition.export_names());
    }

    #[test]
    fn memory_definition_tracks_module_and_import_metadata() {
        let definition = MemoryDefinition::new(1, Some(2))
            .with_module_name(Some("guest".to_string()))
            .with_import("env", "memory")
            .with_export_name("memory");

        assert_eq!(Some("guest"), definition.module_name());
        assert_eq!(Some(("env", "memory")), definition.import());
        assert_eq!(&["memory".to_string()], definition.export_names());
    }

    #[test]
    fn global_definition_tracks_metadata() {
        let definition = GlobalDefinition::new(ValueType::I64, true)
            .with_module_name(Some("guest".to_string()))
            .with_import("env", "counter")
            .with_export_name("counter");

        assert_eq!(ValueType::I64, definition.value_type());
        assert!(definition.is_mutable());
        assert_eq!(Some("guest"), definition.module_name());
        assert_eq!(Some(("env", "counter")), definition.import());
        assert_eq!(&["counter".to_string()], definition.export_names());
    }

    #[test]
    fn table_definition_tracks_metadata() {
        let definition = TableDefinition::new(ValueType::FuncRef, 1, Some(2))
            .with_module_name(Some("guest".to_string()))
            .with_import("env", "table")
            .with_export_name("table");

        assert_eq!(ValueType::FuncRef, definition.ref_type());
        assert_eq!(1, definition.minimum());
        assert_eq!(Some(2), definition.maximum());
        assert_eq!(Some("guest"), definition.module_name());
        assert_eq!(Some(("env", "table")), definition.import());
        assert_eq!(&["table".to_string()], definition.export_names());
    }

    #[test]
    fn mutable_globals_round_trip_through_public_api() {
        let global = Global::new(GlobalValue::I64(1), true);
        assert_eq!(GlobalValue::I64(1), global.get());

        global.set(GlobalValue::I64(2));

        assert_eq!(GlobalValue::I64(2), global.get());
    }

    #[test]
    fn immutable_globals_reject_setters() {
        let global = Global::new(GlobalValue::I64(1), false);

        let err = catch_unwind(AssertUnwindSafe(|| global.set(GlobalValue::I64(2))))
            .expect_err("immutable globals must reject mutation");

        let message = err
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| err.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic>");
        assert!(message.contains("immutable"));
    }

    #[test]
    fn extern_type_names_match_wasm_text_format() {
        assert_eq!("func", extern_type_name(ExternType::Func));
        assert_eq!("table", extern_type_name(ExternType::Table));
        assert_eq!("memory", extern_type_name(ExternType::Memory));
        assert_eq!("global", extern_type_name(ExternType::Global));
    }

    #[test]
    fn value_type_names_match_wasm_text_format() {
        assert_eq!("i32", value_type_name(ValueType::I32));
        assert_eq!("i64", value_type_name(ValueType::I64));
        assert_eq!("f32", value_type_name(ValueType::F32));
        assert_eq!("f64", value_type_name(ValueType::F64));
        assert_eq!("externref", value_type_name(ValueType::ExternRef));
        assert_eq!("funcref", value_type_name(ValueType::FuncRef));
    }

    #[test]
    fn externref_round_trips() {
        for value in [0usize, 0x1234_5678usize] {
            assert_eq!(value, decode_externref(encode_externref(value)));
        }
    }

    #[test]
    fn f32_round_trips_and_keeps_upper_bits_clear() {
        for value in [
            0.0f32,
            100.0,
            -100.0,
            100.01234,
            f32::MAX,
            f32::MIN_POSITIVE,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
        ] {
            let encoded = encode_f32(value);
            assert_eq!(0, encoded >> 32);
            let decoded = decode_f32(encoded);
            if value.is_nan() {
                assert!(decoded.is_nan());
            } else {
                assert_eq!(value, decoded);
            }
        }
    }

    #[test]
    fn f64_round_trips() {
        for value in [
            0.0f64,
            100.0,
            -100.0,
            100.01234124,
            f64::MAX,
            f64::MIN_POSITIVE,
            (1u64 << 36) as f64,
            (1u64 << 37) as f64,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ] {
            let decoded = decode_f64(encode_f64(value));
            if value.is_nan() {
                assert!(decoded.is_nan());
            } else {
                assert_eq!(value, decoded);
            }
        }
    }

    #[test]
    fn i32_encoding_matches_low_word_go_semantics() {
        for value in [0i32, 100, -100, 1, -1, i32::MAX, i32::MIN] {
            let encoded = encode_i32(value);
            assert_eq!(0, encoded >> 32);
            assert_eq!(value, decode_i32(encoded));
        }
    }

    #[test]
    fn decode_i32_ignores_high_bits() {
        let cases = [
            (0u64, 0i32),
            (1u64 << 60, 0i32),
            (1u64 << 30, 1i32 << 30),
            ((1u64 << 30) | (1u64 << 60), 1i32 << 30),
            ((i32::MIN as u32 as u64) | (1u64 << 59), i32::MIN),
            ((i32::MAX as u32 as u64) | (1u64 << 50), i32::MAX),
        ];
        for (input, expected) in cases {
            assert_eq!(expected, decode_i32(input));
        }
    }

    #[test]
    fn u32_encoding_matches_low_word_go_semantics() {
        for value in [0u32, 100, 1, 1 << 31, i32::MAX as u32, u32::MAX] {
            let encoded = encode_u32(value);
            assert_eq!(0, encoded >> 32);
            assert_eq!(value, decode_u32(encoded));
        }
    }

    #[test]
    fn decode_u32_ignores_high_bits() {
        let cases = [
            (0u64, 0u32),
            (1u64 << 60, 0u32),
            (1u64 << 30, 1u32 << 30),
            ((1u64 << 30) | (1u64 << 60), 1u32 << 30),
            ((i32::MIN as u32 as u64) | (1u64 << 59), i32::MIN as u32),
            ((i32::MAX as u32 as u64) | (1u64 << 50), i32::MAX as u32),
        ];
        for (input, expected) in cases {
            assert_eq!(expected, decode_u32(input));
        }
    }

    #[test]
    fn i64_encoding_is_bit_preserving() {
        for value in [0i64, 100, -100, 1, -1, i64::MAX, i64::MIN] {
            assert_eq!(value, encode_i64(value) as i64);
        }
    }
}
