use std::{
    cell::RefCell,
    collections::BTreeMap,
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use razero_decoder::decoder::decode_module;
use razero_decoder::memory::MemorySizer;
use razero_interp::{
    engine::InterpModuleEngine,
    interpreter::{active_host_call_stack, Module as InterpRuntimeModule},
};
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
        error::{Result, RuntimeError},
        wasm::{
            active_invocation, with_active_invocation, CustomSection, FunctionDefinition, Global,
            GlobalValue, HostCallback, Memory, MemoryDefinition, Module, RuntimeModuleRegistry,
            ValueType,
        },
    },
    builder::HostModuleBuilder,
    config::{CompiledModule, CompiledModuleInner, ModuleConfig, RuntimeConfig},
    ctx_keys::Context,
    experimental::{
        listener::StackFrame,
        memory::{DefaultMemoryAllocator, MemoryAllocator},
    },
};

#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    config: RuntimeConfig,
    modules: RuntimeModuleRegistry,
    store: Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    closed: AtomicU64,
}

fn closed_state(exit_code: u32) -> u64 {
    1 + (u64::from(exit_code) << 32)
}

fn closed_exit_code(closed: u64) -> u32 {
    (closed >> 32) as u32
}

thread_local! {
    static ACTIVE_GUEST_RUNTIME_MODULES: RefCell<Vec<ActiveGuestRuntimeModule>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy)]
struct ActiveGuestRuntimeModule {
    module_id: ModuleInstanceId,
    module: NonNull<InterpRuntimeModule>,
}

fn with_active_guest_runtime_module<T>(
    module_id: ModuleInstanceId,
    module: &mut InterpRuntimeModule,
    f: impl FnOnce() -> T,
) -> T {
    ACTIVE_GUEST_RUNTIME_MODULES.with(|active| {
        active.borrow_mut().push(ActiveGuestRuntimeModule {
            module_id,
            module: NonNull::from(module),
        });
    });
    let result = f();
    ACTIVE_GUEST_RUNTIME_MODULES.with(|active| {
        active.borrow_mut().pop();
    });
    result
}

fn with_current_guest_runtime_module<T>(
    module_id: ModuleInstanceId,
    f: impl FnOnce(&InterpRuntimeModule) -> T,
) -> Option<T> {
    ACTIVE_GUEST_RUNTIME_MODULES.with(|active| {
        let binding = active.borrow();
        let active_module = binding
            .iter()
            .rev()
            .find(|entry| entry.module_id == module_id)
            .or_else(|| binding.last())?;
        Some(unsafe { f(active_module.module.as_ref()) })
    })
}

fn with_current_guest_runtime_module_mut<T>(
    module_id: ModuleInstanceId,
    f: impl FnOnce(&mut InterpRuntimeModule) -> T,
) -> Option<T> {
    ACTIVE_GUEST_RUNTIME_MODULES.with(|active| {
        let binding = active.borrow();
        let active_module = binding
            .iter()
            .rev()
            .find(|entry| entry.module_id == module_id)
            .or_else(|| binding.last())?;
        Some(unsafe { f(&mut *active_module.module.as_ptr()) })
    })
}

fn merged_listener_stack(ctx: &Context, module: &Module) -> Option<Vec<StackFrame>> {
    let active_frames = active_host_call_stack()?;
    let runtime_store = module.runtime_store()?;
    let module_id = module.store_module_id()?;
    let store = runtime_store.lock().ok()?;
    let mut instance = store.instance(module_id)?.clone();
    let active_stack: Vec<_> = active_frames
        .into_iter()
        .rev()
        .map(|frame| {
            StackFrame::new(
                convert_function_definition(
                    instance
                        .source
                        .function_definition(frame.function_index as u32),
                ),
                Vec::new(),
                Vec::new(),
                frame.program_counter as u64,
                function_source_offset(&instance.source, frame.function_index as u32),
            )
        })
        .collect();
    let parent_stack = ctx
        .invocation
        .as_ref()
        .map(|invocation| invocation.listener_stack.as_slice())
        .unwrap_or(&[]);
    if let Some(bottom) = active_stack.first() {
        if let Some(index) = parent_stack
            .iter()
            .position(|frame| frame.definition() == bottom.definition())
        {
            let mut merged = parent_stack[..index].to_vec();
            merged.extend(active_stack);
            return Some(merged);
        }
    }
    let mut merged = parent_stack.to_vec();
    merged.extend(active_stack);
    Some(merged)
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
        let mut store = WasmStore::new(razero_interp::engine::InterpEngine::new());
        store.set_secure_memory(config.secure_mode());
        Self {
            inner: Arc::new(RuntimeInner {
                config,
                modules: Arc::new(Mutex::new(BTreeMap::new())),
                store: Arc::new(Mutex::new(store)),
                closed: AtomicU64::new(0),
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
            if name.is_empty() {
                return Ok(module);
            }
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
        if module_name.is_empty() {
            return None;
        }
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
        if self
            .inner
            .closed
            .compare_exchange(
                0,
                closed_state(exit_code),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
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
        let closed = self.inner.closed.load(Ordering::SeqCst);
        if closed != 0 {
            Err(RuntimeError::new(format!(
                "runtime closed with exit_code({})",
                closed_exit_code(closed)
            )))
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
            compiled
                .inner()
                .exported_globals
                .values()
                .cloned()
                .collect(),
            ctx.close_notifier.clone(),
            close_hook,
            Some(Arc::downgrade(&self.inner.modules)),
            Some(Arc::downgrade(&self.inner.store)),
            compiled
                .inner()
                .lower_module
                .as_ref()
                .map(exported_function_source_offsets)
                .unwrap_or_default(),
            store_module_id,
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
        resolve_imports_with_context(ctx, &self.inner.store, &lower_module)?;
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
        let all_globals = guest_all_globals(&self.inner.store, module_id)?;
        let close_hook = Some({
            let store = self.inner.store.clone();
            Arc::new(move |_exit_code| {
                let _ = delete_from_store(&store, module_id);
            }) as Arc<dyn Fn(u32) + Send + Sync>
        });
        let module = Module::new(
            name,
            compiled.inner().exported_functions.clone(),
            callbacks,
            self.inner.config.fuel(),
            memory,
            globals,
            all_globals,
            ctx.close_notifier.clone(),
            close_hook,
            Some(Arc::downgrade(&self.inner.modules)),
            Some(Arc::downgrade(&self.inner.store)),
            exported_function_source_offsets(&lower_module),
            Some(module_id),
        );

        if let Some(start_index) = lower_module.start_section {
            let start =
                guest_callback_for_function_index(self.inner.store.clone(), module_id, start_index);
            if let Err(err) = start(ctx.clone(), module.clone(), &[]) {
                let _ = delete_from_store(&self.inner.store, module_id);
                return Err(err);
            }
        }

        Ok(module)
    }
}

fn resolve_imports_with_context(
    ctx: &Context,
    _store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module: &WasmModule,
) -> Result<()> {
    let Some(resolver) = ctx.import_resolver.as_ref() else {
        return Ok(());
    };

    for import_name in module
        .import_section
        .iter()
        .map(|import| import.module.as_str())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if let Some(module) = resolver(import_name) {
            module.register_import_alias(import_name)?;
        }
    }

    Ok(())
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

fn apply_memory_config(module: &mut WasmModule, config: &RuntimeConfig) {
    let sizer = MemorySizer::new(
        config.memory_limit_pages(),
        config.memory_capacity_from_max(),
    );

    if let Some(memory) = module.memory_section.as_mut() {
        let (min, cap, max) = sizer.size(memory.min, memory.is_max_encoded.then_some(memory.max));
        memory.min = min;
        memory.cap = cap;
        memory.max = max;
    }

    for import in &mut module.import_section {
        let razero_wasm::module::ImportDesc::Memory(memory) = &mut import.desc else {
            continue;
        };
        let (min, cap, max) = sizer.size(memory.min, memory.is_max_encoded.then_some(memory.max));
        memory.min = min;
        memory.cap = cap;
        memory.max = max;
    }
}

fn compile_binary_module(bytes: &[u8], config: &RuntimeConfig) -> Result<CompiledModule> {
    let mut module = decode_module(bytes, config.core_features())
        .map_err(|err| RuntimeError::new(err.to_string()))?;
    apply_memory_config(&mut module, config);
    module
        .validate(config.core_features(), config.memory_limit_pages())
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
    razero_wasm::host_func::host_func(move |caller, stack| {
        let (ctx, module) = active_invocation().ok_or_else(|| {
            razero_wasm::host_func::HostFuncError::new(
                "host functions require an active public invocation context",
            )
        })?;
        let params = stack[..param_count].to_vec();
        let ctx = merged_listener_stack(&ctx, &module)
            .map(|listener_stack| ctx.with_listener_stack(listener_stack))
            .unwrap_or(ctx);
        let results = match (
            caller.data_mut::<InterpRuntimeModule>(),
            module.store_module_id(),
        ) {
            (Some(runtime_module), Some(module_id)) => {
                with_active_guest_runtime_module(module_id, runtime_module, || {
                    callback(ctx, module, &params)
                })
            }
            _ => callback(ctx, module, &params),
        }
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
            guest_callback_for_function_index(store, module_id, function_index),
        );
    }
    Ok(callbacks)
}

pub(crate) fn guest_callback_for_function_index(
    store: Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
    function_index: u32,
) -> HostCallback {
    Arc::new(move |ctx: Context, module: Module, params: &[u64]| {
        let params = params.to_vec();
        with_active_invocation(&ctx, &module, || {
            let engine = store
                .lock()
                .expect("runtime store poisoned")
                .module_engine(module_id)
                .and_then(|engine| engine.as_any().downcast_ref::<InterpModuleEngine>())
                .cloned()
                .ok_or_else(|| RuntimeError::new("module engine is unavailable"))?;
            engine
                .call(function_index, &params)
                .map_err(|err| RuntimeError::new(err.to_string()))
        })
    }) as HostCallback
}

fn guest_memory(
    store: Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
    definition: MemoryDefinition,
) -> Memory {
    let read_store = store.clone();
    let byte_write_store = store.clone();
    let write_store = store.clone();
    let grow_store = store.clone();
    Memory::dynamic(
        definition,
        move || interp_memory_size(&store, module_id).unwrap_or_default(),
        move |offset, len| interp_memory_read(&read_store, module_id, offset, len),
        move |offset, values| interp_memory_write(&byte_write_store, module_id, offset, values),
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

fn guest_all_globals(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
) -> Result<Vec<Global>> {
    let locked = store.lock().expect("runtime store poisoned");
    let instance = locked
        .instance(module_id)
        .cloned()
        .ok_or_else(|| RuntimeError::new("module instance is unavailable"))?;
    drop(locked);

    let mut globals = Vec::with_capacity(instance.global_types.len());
    for (index, ty) in instance.global_types.iter().copied().enumerate() {
        let fallback = export_global_value(&instance, index)
            .ok_or_else(|| RuntimeError::new(format!("global[{index}] value missing")))?;
        let store = store.clone();
        globals.push(Global::dynamic(ty.mutable, move || {
            current_global_value(&store, module_id, index).unwrap_or(fallback)
        }));
    }
    Ok(globals)
}

fn interp_memory_size(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
) -> Option<u32> {
    if let Some(size) = with_current_guest_runtime_module(module_id, |module| {
        module
            .memory
            .as_ref()
            .map(|memory| memory.bytes().len() as u32)
    })
    .flatten()
    {
        return Some(size);
    }
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
    if let Some(bytes) = with_current_guest_runtime_module(module_id, |module| {
        let memory = module.memory.as_ref()?;
        let end = offset.checked_add(len)?;
        memory.bytes().get(offset..end).map(ToOwned::to_owned)
    })
    .flatten()
    {
        return Some(bytes);
    }
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
    if let Some(wrote) = with_current_guest_runtime_module_mut(module_id, |module| {
        let Some(memory) = module.memory.as_mut() else {
            return false;
        };
        let start = offset as usize;
        let Some(end) = start.checked_add(4) else {
            return false;
        };
        let Some(slice) = memory.bytes_mut().get_mut(start..end) else {
            return false;
        };
        slice.copy_from_slice(&value.to_le_bytes());
        true
    }) {
        return wrote;
    }
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

fn interp_memory_write(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
    offset: usize,
    values: &[u8],
) -> bool {
    if let Some(wrote) = with_current_guest_runtime_module_mut(module_id, |module| {
        let Some(memory) = module.memory.as_mut() else {
            return false;
        };
        let Some(end) = offset.checked_add(values.len()) else {
            return false;
        };
        let Some(slice) = memory.bytes_mut().get_mut(offset..end) else {
            return false;
        };
        slice.copy_from_slice(values);
        true
    }) {
        return wrote;
    }
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
    engine.memory_write(offset, values)
}

fn interp_memory_grow(
    store: &Arc<Mutex<WasmStore<razero_interp::engine::InterpEngine>>>,
    module_id: ModuleInstanceId,
    delta: u32,
    maximum: Option<u32>,
) -> Option<u32> {
    if let Some(previous) = with_current_guest_runtime_module_mut(module_id, |module| {
        let memory = module.memory.as_mut()?;
        if let Some(maximum) = maximum {
            memory.max_pages = Some(memory.max_pages.unwrap_or(maximum).min(maximum));
        }
        memory.grow(delta)
    })
    .flatten()
    {
        return Some(previous);
    }
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

fn exported_function_source_offsets(module: &WasmModule) -> BTreeMap<String, u64> {
    let mut module = module.clone();
    let exports: Vec<_> = module
        .exported_functions()
        .into_iter()
        .map(|(name, definition)| (name, definition.index()))
        .collect();
    exports
        .into_iter()
        .map(|(name, index)| (name, function_source_offset(&module, index)))
        .collect()
}

pub(crate) fn function_source_offset(module: &WasmModule, function_index: u32) -> u64 {
    function_index
        .checked_sub(module.import_function_count)
        .and_then(|code_index| module.code_section.get(code_index as usize))
        .map(|code| code.body_offset_in_code_section)
        .unwrap_or(0)
}

pub(crate) fn convert_function_definition(
    definition: &WasmFunctionDefinition,
) -> FunctionDefinition {
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

pub(crate) fn convert_value_type(value_type: WasmValueType) -> ValueType {
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

pub(crate) fn to_wasm_value_type(value_type: ValueType) -> WasmValueType {
    match value_type {
        ValueType::I32 => WasmValueType::I32,
        ValueType::I64 => WasmValueType::I64,
        ValueType::F32 => WasmValueType::F32,
        ValueType::F64 => WasmValueType::F64,
        ValueType::V128 => WasmValueType::V128,
        ValueType::ExternRef => WasmValueType::EXTERNREF,
        ValueType::FuncRef => WasmValueType::FUNCREF,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicI64, AtomicU32, Ordering},
        Arc, Mutex,
    };
    use std::{collections::HashMap, thread};

    use super::Runtime;
    use crate::{
        api::{
            error::RuntimeError,
            wasm::{FunctionDefinition, ValueType},
        },
        cache::CompilationCache,
        config::ModuleConfig,
        ctx_keys::Context,
        experimental::{
            add_fuel, get_snapshotter, get_yielder, remaining_fuel, with_close_notifier,
            with_fuel_controller, with_function_listener_factory, with_snapshotter, with_yielder,
            CloseNotifier, FuelController, FunctionListener, FunctionListenerFactory, Snapshot,
            StackIterator,
        },
        RuntimeConfig,
    };
    use razero_wasm::memory::MemoryBytes;

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
            stack_iterator: &mut dyn StackIterator,
        ) {
            let mut stack = Vec::new();
            while stack_iterator.next() {
                stack.push(format!(
                    "{}@{}:{}",
                    stack_iterator.function().definition().name(),
                    stack_iterator.program_counter(),
                    stack_iterator
                        .function()
                        .source_offset_for_pc(stack_iterator.program_counter())
                ));
            }
            self.events
                .lock()
                .expect("listener events poisoned")
                .push(format!(
                    "before:{}:{params:?}:{}",
                    definition.name(),
                    stack.join("|")
                ));
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

        fn abort(
            &self,
            _ctx: &Context,
            _module: &crate::Module,
            definition: &FunctionDefinition,
            error: &RuntimeError,
        ) {
            self.events
                .lock()
                .expect("listener events poisoned")
                .push(format!("abort:{}:{}", definition.name(), error));
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

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct StackRecord {
        function: String,
        program_counter: u64,
        source_offset: u64,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct BeforeRecord {
        function: String,
        stack: Vec<StackRecord>,
    }

    struct StackRecordingListener {
        events: Arc<Mutex<Vec<BeforeRecord>>>,
    }

    impl FunctionListener for StackRecordingListener {
        fn before(
            &self,
            _ctx: &Context,
            _module: &crate::Module,
            definition: &FunctionDefinition,
            _params: &[u64],
            stack_iterator: &mut dyn StackIterator,
        ) {
            let mut stack = Vec::new();
            while stack_iterator.next() {
                let program_counter = stack_iterator.program_counter();
                stack.push(StackRecord {
                    function: stack_iterator.function().definition().name().to_string(),
                    program_counter,
                    source_offset: stack_iterator
                        .function()
                        .source_offset_for_pc(program_counter),
                });
            }
            self.events
                .lock()
                .expect("listener stack events poisoned")
                .push(BeforeRecord {
                    function: definition.name().to_string(),
                    stack,
                });
        }
    }

    struct StackRecordingFactory {
        events: Arc<Mutex<Vec<BeforeRecord>>>,
    }

    impl FunctionListenerFactory for StackRecordingFactory {
        fn new_listener(
            &self,
            _definition: &FunctionDefinition,
        ) -> Option<Arc<dyn FunctionListener>> {
            Some(Arc::new(StackRecordingListener {
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

    #[derive(Default)]
    struct CountingCache {
        modules: Mutex<HashMap<String, Vec<u8>>>,
        gets: AtomicU32,
        inserts: AtomicU32,
    }

    impl CompilationCache for CountingCache {
        fn get(&self, key: &str) -> Option<Vec<u8>> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            self.modules
                .lock()
                .expect("cache poisoned")
                .get(key)
                .cloned()
        }

        fn insert(&self, key: &str, bytes: &[u8]) {
            self.inserts.fetch_add(1, Ordering::SeqCst);
            self.modules
                .lock()
                .expect("cache poisoned")
                .insert(key.to_string(), bytes.to_vec());
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
    fn binary_compilation_cache_is_shared_across_runtimes() {
        let cache = Arc::new(CountingCache::default());
        let config = RuntimeConfig::new().with_compilation_cache(cache.clone());
        let runtime_a = Runtime::with_config(config.clone());
        let runtime_b = Runtime::with_config(config);
        let bytes = b"\0asm\x01\0\0\0";

        runtime_a.compile(bytes).unwrap();
        runtime_b.compile(bytes).unwrap();

        assert_eq!(2, cache.gets.load(Ordering::SeqCst));
        assert_eq!(1, cache.inserts.load(Ordering::SeqCst));
    }

    #[test]
    fn memory_limit_does_not_change_binary_cache_reuse() {
        let cache = Arc::new(CountingCache::default());
        let config = RuntimeConfig::new().with_compilation_cache(cache);
        let runtime_0 = Runtime::with_config(config.clone());
        let runtime_1 = Runtime::with_config(config.clone().with_memory_limit_pages(2));
        let runtime_2 = Runtime::with_config(config.with_memory_limit_pages(4));
        let bytes = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x04, 0x01, 0x01, 0x01, 0x05,
            0x07, 0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
        ];

        let module_0 = runtime_0.compile(&bytes).unwrap();
        let module_1 = runtime_1.compile(&bytes).unwrap();
        let module_2 = runtime_2.compile(&bytes).unwrap();

        assert_eq!(
            Some(5),
            module_0
                .exported_memories()
                .get("memory")
                .and_then(|memory| memory.maximum_pages())
        );
        assert_eq!(
            Some(2),
            module_1
                .exported_memories()
                .get("memory")
                .and_then(|memory| memory.maximum_pages())
        );
        assert_eq!(
            Some(4),
            module_2
                .exported_memories()
                .get("memory")
                .and_then(|memory| memory.maximum_pages())
        );
    }

    #[test]
    fn host_module_compilation_does_not_use_binary_cache() {
        let cache = Arc::new(CountingCache::default());
        let runtime =
            Runtime::with_config(RuntimeConfig::new().with_compilation_cache(cache.clone()));

        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("noop")
            .compile(&Context::default())
            .unwrap();

        assert_eq!(0, cache.gets.load(Ordering::SeqCst));
        assert_eq!(0, cache.inserts.load(Ordering::SeqCst));
    }

    #[test]
    fn secure_mode_uses_guarded_guest_memory_and_preserves_oob_traps() {
        let module = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01,
            0x7f, 0x03, 0x02, 0x01, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x07, 0x01, 0x03,
            b'o', b'o', b'b', 0x00, 0x00, 0x0a, 0x0b, 0x01, 0x09, 0x00, 0x41, 0xa0, 0x8d, 0x06,
            0x28, 0x02, 0x00, 0x0b,
        ];

        for secure_mode in [false, true] {
            let runtime = Runtime::with_config(RuntimeConfig::new().with_secure_mode(secure_mode));
            let compiled = runtime.compile(&module).unwrap();
            let instance = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();

            if secure_mode {
                let store = runtime.inner.store.lock().expect("runtime store poisoned");
                let module_id = instance.store_module_id().expect("guest module id");
                let memory = store
                    .instance(module_id)
                    .and_then(|module| module.memory_instance.as_ref())
                    .expect("guest memory instance");
                assert!(matches!(memory.bytes, MemoryBytes::Guarded { .. }));
            }

            let err = instance
                .exported_function("oob")
                .unwrap()
                .call(&[])
                .expect_err("out-of-bounds load should trap");
            assert!(err.to_string().contains("out of bounds memory access"));
        }
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
    fn import_resolver_can_link_anonymous_module_instances() {
        let runtime = Runtime::new();
        let call_count = Arc::new(AtomicU32::new(0));

        let compiled_host = runtime
            .new_host_module_builder("env0")
            .new_function_builder()
            .with_func(
                {
                    let call_count = call_count.clone();
                    move |_ctx, _module, _params| {
                        call_count.fetch_add(1, Ordering::SeqCst);
                        Ok(Vec::new())
                    }
                },
                &[],
                &[],
            )
            .with_name("start")
            .export("start")
            .compile(&Context::default())
            .unwrap();

        let anonymous_import = runtime
            .instantiate_with_context(
                &Context::default(),
                &compiled_host,
                ModuleConfig::new().with_name(""),
            )
            .unwrap();

        let resolver_ctx =
            crate::experimental::with_import_resolver(&Context::default(), move |name| {
                (name == "env").then_some(anonymous_import.clone())
            });
        let compiled_guest = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
                0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r', b't', 0x00,
                0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01,
                0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
            ])
            .unwrap();

        let guest = runtime
            .instantiate_with_context(&resolver_ctx, &compiled_guest, ModuleConfig::new())
            .unwrap();

        guest.exported_function("run").unwrap().call(&[]).unwrap();
        assert!(!guest.is_closed());
        assert_eq!(1, call_count.load(Ordering::SeqCst));
    }

    #[test]
    fn anonymous_module_names_are_not_registered_or_reserved() {
        let runtime = Runtime::new();
        let compiled = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("noop")
            .compile(&Context::default())
            .unwrap();

        let first = runtime
            .instantiate_with_context(
                &Context::default(),
                &compiled,
                ModuleConfig::new().with_name(""),
            )
            .unwrap();
        let second = runtime
            .instantiate_with_context(
                &Context::default(),
                &compiled,
                ModuleConfig::new().with_name(""),
            )
            .unwrap();

        assert!(runtime.module("").is_none());
        assert!(!first.is_closed());
        assert!(!second.is_closed());
    }

    #[test]
    fn import_resolver_can_satisfy_start_section_imports() {
        for iteration in 0..5 {
            let runtime = Runtime::new();
            let call_count = Arc::new(AtomicU32::new(0));
            let compiled_host = runtime
                .new_host_module_builder(format!("env{iteration}"))
                .new_function_builder()
                .with_func(
                    {
                        let call_count = call_count.clone();
                        move |_ctx, _module, _params| {
                            call_count.fetch_add(1, Ordering::SeqCst);
                            Ok(Vec::new())
                        }
                    },
                    &[],
                    &[],
                )
                .with_name("start")
                .export("start")
                .compile(&Context::default())
                .unwrap();

            let anonymous_import = runtime
                .instantiate_with_context(
                    &Context::default(),
                    &compiled_host,
                    ModuleConfig::new().with_name(""),
                )
                .unwrap();

            let resolver_ctx =
                crate::experimental::with_import_resolver(&Context::default(), move |name| {
                    (name == "env").then_some(anonymous_import.clone())
                });
            let compiled_guest = runtime
                .compile(&[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00,
                    0x00, 0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r',
                    b't', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x08, 0x01, 0x01, 0x0a, 0x06, 0x01,
                    0x04, 0x00, 0x10, 0x00, 0x0b,
                ])
                .unwrap();

            let guest = runtime
                .instantiate_with_context(&resolver_ctx, &compiled_guest, ModuleConfig::new())
                .unwrap();

            assert!(!guest.is_closed());
            assert_eq!(1, call_count.load(Ordering::SeqCst));
        }
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
            vec!["before:add:[20, 22]:add@0:0", "after:add:[42]"],
            *events.lock().expect("events poisoned")
        );
    }

    #[test]
    fn listener_abort_runs_when_host_function_errors() {
        let runtime = Runtime::new();
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                |_ctx, _module, _params| Err(RuntimeError::new("boom")),
                &[],
                &[],
            )
            .export("fail")
            .instantiate(&Context::default())
            .unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let call_ctx = with_function_listener_factory(
            &Context::default(),
            RecordingListenerFactory {
                events: events.clone(),
            },
        );

        let err = module
            .exported_function("fail")
            .unwrap()
            .call_with_context(&call_ctx, &[])
            .unwrap_err();

        assert_eq!("boom", err.to_string());
        assert_eq!(
            vec!["before:fail:[]:fail@0:0", "abort:fail:boom"],
            *events.lock().expect("events poisoned")
        );
    }

    #[test]
    fn nested_public_host_calls_extend_listener_stack() {
        let runtime = Runtime::new();
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                |ctx, module, params| {
                    module
                        .exported_function("inner")
                        .expect("inner export should exist")
                        .call_with_context(&ctx, params)
                },
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("outer")
            .export("outer")
            .new_function_builder()
            .with_func(
                |_ctx, _module, params| Ok(vec![params[0] + 1]),
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("inner")
            .export("inner")
            .instantiate(&Context::default())
            .unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let call_ctx = with_function_listener_factory(
            &Context::default(),
            StackRecordingFactory {
                events: events.clone(),
            },
        );

        let results = module
            .exported_function("outer")
            .unwrap()
            .call_with_context(&call_ctx, &[41])
            .unwrap();

        assert_eq!(vec![42], results);
        assert_eq!(
            vec![
                BeforeRecord {
                    function: "outer".to_string(),
                    stack: vec![StackRecord {
                        function: "outer".to_string(),
                        program_counter: 0,
                        source_offset: 0,
                    }],
                },
                BeforeRecord {
                    function: "inner".to_string(),
                    stack: vec![
                        StackRecord {
                            function: "inner".to_string(),
                            program_counter: 0,
                            source_offset: 0,
                        },
                        StackRecord {
                            function: "outer".to_string(),
                            program_counter: 0,
                            source_offset: 0,
                        },
                    ],
                },
            ],
            *events.lock().expect("listener stack events poisoned")
        );
    }

    #[test]
    fn nested_guest_host_public_calls_capture_runtime_stack() {
        let runtime = Runtime::new();
        let helper = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00,
                    0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x0a, 0x01, 0x06, b'h', b'e', b'l',
                    b'p', b'e', b'r', 0x00, 0x00, 0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x2a, 0x0b,
                ],
                ModuleConfig::new().with_name("helper"),
            )
            .unwrap();

        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                {
                    let helper = helper.clone();
                    move |ctx, _module, _params| {
                        helper
                            .exported_function("helper")
                            .expect("helper export should exist")
                            .call_with_context(&ctx, &[])
                    }
                },
                &[],
                &[ValueType::I32],
            )
            .with_name("hook")
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00,
                    0x01, 0x7f, 0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04, b'h', b'o', b'o',
                    b'k', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u',
                    b'n', 0x00, 0x01, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x41, 0x00, 0x1a, 0x10, 0x00,
                    0x0b,
                ],
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let call_ctx = with_function_listener_factory(
            &Context::default(),
            StackRecordingFactory {
                events: events.clone(),
            },
        );

        let results = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&call_ctx, &[])
            .unwrap();

        assert_eq!(vec![42], results);
        let events = events
            .lock()
            .expect("listener stack events poisoned")
            .clone();
        assert_eq!(2, events.len());

        assert_eq!("run", events[0].function);
        assert_eq!(1, events[0].stack.len());
        assert_eq!("run", events[0].stack[0].function);
        assert_eq!(0, events[0].stack[0].program_counter);
        assert!(events[0].stack[0].source_offset > 0);

        assert_eq!("helper", events[1].function);
        assert_eq!(3, events[1].stack.len());
        assert_eq!(
            StackRecord {
                function: "helper".to_string(),
                program_counter: 0,
                source_offset: events[1].stack[0].source_offset,
            },
            events[1].stack[0]
        );
        assert!(events[1].stack[0].source_offset > 0);
        assert_eq!(0, events[1].stack[1].program_counter);
        assert_eq!(0, events[1].stack[1].source_offset);
        assert_eq!("run", events[1].stack[2].function);
        assert!(events[1].stack[2].source_offset > 0);
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
    fn closed_runtime_rejects_compile_and_instantiation_with_exit_code() {
        let bytes = b"\0asm\x01\0\0\0";

        for exit_code in [0, 2] {
            let runtime = Runtime::new();
            let compiled = runtime.compile(bytes).unwrap();
            runtime
                .close_with_exit_code(&Context::default(), exit_code)
                .unwrap();

            for err in [
                match runtime.compile(bytes) {
                    Ok(_) => panic!("compile should fail after close"),
                    Err(err) => err,
                },
                match runtime.instantiate(&compiled, ModuleConfig::new()) {
                    Ok(_) => panic!("instantiate should fail after close"),
                    Err(err) => err,
                },
                match runtime.instantiate_binary(bytes, ModuleConfig::new()) {
                    Ok(_) => panic!("instantiate_binary should fail after close"),
                    Err(err) => err,
                },
            ] {
                assert_eq!(
                    format!("runtime closed with exit_code({exit_code})"),
                    err.to_string()
                );
            }
        }
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
            .expect("resumer should be present")
            .resume(&Context::default(), &[55])
            .unwrap();
        assert_eq!(vec![55], resumed);
    }

    #[test]
    fn close_notify_fn_runs_on_module_close() {
        let runtime = Runtime::new();
        let closed = Arc::new(AtomicU32::new(0));
        let instantiate_ctx = with_close_notifier(
            &Context::default(),
            crate::experimental::CloseNotifyFn::new({
                let closed = closed.clone();
                move |_ctx: &Context, exit_code| {
                    closed.store(exit_code + 1, Ordering::SeqCst);
                }
            }),
        );
        let module = runtime
            .new_host_module_builder("env")
            .instantiate(&instantiate_ctx)
            .unwrap();

        assert_eq!(0, closed.load(Ordering::SeqCst));
        module.close(&instantiate_ctx).unwrap();
        assert_eq!(1, closed.load(Ordering::SeqCst));
    }

    #[test]
    fn imported_host_function_can_write_guest_memory() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                |ctx, module, params| {
                    assert!(get_snapshotter(&ctx).is_none());
                    assert!(module
                        .memory()
                        .expect("guest memory should be present")
                        .write_u32_le(params[0] as u32, 42));
                    Ok(Vec::new())
                },
                &[ValueType::I32],
                &[],
            )
            .export("write")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x60, 0x01,
                    0x7f, 0x00, 0x60, 0x00, 0x00, 0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05,
                    b'w', b'r', b'i', b't', b'e', 0x00, 0x00, 0x03, 0x02, 0x01, 0x01, 0x05, 0x03,
                    0x01, 0x00, 0x01, 0x07, 0x10, 0x02, 0x03, b'r', b'u', b'n', 0x00, 0x01, 0x06,
                    b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, 0x0a, 0x08, 0x01, 0x06, 0x00,
                    0x41, 0x00, 0x10, 0x00, 0x0b,
                ],
                ModuleConfig::new(),
            )
            .unwrap();

        guest.exported_function("run").unwrap().call(&[]).unwrap();
        assert_eq!(
            Some(42),
            guest
                .memory()
                .expect("guest memory should be exported")
                .read_u32_le(0)
        );
    }

    #[test]
    fn later_imported_host_functions_dispatch_correctly() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                |_ctx, _module, _params| Ok(vec![11]),
                &[],
                &[ValueType::I32],
            )
            .export("first")
            .new_function_builder()
            .with_callback(
                |_ctx, _module, _params| Ok(vec![22]),
                &[],
                &[ValueType::I32],
            )
            .export("second")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00,
                    0x01, 0x7f, 0x02, 0x1a, 0x02, 0x03, b'e', b'n', b'v', 0x05, b'f', b'i', b'r',
                    b's', b't', 0x00, 0x00, 0x03, b'e', b'n', b'v', 0x06, b's', b'e', b'c', b'o',
                    b'n', b'd', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r',
                    b'u', b'n', 0x00, 0x02, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x10, 0x00, 0x10, 0x01,
                    0x6a, 0x0b,
                ],
                ModuleConfig::new(),
            )
            .unwrap();

        assert_eq!(
            vec![33],
            guest.exported_function("run").unwrap().call(&[]).unwrap()
        );
    }

    #[test]
    fn yielded_guest_execution_resumes_into_guest_continuation() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("example")
            .new_function_builder()
            .with_callback(
                |ctx, _module, _params| {
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(vec![0])
                },
                &[],
                &[ValueType::I32],
            )
            .export("async_work")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                include_bytes!("../../experimental/testdata/yield.wasm"),
                ModuleConfig::new(),
            )
            .unwrap();

        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&with_yielder(&Context::default()), &[])
            .unwrap_err();
        let RuntimeError::Yield(yield_error) = err else {
            panic!("expected yield error");
        };

        let results = yield_error
            .resumer()
            .expect("resumer should be present")
            .resume(&with_yielder(&Context::default()), &[42])
            .unwrap();
        assert_eq!(vec![142], results);
    }

    #[test]
    fn yielded_guest_execution_can_resume_from_another_thread() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("example")
            .new_function_builder()
            .with_callback(
                |ctx, _module, _params| {
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(vec![0])
                },
                &[],
                &[ValueType::I32],
            )
            .export("async_work")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                include_bytes!("../../experimental/testdata/yield.wasm"),
                ModuleConfig::new(),
            )
            .unwrap();

        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&with_yielder(&Context::default()), &[])
            .unwrap_err();
        let RuntimeError::Yield(yield_error) = err else {
            panic!("expected yield error");
        };

        let resumer = yield_error.resumer().expect("resumer should be present");
        let handle =
            thread::spawn(move || resumer.resume(&with_yielder(&Context::default()), &[7]));
        let results = handle.join().expect("resume thread panicked").unwrap();
        assert_eq!(vec![107], results);
    }

    #[test]
    fn yielded_guest_execution_can_yield_twice() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("example")
            .new_function_builder()
            .with_callback(
                |ctx, _module, _params| {
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(vec![0])
                },
                &[],
                &[ValueType::I32],
            )
            .export("async_work")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                include_bytes!("../../experimental/testdata/yield.wasm"),
                ModuleConfig::new(),
            )
            .unwrap();

        let err = guest
            .exported_function("run_twice")
            .unwrap()
            .call_with_context(&with_yielder(&Context::default()), &[])
            .unwrap_err();
        let RuntimeError::Yield(first_yield) = err else {
            panic!("expected initial yield error");
        };

        let err = first_yield
            .resumer()
            .expect("first resumer should be present")
            .resume(&with_yielder(&Context::default()), &[40])
            .unwrap_err();
        let RuntimeError::Yield(second_yield) = err else {
            panic!("expected chained yield error");
        };

        let results = second_yield
            .resumer()
            .expect("second resumer should be present")
            .resume(&with_yielder(&Context::default()), &[2])
            .unwrap();
        assert_eq!(vec![42], results);
    }

    #[test]
    fn cancelled_yielded_guest_execution_cannot_resume() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("example")
            .new_function_builder()
            .with_callback(
                |ctx, _module, _params| {
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(vec![0])
                },
                &[],
                &[ValueType::I32],
            )
            .export("async_work")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                include_bytes!("../../experimental/testdata/yield.wasm"),
                ModuleConfig::new(),
            )
            .unwrap();

        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&with_yielder(&Context::default()), &[])
            .unwrap_err();
        let RuntimeError::Yield(yield_error) = err else {
            panic!("expected yield error");
        };

        let resumer = yield_error.resumer().expect("resumer should be present");
        resumer.cancel();
        resumer.cancel();
        let err = resumer
            .resume(&with_yielder(&Context::default()), &[42])
            .unwrap_err();
        assert_eq!("cannot resume: resumer has been cancelled", err.to_string());
    }

    #[test]
    fn yielded_execution_can_resume_from_another_thread() {
        let runtime = Runtime::new();
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                |ctx, _module, _params| {
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(vec![0])
                },
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let err = module
            .exported_function("hook")
            .unwrap()
            .call_with_context(&with_yielder(&Context::default()), &[1])
            .unwrap_err();
        let RuntimeError::Yield(yield_error) = err else {
            panic!("expected yield error");
        };

        let resumer = yield_error.resumer().expect("resumer should be present");
        let handle =
            thread::spawn(move || resumer.resume(&with_yielder(&Context::default()), &[77]));
        let resumed = handle.join().expect("resume thread panicked").unwrap();
        assert_eq!(vec![77], resumed);
    }

    #[test]
    fn cancelled_yielded_execution_cannot_resume() {
        let runtime = Runtime::new();
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                |ctx, _module, _params| {
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(vec![0])
                },
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let err = module
            .exported_function("hook")
            .unwrap()
            .call_with_context(&with_yielder(&Context::default()), &[1])
            .unwrap_err();
        let RuntimeError::Yield(yield_error) = err else {
            panic!("expected yield error");
        };

        let resumer = yield_error.resumer().expect("resumer should be present");
        resumer.cancel();
        resumer.cancel();
        let err = resumer
            .resume(&with_yielder(&Context::default()), &[42])
            .unwrap_err();
        assert_eq!("cannot resume: resumer has been cancelled", err.to_string());
    }

    #[test]
    fn snapshot_restore_from_later_invocation_panics() {
        let runtime = Runtime::new();
        let snapshots = Arc::new(Mutex::new(Vec::new()));
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                {
                    let snapshots = snapshots.clone();
                    move |ctx, _module, _params| {
                        let snapshot = get_snapshotter(&ctx)
                            .expect("snapshotter should be injected")
                            .snapshot();
                        snapshots.lock().expect("snapshots poisoned").push(snapshot);
                        Ok(vec![0])
                    }
                },
                &[],
                &[ValueType::I32],
            )
            .export("snapshot")
            .new_function_builder()
            .with_callback(
                {
                    let snapshots = snapshots.clone();
                    move |_ctx, _module, _params| {
                        snapshots.lock().expect("snapshots poisoned")[0].restore(&[12]);
                        Ok(vec![1])
                    }
                },
                &[],
                &[ValueType::I32],
            )
            .export("restore")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = with_snapshotter(&Context::default());
        let results = module
            .exported_function("snapshot")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap();
        assert_eq!(vec![0], results);

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = module
                .exported_function("restore")
                .unwrap()
                .call_with_context(&ctx, &[]);
        }))
        .expect_err("expected stale snapshot restore to panic");
        let message = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .expect("panic payload should be string");
        assert_eq!(
            "unhandled snapshot restore, this generally indicates restore was called from a different exported function invocation than snapshot",
            message
        );
    }

    #[test]
    fn snapshot_restore_within_nested_invocation_overrides_results() {
        let runtime = Runtime::new();
        let snapshot = Arc::new(Mutex::new(None::<Snapshot>));
        let sidechannel = Arc::new(AtomicI64::new(0));
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                {
                    let snapshot = snapshot.clone();
                    let sidechannel = sidechannel.clone();
                    move |ctx, module, _params| {
                        *snapshot.lock().expect("snapshot poisoned") = Some(
                            get_snapshotter(&ctx)
                                .expect("snapshotter should be injected")
                                .snapshot(),
                        );

                        let restored = module
                            .exported_function("restore")
                            .expect("restore export should exist")
                            .call_with_context(&ctx, &[])?;
                        assert_eq!(vec![0], restored);
                        sidechannel.store(10, Ordering::SeqCst);
                        Ok(vec![2])
                    }
                },
                &[],
                &[ValueType::I32],
            )
            .export("snapshot")
            .new_function_builder()
            .with_callback(
                {
                    let snapshot = snapshot.clone();
                    move |_ctx, _module, _params| {
                        snapshot
                            .lock()
                            .expect("snapshot poisoned")
                            .as_ref()
                            .expect("snapshot should be present")
                            .restore(&[12]);
                        Ok(vec![0])
                    }
                },
                &[],
                &[ValueType::I32],
            )
            .export("restore")
            .instantiate(&Context::default())
            .unwrap();

        let results = module
            .exported_function("snapshot")
            .unwrap()
            .call_with_context(&with_snapshotter(&Context::default()), &[])
            .unwrap();
        assert_eq!(vec![12], results);
        assert_eq!(10, sidechannel.load(Ordering::SeqCst));
    }
}
