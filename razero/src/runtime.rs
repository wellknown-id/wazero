use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use razero_decoder::decoder::decode_module;
use razero_interp::engine::InterpModuleEngine;
use razero_secmem::GuardPageAllocator;
use razero_wasm::{
    function_definition::FunctionDefinition as WasmFunctionDefinition,
    memory_definition::MemoryDefinition as WasmMemoryDefinition,
    module::{ExternType as WasmExternType, Module as WasmModule, ValueType as WasmValueType},
    store::Store as WasmStore,
    store_module_list::ModuleInstanceId,
};

use crate::{
    api::{
        error::{ExitError, Result, RuntimeError},
        wasm::{
            active_invocation, with_active_invocation, CustomSection, FunctionDefinition, Global,
            GlobalValue, HostCallback, Memory, MemoryDefinition, Module, RuntimeModuleRegistry,
            ValueType,
        },
    },
    builder::HostModuleBuilder,
    config::{CompiledModule, CompiledModuleInner, ModuleConfig, RuntimeConfig},
    ctx_keys::Context,
    experimental::memory::{DefaultMemoryAllocator, MemoryAllocator},
};

#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    config: RuntimeConfig,
    modules: RuntimeModuleRegistry,
    store: Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    closed: AtomicBool,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::with_config(RuntimeConfig::new())
    }
}

impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: RuntimeConfig) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                config,
                modules: Arc::new(Mutex::new(BTreeMap::new())),
                store: Arc::new(Mutex::new(WasmStore::new(
                    razero_interp::engine::InterpEngine::new(),
                ))),
                closed: AtomicBool::new(false),
            }),
        }
    }

    pub fn config(&self) -> &RuntimeConfig {
        &self.inner.config
    }

    pub fn compile(&self, bytes: &[u8]) -> Result<CompiledModule> {
        self.fail_if_closed()?;
        if let Some(cache) = self.inner.config.compilation_cache() {
            let key = cache_key(bytes);
            if let Some(cached) = cache.get(&key) {
                return compile_binary_module(&cached, &self.inner.config);
            }
            let compiled = compile_binary_module(bytes, &self.inner.config)?;
            cache.insert(&key, bytes);
            return Ok(compiled);
        }
        compile_binary_module(bytes, &self.inner.config)
    }

    pub fn instantiate(&self, compiled: &CompiledModule, config: ModuleConfig) -> Result<Module> {
        self.instantiate_with_context(&Context::default(), compiled, config)
    }

    pub fn instantiate_with_context(
        &self,
        ctx: &Context,
        compiled: &CompiledModule,
        config: ModuleConfig,
    ) -> Result<Module> {
        self.fail_if_closed()?;
        if compiled.is_closed() {
            return Err(RuntimeError::new("compiled module is closed"));
        }
        let name = if config.name_set() {
            config.name().map(ToOwned::to_owned)
        } else {
            compiled.name().map(ToOwned::to_owned)
        };
        if let Some(name) = &name {
            let modules = self.inner.modules.lock().expect("runtime modules poisoned");
            if modules.contains_key(name) {
                return Err(RuntimeError::new(format!(
                    "module[{name}] has already been instantiated"
                )));
            }
        }

        let module = if compiled.inner().host_callbacks.is_empty() {
            self.instantiate_guest_module(ctx, compiled, name.clone())?
        } else {
            self.instantiate_host_module(ctx, compiled, name.clone())?
        };

        if let Some(name) = name {
            self.inner
                .modules
                .lock()
                .expect("runtime modules poisoned")
                .insert(name, module.clone());
        }
        Ok(module)
    }

    pub fn instantiate_binary(&self, bytes: &[u8], config: ModuleConfig) -> Result<Module> {
        let compiled = self.compile(bytes)?;
        self.instantiate(&compiled, config)
    }

    pub fn new_host_module_builder(&self, module_name: impl Into<String>) -> HostModuleBuilder {
        HostModuleBuilder::attached(self.clone(), module_name)
    }

    pub fn module(&self, module_name: &str) -> Option<Module> {
        self.inner
            .modules
            .lock()
            .expect("runtime modules poisoned")
            .get(module_name)
            .cloned()
    }

    pub fn close(&self, ctx: &Context) -> Result<()> {
        self.close_with_exit_code(ctx, 0)
    }

    pub fn close_with_exit_code(&self, ctx: &Context, exit_code: u32) -> Result<()> {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let modules = self
            .inner
            .modules
            .lock()
            .expect("runtime modules poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for module in modules {
            module.close_with_exit_code(ctx, exit_code)?;
        }
        self.inner
            .modules
            .lock()
            .expect("runtime modules poisoned")
            .clear();
        self.inner
            .store
            .lock()
            .expect("runtime store poisoned")
            .close_with_exit_code(exit_code)
            .map_err(|err| RuntimeError::new(err.to_string()))?;
        Ok(())
    }

    fn fail_if_closed(&self) -> Result<()> {
        if self.inner.closed.load(Ordering::SeqCst) {
            Err(ExitError::new(0).into())
        } else {
            Ok(())
        }
    }

    fn instantiate_host_module(
        &self,
        ctx: &Context,
        compiled: &CompiledModule,
        name: Option<String>,
    ) -> Result<Module> {
        let store_module_id = compiled
            .inner()
            .lower_module
            .clone()
            .map(|module| instantiate_in_store(&self.inner.store, module, name.as_deref()))
            .transpose()?;
        let close_hook = store_module_id.map(|module_id| {
            let store = self.inner.store.clone();
            Arc::new(move |_exit_code| {
                let _ = delete_from_store(&store, module_id);
            }) as Arc<dyn Fn(u32) + Send + Sync>
        });
        Ok(Module::new(
            name,
            compiled.inner().exported_functions.clone(),
            compiled.inner().host_callbacks.clone(),
            self.inner.config.fuel(),
            None,
            compiled.inner().exported_globals.clone(),
            ctx.close_notifier.clone(),
            close_hook,
            Some(Arc::downgrade(&self.inner.modules)),
        ))
    }

    fn instantiate_guest_module(
        &self,
        ctx: &Context,
        compiled: &CompiledModule,
        name: Option<String>,
    ) -> Result<Module> {
        let lower_module =
            compiled.inner().lower_module.clone().ok_or_else(|| {
                RuntimeError::new("compiled module is missing lower runtime state")
            })?;
        let module_id =
            instantiate_in_store(&self.inner.store, lower_module.clone(), name.as_deref())?;
        let callbacks = build_guest_callbacks(&self.inner.store, module_id, &lower_module)?;
        let memory = compiled
            .inner()
            .exported_memories
            .values()
            .next()
            .cloned()
            .map(|definition| guest_memory(self.inner.store.clone(), module_id, definition));
        let globals = guest_globals(&self.inner.store, module_id, &lower_module)?;
        let close_hook = Some({
            let store = self.inner.store.clone();
            Arc::new(move |_exit_code| {
                let _ = delete_from_store(&store, module_id);
            }) as Arc<dyn Fn(u32) + Send + Sync>
        });
        Ok(Module::new(
            name,
            compiled.inner().exported_functions.clone(),
            callbacks,
            self.inner.config.fuel(),
            memory,
            globals,
            ctx.close_notifier.clone(),
            close_hook,
            Some(Arc::downgrade(&self.inner.modules)),
        ))
    }
}

#[allow(dead_code)]
fn instantiate_memory(
    ctx: &Context,
    definition: Option<MemoryDefinition>,
    config: &RuntimeConfig,
) -> Result<Option<Memory>> {
    let Some(definition) = definition else {
        return Ok(None);
    };
    let allocator: Arc<dyn MemoryAllocator> = if let Some(allocator) = ctx.memory_allocator.clone()
    {
        allocator
    } else if config.secure_mode() {
        Arc::new(GuardPageMemoryAllocator)
    } else {
        Arc::new(DefaultMemoryAllocator)
    };
    let max_pages = definition
        .maximum_pages()
        .unwrap_or(config.memory_limit_pages());
    let max_bytes = max_pages as usize * 65_536;
    let linear_memory = allocator
        .allocate(definition.minimum_pages() as usize * 65_536, max_bytes)
        .ok_or_else(|| RuntimeError::new("memory allocation failed"))?;
    Ok(Some(Memory::new(definition, linear_memory)))
}

#[derive(Clone, Copy, Debug, Default)]
#[allow(dead_code)]
struct GuardPageMemoryAllocator;

impl MemoryAllocator for GuardPageMemoryAllocator {
    fn allocate(
        &self,
        cap: usize,
        max: usize,
    ) -> Option<crate::experimental::memory::LinearMemory> {
        let allocation = GuardPageAllocator.allocate_zeroed(max).ok()?;
        Some(crate::experimental::memory::LinearMemory::from_guarded(
            allocation, cap, max,
        ))
    }
}

fn cache_key(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}-{:x}", bytes.len())
}

fn compile_binary_module(bytes: &[u8], config: &RuntimeConfig) -> Result<CompiledModule> {
    let mut module = decode_module(bytes, config.core_features())
        .map_err(|err| RuntimeError::new(err.to_string()))?;
    module.assign_module_id(bytes, &[], false);
    module.build_memory_definitions();
    let imported_functions = module
        .imported_functions()
        .into_iter()
        .map(convert_function_definition)
        .collect();
    let exported_functions = module
        .exported_functions()
        .into_iter()
        .map(|(name, definition)| (name, convert_function_definition(definition)))
        .collect();
    let imported_memories = module
        .imported_memories()
        .into_iter()
        .map(convert_memory_definition)
        .collect();
    let exported_memories = module
        .exported_memories()
        .into_iter()
        .map(|(name, definition)| (name, convert_memory_definition(definition)))
        .collect();
    let custom_sections = if config.custom_sections() {
        module
            .custom_sections
            .iter()
            .map(|section| CustomSection::new(section.name.clone(), section.data.clone()))
            .collect()
    } else {
        Vec::new()
    };
    let name = module
        .name_section
        .as_ref()
        .and_then(|names| (!names.module_name.is_empty()).then_some(names.module_name.clone()));

    Ok(CompiledModule::new(CompiledModuleInner {
        name,
        bytes: bytes.to_vec(),
        imported_functions,
        exported_functions,
        imported_memories,
        exported_memories,
        exported_globals: BTreeMap::new(),
        custom_sections,
        host_callbacks: BTreeMap::new(),
        lower_module: Some(module),
        closed: std::sync::atomic::AtomicBool::new(false),
    }))
}

pub(crate) fn lower_host_function_callback(
    callback: HostCallback,
    param_count: usize,
    result_count: usize,
) -> razero_wasm::host_func::HostFuncRef {
    razero_wasm::host_func::host_func(move |_caller, stack| {
        let (ctx, module) = active_invocation().ok_or_else(|| {
            razero_wasm::host_func::HostFuncError::new(
                "host functions require an active public invocation context",
            )
        })?;
        let params = stack[..param_count].to_vec();
        let results = callback(ctx, module, &params)
            .map_err(|err| razero_wasm::host_func::HostFuncError::new(err.to_string()))?;
        if results.len() != result_count {
            return Err(razero_wasm::host_func::HostFuncError::new(format!(
                "expected {result_count} results, received {}",
                results.len()
            )));
        }
        stack[..result_count].copy_from_slice(&results);
        Ok(())
    })
}

fn instantiate_in_store(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module: WasmModule,
    name: Option<&str>,
) -> Result<ModuleInstanceId> {
    store
        .lock()
        .expect("runtime store poisoned")
        .instantiate(module, name.unwrap_or_default(), None)
        .map_err(|err| RuntimeError::new(err.to_string()))
}

fn delete_from_store(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
) -> Result<()> {
    store
        .lock()
        .expect("runtime store poisoned")
        .delete_module(module_id)
        .map_err(|err| RuntimeError::new(err.to_string()))
}

fn build_guest_callbacks(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
    module: &WasmModule,
) -> Result<BTreeMap<String, HostCallback>> {
    let mut callbacks = BTreeMap::new();
    for export in module
        .export_section
        .iter()
        .filter(|export| export.ty == WasmExternType::FUNC)
    {
        let store = store.clone();
        let function_index = export.index;
        let export_name = export.name.clone();
        callbacks.insert(
            export_name,
            Arc::new(move |ctx: Context, module: Module, params: &[u64]| {
                let params = params.to_vec();
                with_active_invocation(&ctx, &module, || {
                    let store = store.lock().expect("runtime store poisoned");
                    let engine = store
                        .module_engine(module_id)
                        .and_then(|engine| engine.as_any().downcast_ref::<InterpModuleEngine>())
                        .ok_or_else(|| RuntimeError::new("module engine is unavailable"))?;
                    engine
                        .call(function_index, &params)
                        .map_err(|err| RuntimeError::new(err.to_string()))
                })
            }) as HostCallback,
        );
    }
    Ok(callbacks)
}

fn guest_memory(
    store: Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
    definition: MemoryDefinition,
) -> Memory {
    let read_store = store.clone();
    let write_store = store.clone();
    let grow_store = store.clone();
    Memory::dynamic(
        definition,
        move || interp_memory_size(&store, module_id).unwrap_or_default(),
        move |offset, len| interp_memory_read(&read_store, module_id, offset, len),
        move |offset, value| interp_memory_write_u32(&write_store, module_id, offset, value),
        move |delta, maximum| interp_memory_grow(&grow_store, module_id, delta, maximum),
    )
}

fn guest_globals(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
    module: &WasmModule,
) -> Result<BTreeMap<String, Global>> {
    let locked = store.lock().expect("runtime store poisoned");
    let instance = locked
        .instance(module_id)
        .cloned()
        .ok_or_else(|| RuntimeError::new("module instance is unavailable"))?;
    drop(locked);

    let mut globals = BTreeMap::new();
    for export in module
        .export_section
        .iter()
        .filter(|export| export.ty == WasmExternType::GLOBAL)
    {
        let index = export.index as usize;
        let ty = *instance
            .global_types
            .get(index)
            .ok_or_else(|| RuntimeError::new(format!("global[{index}] type missing")))?;
        let fallback = export_global_value(&instance, index)
            .ok_or_else(|| RuntimeError::new(format!("global[{index}] value missing")))?;
        let store = store.clone();
        globals.insert(
            export.name.clone(),
            Global::dynamic(ty.mutable, move || {
                current_global_value(&store, module_id, index).unwrap_or(fallback)
            }),
        );
    }
    Ok(globals)
}

fn interp_memory_size(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
) -> Option<u32> {
    let store = store.lock().ok()?;
    let engine = store
        .module_engine(module_id)?
        .as_any()
        .downcast_ref::<InterpModuleEngine>()?;
    engine.memory_size()
}

fn interp_memory_read(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
    offset: usize,
    len: usize,
) -> Option<Vec<u8>> {
    let store = store.lock().ok()?;
    let engine = store
        .module_engine(module_id)?
        .as_any()
        .downcast_ref::<InterpModuleEngine>()?;
    engine.memory_read(offset, len)
}

fn interp_memory_write_u32(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
    offset: u32,
    value: u32,
) -> bool {
    let store = match store.lock() {
        Ok(store) => store,
        Err(_) => return false,
    };
    let Some(engine) = store
        .module_engine(module_id)
        .and_then(|engine| engine.as_any().downcast_ref::<InterpModuleEngine>())
    else {
        return false;
    };
    engine.memory_write_u32(offset, value)
}

fn interp_memory_grow(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
    delta: u32,
    maximum: Option<u32>,
) -> Option<u32> {
    let store = store.lock().ok()?;
    let engine = store
        .module_engine(module_id)?
        .as_any()
        .downcast_ref::<InterpModuleEngine>()?;
    engine.memory_grow(delta, maximum)
}

fn current_global_value(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
    index: usize,
) -> Option<GlobalValue> {
    let store = store.lock().ok()?;
    let engine = store
        .module_engine(module_id)?
        .as_any()
        .downcast_ref::<InterpModuleEngine>()?;
    engine
        .global_value(index as u32)
        .map(|(lo, _hi, ty)| convert_global_value(ty, lo))
        .or_else(|| {
            store
                .instance(module_id)
                .and_then(|instance| export_global_value(instance, index))
        })
}

fn export_global_value(
    instance: &razero_wasm::module_instance::ModuleInstance,
    index: usize,
) -> Option<GlobalValue> {
    let global = instance.globals.get(index)?;
    Some(convert_global_value(global.ty.val_type, global.value))
}

fn convert_global_value(value_type: WasmValueType, lo: u64) -> GlobalValue {
    match value_type {
        WasmValueType::I32 => GlobalValue::I32(lo as i32),
        WasmValueType::I64 => GlobalValue::I64(lo as i64),
        WasmValueType::F32 => GlobalValue::F32(lo as u32),
        WasmValueType::F64 => GlobalValue::F64(lo),
        _ => GlobalValue::I64(lo as i64),
    }
}

fn convert_function_definition(definition: &WasmFunctionDefinition) -> FunctionDefinition {
    let mut converted = FunctionDefinition::new(if definition.name().is_empty() {
        definition
            .export_names()
            .first()
            .cloned()
            .unwrap_or_default()
    } else {
        definition.name().to_string()
    })
    .with_module_name(
        (!definition.module_name().is_empty()).then_some(definition.module_name().to_string()),
    )
    .with_signature(
        definition
            .param_types()
            .iter()
            .copied()
            .map(convert_value_type)
            .collect(),
        definition
            .result_types()
            .iter()
            .copied()
            .map(convert_value_type)
            .collect(),
    )
    .with_parameter_names(definition.param_names().to_vec())
    .with_result_names(definition.result_names().to_vec());
    if let Some((module, name)) = definition.import() {
        converted = converted.with_import(module.to_string(), name.to_string());
    }
    for export_name in definition.export_names() {
        converted = converted.with_export_name(export_name.clone());
    }
    converted
}

fn convert_memory_definition(definition: &WasmMemoryDefinition) -> MemoryDefinition {
    let (maximum_pages, has_max) = definition.max();
    let mut converted = MemoryDefinition::new(definition.min(), has_max.then_some(maximum_pages))
        .with_module_name(
            (!definition.module_name().is_empty()).then_some(definition.module_name().to_string()),
        );
    if let Some((module, name)) = definition.import() {
        converted = converted.with_import(module.to_string(), name.to_string());
    }
    for export_name in definition.export_names() {
        converted = converted.with_export_name(export_name.clone());
    }
    converted
}

fn convert_value_type(value_type: WasmValueType) -> ValueType {
    match value_type {
        WasmValueType::I32 => ValueType::I32,
        WasmValueType::I64 => ValueType::I64,
        WasmValueType::F32 => ValueType::F32,
        WasmValueType::F64 => ValueType::F64,
        WasmValueType::V128 => ValueType::V128,
        WasmValueType::EXTERNREF => ValueType::ExternRef,
        WasmValueType::FUNCREF => ValueType::FuncRef,
        _ => ValueType::I32,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicI64, AtomicU32, Ordering},
        Arc, Mutex,
    };

    use super::Runtime;
    use crate::{
        api::{
            error::RuntimeError,
            wasm::{FunctionDefinition, ValueType},
        },
        config::ModuleConfig,
        ctx_keys::Context,
        experimental::{
            add_fuel, get_snapshotter, get_yielder, remaining_fuel, with_close_notifier,
            with_fuel_controller, with_function_listener_factory, with_snapshotter, with_yielder,
            CloseNotifier, FuelController, FunctionListener, FunctionListenerFactory,
        },
    };

    #[derive(Clone)]
    struct TestFuelController {
        budget: i64,
        consumed: Arc<AtomicI64>,
    }

    impl FuelController for TestFuelController {
        fn budget(&self) -> i64 {
            self.budget
        }

        fn consumed(&self, amount: i64) {
            self.consumed.fetch_add(amount, Ordering::SeqCst);
        }
    }

    struct RecordingListener {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl FunctionListener for RecordingListener {
        fn before(
            &self,
            _ctx: &Context,
            _module: &crate::Module,
            definition: &FunctionDefinition,
            params: &[u64],
        ) {
            self.events
                .lock()
                .expect("listener events poisoned")
                .push(format!("before:{}:{params:?}", definition.name()));
        }

        fn after(
            &self,
            _ctx: &Context,
            _module: &crate::Module,
            definition: &FunctionDefinition,
            results: &[u64],
        ) {
            self.events
                .lock()
                .expect("listener events poisoned")
                .push(format!("after:{}:{results:?}", definition.name()));
        }
    }

    struct RecordingListenerFactory {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl FunctionListenerFactory for RecordingListenerFactory {
        fn new_listener(
            &self,
            _definition: &FunctionDefinition,
        ) -> Option<Arc<dyn FunctionListener>> {
            Some(Arc::new(RecordingListener {
                events: self.events.clone(),
            }))
        }
    }

    struct RecordingCloseNotifier {
        exit_code: Arc<AtomicU32>,
    }

    impl CloseNotifier for RecordingCloseNotifier {
        fn close_notify(&self, _ctx: &Context, exit_code: u32) {
            self.exit_code.store(exit_code, Ordering::SeqCst);
        }
    }

    #[test]
    fn runtime_rejects_invalid_magic() {
        let runtime = Runtime::new();
        let err = match runtime.compile(b"not-wasm") {
            Ok(_) => panic!("expected invalid magic error"),
            Err(err) => err,
        };
        assert_eq!("invalid magic number", err.to_string());
    }

    #[test]
    fn runtime_can_instantiate_empty_module() {
        let runtime = Runtime::new();
        let bytes = b"\0asm\x01\0\0\0";
        let compiled = runtime.compile(bytes).unwrap();
        let module = runtime
            .instantiate_with_context(&Context::default(), &compiled, ModuleConfig::new())
            .unwrap();
        assert!(!module.is_closed());
    }

    #[test]
    fn guest_exports_execute_through_public_api() {
        let runtime = Runtime::new();
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f,
                0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00,
                0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();

        assert_eq!(
            vec![42],
            module
                .exported_function("run")
                .unwrap()
                .call(&[41])
                .unwrap()
        );
    }

    #[test]
    fn guest_modules_can_call_public_host_imports() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                |_ctx, _module, params| Ok(vec![params[0] + 1]),
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .export("inc")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01,
                    0x7f, 0x01, 0x7f, 0x02, 0x0b, 0x01, 0x03, b'e', b'n', b'v', 0x03, b'i', b'n',
                    b'c', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u',
                    b'n', 0x00, 0x01, 0x0a, 0x08, 0x01, 0x06, 0x00, 0x20, 0x00, 0x10, 0x00, 0x0b,
                ],
                ModuleConfig::new(),
            )
            .unwrap();

        assert_eq!(
            vec![42],
            guest.exported_function("run").unwrap().call(&[41]).unwrap()
        );
    }

    #[test]
    fn host_module_calls_listeners_and_tracks_fuel() {
        let runtime = Runtime::new();
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                |ctx, _module, params| {
                    assert_eq!(4, remaining_fuel(&ctx).unwrap());
                    add_fuel(&ctx, -2).unwrap();
                    Ok(vec![params[0] + params[1]])
                },
                &[ValueType::I32, ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("add")
            .export("add")
            .instantiate(&Context::default())
            .unwrap();

        let consumed = Arc::new(AtomicI64::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let call_ctx = with_fuel_controller(
            &with_function_listener_factory(
                &Context::default(),
                RecordingListenerFactory {
                    events: events.clone(),
                },
            ),
            TestFuelController {
                budget: 5,
                consumed: consumed.clone(),
            },
        );

        let results = module
            .exported_function("add")
            .unwrap()
            .call_with_context(&call_ctx, &[20, 22])
            .unwrap();

        assert_eq!(vec![42], results);
        assert_eq!(3, consumed.load(Ordering::SeqCst));
        assert_eq!(
            vec!["before:add:[20, 22]", "after:add:[42]"],
            *events.lock().expect("events poisoned")
        );
    }

    #[test]
    fn memory_supports_read_write_and_grow() {
        let runtime = Runtime::new();
        let bytes = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07,
            0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
        ];
        let compiled = runtime.compile(&bytes).unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let memory = module.exported_memory("memory").unwrap();

        assert_eq!(65_536, memory.size());
        assert!(memory.write_u32_le(8, 0x1122_3344));
        assert_eq!(Some(0x1122_3344), memory.read_u32_le(8));
        assert_eq!(Some(1), memory.grow(1));
        assert_eq!(131_072, memory.size());
    }

    #[test]
    fn compiled_module_close_prevents_instantiation() {
        let runtime = Runtime::new();
        let compiled = runtime.compile(b"\0asm\x01\0\0\0").unwrap();
        compiled.close();
        let err = match runtime.instantiate(&compiled, ModuleConfig::new()) {
            Ok(_) => panic!("expected closed compiled module error"),
            Err(err) => err,
        };
        assert_eq!("compiled module is closed", err.to_string());
    }

    #[test]
    fn host_stack_callback_still_supported() {
        let runtime = Runtime::new();
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_stack_callback(
                |_ctx, _module, stack| {
                    stack[0] += stack[1];
                    Ok(())
                },
                &[ValueType::I32, ValueType::I32],
                &[ValueType::I32],
            )
            .export("add")
            .instantiate(&Context::default())
            .unwrap();

        assert_eq!(
            vec![42],
            module
                .exported_function("add")
                .unwrap()
                .call(&[20, 22])
                .unwrap()
        );
    }

    #[test]
    fn close_notifier_runs_on_module_close() {
        let runtime = Runtime::new();
        let exit_code = Arc::new(AtomicU32::new(0));
        let instantiate_ctx = with_close_notifier(
            &Context::default(),
            RecordingCloseNotifier {
                exit_code: exit_code.clone(),
            },
        );

        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("noop")
            .instantiate(&instantiate_ctx)
            .unwrap();

        module.close_with_exit_code(&instantiate_ctx, 7).unwrap();
        assert_eq!(7, exit_code.load(Ordering::SeqCst));
    }

    #[test]
    fn snapshot_and_yield_hooks_work_for_host_functions() {
        let runtime = Runtime::new();
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                |ctx, _module, params| {
                    if params[0] == 0 {
                        let snapshotter = get_snapshotter(&ctx).unwrap();
                        snapshotter.snapshot().restore(&[99]);
                        return Ok(vec![1]);
                    }
                    get_yielder(&ctx).unwrap().r#yield();
                    Ok(vec![0])
                },
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let snapshot_ctx = with_snapshotter(&Context::default());
        let results = module
            .exported_function("hook")
            .unwrap()
            .call_with_context(&snapshot_ctx, &[0])
            .unwrap();
        assert_eq!(vec![99], results);

        let yield_ctx = with_yielder(&Context::default());
        let err = module
            .exported_function("hook")
            .unwrap()
            .call_with_context(&yield_ctx, &[1])
            .unwrap_err();
        let RuntimeError::Yield(yield_error) = err else {
            panic!("expected yield error");
        };
        let resumed = yield_error
            .resumer()
            .resume(&Context::default(), &[55])
            .unwrap();
        assert_eq!(vec![55], resumed);
    }
}
