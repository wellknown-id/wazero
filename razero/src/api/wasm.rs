use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt::{self, Display, Formatter},
    panic::{self, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex, Weak,
    },
};

use crate::{
    api::error::{ExitError, Result, RuntimeError},
    ctx_keys::{Context, InvocationContext},
    experimental::{
        close_notifier::CloseNotifier,
        fuel::FuelController,
        listener::{new_stack_iterator, FunctionListener, StackFrame},
        memory::LinearMemory,
        r#yield::{Resumer, YieldError, Yielder},
        snapshotter::{Snapshot, Snapshotter},
    },
};
use razero_interp::{
    engine::{take_suspended_invocation, InterpEngine, SuspendedInvocation},
    interpreter::{is_yield_suspend_payload, YieldSuspend},
};
use razero_wasm::{
    module::FunctionType as WasmFunctionType, module_instance_lookup::LookupError,
    store::Store as WasmStore, store_module_list::ModuleInstanceId,
};

pub type HostCallback =
    Arc<dyn Fn(Context, Module, &[u64]) -> Result<Vec<u64>> + Send + Sync + 'static>;

pub type RuntimeModuleRegistry = Arc<Mutex<BTreeMap<String, Module>>>;

thread_local! {
    static ACTIVE_INVOCATIONS: RefCell<Vec<(Context, Module)>> = const { RefCell::new(Vec::new()) };
}

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

    pub fn export_names(&self) -> &[String] {
        &self.export_names
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

    fn is_mutable(&self) -> bool {
        self.mutable
    }
}

struct DynamicGlobalAccess {
    getter: Arc<dyn Fn() -> GlobalValue + Send + Sync>,
    mutable: bool,
}

impl GlobalAccess for DynamicGlobalAccess {
    fn get(&self) -> GlobalValue {
        (self.getter)()
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

    pub(crate) fn dynamic(
        mutable: bool,
        getter: impl Fn() -> GlobalValue + Send + Sync + 'static,
    ) -> Self {
        Self {
            access: Arc::new(DynamicGlobalAccess {
                getter: Arc::new(getter),
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

    pub fn write_u32_le(&self, offset: u32, value: u32) -> bool {
        self.access.write_u32_le(offset, value)
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
    default_fuel: i64,
    source_offset: u64,
}

impl Function {
    pub(crate) fn new(
        module: Weak<ModuleInner>,
        definition: FunctionDefinition,
        callback: Option<HostCallback>,
        default_fuel: i64,
        source_offset: u64,
    ) -> Self {
        Self {
            inner: Arc::new(FunctionInner {
                module,
                definition,
                callback,
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

        let callback = self.inner.callback.clone().ok_or_else(|| {
            RuntimeError::new("guest function execution is not yet wired through the public API")
        })?;

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
        if let Some(remaining) = &fuel_remaining {
            remaining.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
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
        let resumer = Arc::new(PendingResumer::new(
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
            callback(invocation_ctx.clone(), module.clone(), params)
        }));

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
                    snapshot_active.store(false, std::sync::atomic::Ordering::SeqCst);
                    let resumer: Arc<dyn Resumer> = resumer;
                    return Err(RuntimeError::from(YieldError::new(Some(resumer))));
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
    closed: AtomicBool,
    exit_code: AtomicU32,
    default_fuel: i64,
    runtime_registry: Option<Weak<Mutex<BTreeMap<String, Module>>>>,
    runtime_store: Option<Weak<Mutex<WasmStore<InterpEngine>>>>,
    store_module_id: Option<ModuleInstanceId>,
    import_aliases: Mutex<Vec<String>>,
}

pub type Instance = Module;

impl Module {
    pub(crate) fn new(
        name: Option<String>,
        exported_functions: BTreeMap<String, FunctionDefinition>,
        host_callbacks: BTreeMap<String, HostCallback>,
        default_fuel: i64,
        memory: Option<Memory>,
        globals: BTreeMap<String, Global>,
        all_globals: Vec<Global>,
        close_notifier: Option<Arc<dyn CloseNotifier>>,
        close_hook: Option<Arc<dyn Fn(u32) + Send + Sync>>,
        runtime_registry: Option<Weak<Mutex<BTreeMap<String, Module>>>>,
        runtime_store: Option<Weak<Mutex<WasmStore<InterpEngine>>>>,
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
                    closed: AtomicBool::new(false),
                    exit_code: AtomicU32::new(0),
                    default_fuel,
                    runtime_registry,
                    runtime_store,
                    store_module_id,
                    import_aliases: Mutex::new(Vec::new()),
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

    pub(crate) fn runtime_store(&self) -> Option<Arc<Mutex<WasmStore<InterpEngine>>>> {
        self.inner.runtime_store.as_ref().and_then(Weak::upgrade)
    }

    pub fn exported_function(&self, name: &str) -> Option<Function> {
        self.inner.functions.get(name).cloned()
    }

    pub fn exported_function_definitions(&self) -> BTreeMap<String, FunctionDefinition> {
        self.inner
            .functions
            .iter()
            .map(|(name, function)| (name.clone(), function.definition().clone()))
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

    pub fn exported_global(&self, name: &str) -> Option<Global> {
        self.inner.globals.get(name).cloned()
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
        panic::panic_any(YieldSuspend);
    }
}

struct PendingResumer {
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
        if let PendingResumerState::Pending {
            suspended: slot, ..
        } = &mut *state
        {
            *slot = Some(suspended);
        }
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
}

impl Resumer for PendingResumer {
    fn resume(&self, ctx: &Context, host_results: &[u64]) -> Result<Vec<u64>> {
        let suspended = {
            let mut state = self.state.lock().expect("resumer state poisoned");
            match std::mem::replace(&mut *state, PendingResumerState::Completed) {
                PendingResumerState::Pending {
                    suspended,
                    cancelled: false,
                } => suspended,
                PendingResumerState::Pending {
                    cancelled: true, ..
                } => {
                    return Err(RuntimeError::new(
                        "cannot resume: resumer has been cancelled",
                    ));
                }
                PendingResumerState::Completed => {
                    return Err(RuntimeError::new("resumer already completed"));
                }
            }
        };

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
                    suspended.resume(host_results)
                })
            }));
            match outcome {
                Ok(Ok(results)) => {
                    if let Some(listener) = &self.listener {
                        listener.after(&invocation_ctx, &self.module, &self.definition, &results);
                    }
                    self.consume_incremental_fuel();
                    Ok(results)
                }
                Ok(Err(error)) => {
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
                        let next: Arc<dyn Resumer> = next;
                        return Err(RuntimeError::from(YieldError::new(Some(next))));
                    }
                    panic::resume_unwind(payload);
                }
            }
        } else {
            if let Some(listener) = &self.listener {
                listener.after(ctx, &self.module, &self.definition, host_results);
            }
            self.consume_incremental_fuel();
            Ok(host_results.to_vec())
        }
    }

    fn cancel(&self) {
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
    Completed,
}

#[cfg(test)]
mod tests {
    use super::{
        decode_externref, decode_f32, decode_f64, decode_i32, decode_u32, encode_externref,
        encode_f32, encode_f64, encode_i32, encode_i64, encode_u32, extern_type_name,
        value_type_name, ExternType, FunctionDefinition, ValueType,
    };
    use std::{f32, f64};

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
