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
        listener::FunctionListener,
        memory::LinearMemory,
        r#yield::{Resumer, YieldError, YieldSuspend, Yielder},
        snapshotter::{Snapshot, Snapshotter},
    },
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
        write_u32_le: impl Fn(u32, u32) -> bool + Send + Sync + 'static,
        grow: impl Fn(u32, Option<u32>) -> Option<u32> + Send + Sync + 'static,
    ) -> Self {
        Self {
            definition,
            access: Arc::new(DynamicMemoryAccess {
                size: Arc::new(size),
                read: Arc::new(read),
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
}

impl Function {
    pub(crate) fn new(
        module: Weak<ModuleInner>,
        definition: FunctionDefinition,
        callback: Option<HostCallback>,
        default_fuel: i64,
    ) -> Self {
        Self {
            inner: Arc::new(FunctionInner {
                module,
                definition,
                callback,
                default_fuel,
            }),
        }
    }

    pub fn name(&self) -> &str {
        self.inner.definition.name()
    }

    pub fn definition(&self) -> &FunctionDefinition {
        &self.inner.definition
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
        let snapshotter = ctx.snapshotter_enabled.then(|| {
            Arc::new(ActiveSnapshotter::new(restored_results.clone())) as Arc<dyn Snapshotter>
        });

        let resumer = Arc::new(PendingResumer::new(
            module.clone(),
            self.inner.definition.clone(),
            listener.clone(),
            ctx.fuel_controller.clone(),
            fuel_remaining.clone(),
        ));
        let yielder = ctx
            .yielder_enabled
            .then(|| Arc::new(ActiveYielder::new(resumer.clone())) as Arc<dyn Yielder>);
        let invocation_ctx = ctx.with_invocation(InvocationContext {
            fuel_remaining: fuel_remaining.clone(),
            snapshotter,
            yielder,
        });

        if let Some(listener) = &listener {
            listener.before(&invocation_ctx, &module, &self.inner.definition, params);
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
                    return Err(RuntimeError::new("fuel exhausted"));
                }
                Ok(results)
            }
            Ok(Err(error)) => {
                if let Some(listener) = &listener {
                    listener.abort(&invocation_ctx, &module, &self.inner.definition, &error);
                }
                consume_fuel(&ctx.fuel_controller, &fuel_remaining);
                Err(error)
            }
            Err(payload) => {
                if let Some(suspend) = payload.downcast_ref::<YieldSuspend>() {
                    consume_fuel(&ctx.fuel_controller, &fuel_remaining);
                    return Err(RuntimeError::from(YieldError::new(suspend.resumer.clone())));
                }
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
    close_notifier: Option<Arc<dyn CloseNotifier>>,
    close_hook: Option<Arc<dyn Fn(u32) + Send + Sync>>,
    closed: AtomicBool,
    exit_code: AtomicU32,
    runtime_registry: Option<Weak<Mutex<BTreeMap<String, Module>>>>,
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
        close_notifier: Option<Arc<dyn CloseNotifier>>,
        close_hook: Option<Arc<dyn Fn(u32) + Send + Sync>>,
        runtime_registry: Option<Weak<Mutex<BTreeMap<String, Module>>>>,
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
                            ),
                        )
                    })
                    .collect();
                ModuleInner {
                    name,
                    functions,
                    memory,
                    globals,
                    close_notifier,
                    close_hook,
                    closed: AtomicBool::new(false),
                    exit_code: AtomicU32::new(0),
                    runtime_registry,
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

    pub fn close_with_exit_code(&self, ctx: &Context, exit_code: u32) -> Result<()> {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.inner.exit_code.store(exit_code, Ordering::SeqCst);
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
}

impl ActiveSnapshotter {
    fn new(restored_results: Arc<Mutex<Option<Vec<u64>>>>) -> Self {
        Self { restored_results }
    }
}

impl Snapshotter for ActiveSnapshotter {
    fn snapshot(&self) -> Snapshot {
        Snapshot::new(self.restored_results.clone())
    }
}

struct ActiveYielder {
    resumer: Arc<dyn Resumer>,
}

impl ActiveYielder {
    fn new(resumer: Arc<dyn Resumer>) -> Self {
        Self { resumer }
    }
}

impl Yielder for ActiveYielder {
    fn r#yield(&self) {
        panic::panic_any(YieldSuspend {
            resumer: self.resumer.clone(),
        });
    }
}

struct PendingResumer {
    module: Module,
    definition: FunctionDefinition,
    listener: Option<Arc<dyn FunctionListener>>,
    fuel_controller: Option<Arc<dyn FuelController>>,
    fuel_remaining: Option<Arc<std::sync::atomic::AtomicI64>>,
    completed: AtomicBool,
}

impl PendingResumer {
    fn new(
        module: Module,
        definition: FunctionDefinition,
        listener: Option<Arc<dyn FunctionListener>>,
        fuel_controller: Option<Arc<dyn FuelController>>,
        fuel_remaining: Option<Arc<std::sync::atomic::AtomicI64>>,
    ) -> Self {
        Self {
            module,
            definition,
            listener,
            fuel_controller,
            fuel_remaining,
            completed: AtomicBool::new(false),
        }
    }
}

impl Resumer for PendingResumer {
    fn resume(&self, ctx: &Context, host_results: &[u64]) -> Result<Vec<u64>> {
        if self.completed.swap(true, Ordering::SeqCst) {
            return Err(RuntimeError::new("resumer already completed"));
        }
        if let Some(listener) = &self.listener {
            listener.after(ctx, &self.module, &self.definition, host_results);
        }
        if let (Some(controller), Some(remaining)) = (&self.fuel_controller, &self.fuel_remaining) {
            controller.consumed(remaining.load(std::sync::atomic::Ordering::SeqCst).max(0));
        }
        Ok(host_results.to_vec())
    }

    fn cancel(&self) {
        self.completed.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::{FunctionDefinition, ValueType};

    #[test]
    fn function_definition_tracks_signature() {
        let definition = FunctionDefinition::new("sum")
            .with_signature(vec![ValueType::I32, ValueType::I32], vec![ValueType::I32])
            .with_export_name("sum");
        assert_eq!("sum", definition.name());
        assert_eq!(2, definition.param_types().len());
        assert_eq!(1, definition.result_types().len());
    }
}
