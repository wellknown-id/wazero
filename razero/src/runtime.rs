use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt::{Display, Formatter},
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use razero_compiler::{
    call_engine::CallEngineError, engine::CompilerEngine, module_engine::CompilerModuleEngine,
};
use razero_decoder::decoder::decode_module;
use razero_decoder::memory::MemorySizer;
use razero_interp::{
    compiler::ValueType as InterpValueType,
    engine::{register_guest_callback, InterpModuleEngine},
    interpreter::{active_host_call_stack, Module as InterpRuntimeModule},
    signature::Signature as InterpSignature,
};
use razero_platform::{compiler_supported, supports_guard_pages};
use razero_secmem::GuardPageAllocator;
use razero_wasm::{
    engine::{
        Engine as WasmEngine, EngineError as WasmEngineError, ModuleEngine as WasmModuleEngine,
    },
    function_definition::FunctionDefinition as WasmFunctionDefinition,
    memory_definition::MemoryDefinition as WasmMemoryDefinition,
    module::{ExternType as WasmExternType, Module as WasmModule, ValueType as WasmValueType},
    store::Store as WasmStore,
    store_module_list::ModuleInstanceId,
};

use crate::{
    api::{
        error::{policy_denied_error, ExitError, Result, RuntimeError},
        wasm::{
            active_invocation, install_close_on_context_done, with_active_invocation,
            CustomSection, FunctionDefinition, Global, GlobalValue, HostCallback, Memory,
            MemoryDefinition, Module, RuntimeModuleRegistry, ValueType,
        },
    },
    builder::HostModuleBuilder,
    cache::BinaryCompilationArtifact,
    config::{CompiledModule, CompiledModuleInner, ModuleConfig, RuntimeConfig, RuntimeEngineKind},
    ctx_keys::Context,
    experimental::{
        get_compilation_workers,
        host_call_policy::{get_host_call_policy, HostCallPolicyRequest},
        host_call_policy_observer::{notify_host_call_policy_observer, HostCallPolicyDecision},
        listener::StackFrame,
        memory::{DefaultMemoryAllocator, MemoryAllocator},
        trap::{
            get_trap_observer, trap_cause_of, trap_cause_of_call_engine_error, TrapObservation,
        },
    },
};

pub(crate) struct PublicEngine {
    inner: Box<dyn WasmEngine>,
}

pub(crate) type RuntimeStore = WasmStore<PublicEngine>;

impl PublicEngine {
    fn new(kind: RuntimeEngineKind, secure_mode: bool, fuel: i64) -> Self {
        let inner: Box<dyn WasmEngine> = match kind {
            RuntimeEngineKind::Compiler => {
                Box::new(CompilerEngine::with_secure_mode_and_fuel(secure_mode, fuel))
            }
            RuntimeEngineKind::Interpreter => Box::new(razero_interp::engine::InterpEngine::new()),
            RuntimeEngineKind::Auto => unreachable!("runtime engine kind must be resolved"),
        };
        Self { inner }
    }
}

impl WasmEngine for PublicEngine {
    fn close(&mut self) -> std::result::Result<(), WasmEngineError> {
        self.inner.close()
    }

    fn compile_module_with_options(
        &mut self,
        module: &WasmModule,
        options: &razero_wasm::engine::CompileOptions,
    ) -> std::result::Result<(), WasmEngineError> {
        self.inner.compile_module_with_options(module, options)
    }

    fn compile_module(&mut self, module: &WasmModule) -> std::result::Result<(), WasmEngineError> {
        self.inner.compile_module(module)
    }

    fn compiled_module_count(&self) -> u32 {
        self.inner.compiled_module_count()
    }

    fn delete_compiled_module(&mut self, module: &WasmModule) {
        self.inner.delete_compiled_module(module);
    }

    fn load_precompiled_module(
        &mut self,
        module: &WasmModule,
        artifact: &[u8],
    ) -> std::result::Result<(), WasmEngineError> {
        self.inner.load_precompiled_module(module, artifact)
    }

    fn precompiled_module_bytes(&self, module: &WasmModule) -> Option<Vec<u8>> {
        self.inner.precompiled_module_bytes(module)
    }

    fn new_module_engine(
        &self,
        module: &WasmModule,
        instance: &razero_wasm::module_instance::ModuleInstance,
    ) -> std::result::Result<Box<dyn WasmModuleEngine>, WasmEngineError> {
        self.inner.new_module_engine(module, instance)
    }
}

#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    config: RuntimeConfig,
    modules: RuntimeModuleRegistry,
    store: Arc<Mutex<RuntimeStore>>,
    closed: AtomicU64,
}

// This runtime-owned cache format is distinct from razero-compiler native packaging
// (`.razero-package` + ELF object/executable artifacts).
const PRECOMPILED_ARTIFACT_MAGIC: &[u8; 8] = b"RZAOT001";

/// Runtime-owned precompiled module artifact used by `razero` embedding APIs.
///
/// This format is intentionally separate from the native packaging ABI owned by
/// `razero-compiler`. Packaged native executables do not replace the interpreter or the
/// embedding/runtime APIs exposed by `razero`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrecompiledArtifact {
    wasm_bytes: Vec<u8>,
    compiled_bytes: Vec<u8>,
}

impl PrecompiledArtifact {
    pub fn new(wasm_bytes: Vec<u8>, compiled_bytes: Vec<u8>) -> Self {
        Self {
            wasm_bytes,
            compiled_bytes,
        }
    }

    pub fn wasm_bytes(&self) -> &[u8] {
        &self.wasm_bytes
    }

    pub fn compiled_bytes(&self) -> &[u8] {
        &self.compiled_bytes
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            PRECOMPILED_ARTIFACT_MAGIC.len()
                + 16
                + self.wasm_bytes.len()
                + self.compiled_bytes.len(),
        );
        out.extend_from_slice(PRECOMPILED_ARTIFACT_MAGIC);
        out.extend_from_slice(&(self.wasm_bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(&self.wasm_bytes);
        out.extend_from_slice(&(self.compiled_bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(&self.compiled_bytes);
        out
    }

    pub fn decode(bytes: &[u8]) -> std::result::Result<Self, PrecompiledArtifactError> {
        let mut cursor = 0usize;
        if bytes.len() < PRECOMPILED_ARTIFACT_MAGIC.len()
            || &bytes[..PRECOMPILED_ARTIFACT_MAGIC.len()] != PRECOMPILED_ARTIFACT_MAGIC
        {
            return Err(PrecompiledArtifactError::InvalidHeader(
                "invalid precompiled artifact magic".to_string(),
            ));
        }
        cursor += PRECOMPILED_ARTIFACT_MAGIC.len();
        let wasm_len = read_u64_artifact(bytes, &mut cursor, "missing wasm length")? as usize;
        let wasm_bytes = read_vec_artifact(bytes, &mut cursor, wasm_len, "truncated wasm payload")?;
        let compiled_len =
            read_u64_artifact(bytes, &mut cursor, "missing compiled artifact length")? as usize;
        let compiled_bytes = read_vec_artifact(
            bytes,
            &mut cursor,
            compiled_len,
            "truncated compiled artifact payload",
        )?;
        if cursor != bytes.len() {
            return Err(PrecompiledArtifactError::InvalidHeader(
                "unexpected trailing bytes in precompiled artifact".to_string(),
            ));
        }
        Ok(Self::new(wasm_bytes, compiled_bytes))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrecompiledArtifactError {
    InvalidHeader(String),
}

impl Display for PrecompiledArtifactError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHeader(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for PrecompiledArtifactError {}

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
    let mut source = module.lower_module().or_else(|| {
        let runtime_store = module.runtime_store()?;
        let module_id = module.store_module_id()?;
        let store = runtime_store.lock().ok()?;
        Some(store.instance(module_id)?.source.clone())
    })?;
    let active_stack: Vec<_> = active_frames
        .into_iter()
        .rev()
        .map(|frame| {
            StackFrame::new(
                convert_function_definition(
                    source.function_definition(frame.function_index as u32),
                ),
                Vec::new(),
                Vec::new(),
                frame.program_counter as u64,
                function_source_offset(&source, frame.function_index as u32),
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

fn host_callback_context(
    ctx: &Context,
    module: &Module,
    definition: &FunctionDefinition,
    params: &[u64],
) -> Context {
    let listener_stack = merged_listener_stack(ctx, module).unwrap_or_else(|| {
        let mut stack = ctx
            .invocation
            .as_ref()
            .map(|invocation| invocation.listener_stack.clone())
            .unwrap_or_default();
        stack.push(StackFrame::new(
            definition.clone(),
            params.to_vec(),
            Vec::new(),
            0,
            0,
        ));
        stack
    });
    ctx.with_listener_stack(listener_stack)
        .with_function_definition(definition.clone())
}

fn enforce_host_call_policy(
    ctx: &Context,
    module: &Module,
    definition: &FunctionDefinition,
) -> Result<()> {
    let mut request = HostCallPolicyRequest::new().with_function(definition.clone());
    if let Some(module_name) = module.name() {
        request = request.with_caller_module_name(module_name);
    }
    if let Some(memory) = module.memory() {
        request = request.with_memory(memory.definition().clone());
    }
    let policy = get_host_call_policy(ctx).or_else(|| module.host_call_policy());
    let allowed = !policy
        .as_ref()
        .is_some_and(|policy| !policy.allow_host_call(ctx, &request));
    notify_host_call_policy_observer(
        ctx,
        module,
        &request,
        if allowed {
            HostCallPolicyDecision::Allowed
        } else {
            HostCallPolicyDecision::Denied
        },
    );
    if !allowed {
        return Err(policy_denied_error("host call"));
    }
    Ok(())
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
        let mut store = WasmStore::new(PublicEngine::new(
            resolve_runtime_engine_kind(&config),
            config.secure_mode(),
            config.fuel(),
        ));
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
        self.compile_with_context(&Context::default(), bytes)
    }

    pub fn build_precompiled_artifact(&self, bytes: &[u8]) -> Result<PrecompiledArtifact> {
        self.build_precompiled_artifact_with_context(&Context::default(), bytes)
    }

    pub fn build_precompiled_artifact_with_context(
        &self,
        ctx: &Context,
        bytes: &[u8],
    ) -> Result<PrecompiledArtifact> {
        let compiled = self.compile_with_context(ctx, bytes)?;
        let lower_module =
            compiled.inner().lower_module.as_ref().ok_or_else(|| {
                RuntimeError::new("compiled module is missing lower runtime state")
            })?;
        let compiled_bytes = self
            .inner
            .store
            .lock()
            .expect("runtime store poisoned")
            .engine
            .precompiled_module_bytes(lower_module)
            .ok_or_else(|| {
                RuntimeError::new("runtime engine does not expose precompiled artifacts")
            })?;
        Ok(PrecompiledArtifact::new(
            compiled.bytes().to_vec(),
            compiled_bytes,
        ))
    }

    pub fn compile_with_context(&self, ctx: &Context, bytes: &[u8]) -> Result<CompiledModule> {
        self.fail_if_closed()?;
        let compiled = if let Some(cache) = self.inner.config.compilation_cache() {
            let key = cache_key(bytes);
            if let Some(artifact) = cache.get_binary_artifact(&key) {
                compile_cached_binary_module(&artifact, &self.inner.config, None)?
            } else if let Some(cached) = cache.get(&key) {
                let artifact = decode_binary_artifact(&cached, &self.inner.config)?;
                cache.insert_binary_artifact(&key, artifact.clone());
                compile_cached_binary_module(&artifact, &self.inner.config, None)?
            } else {
                let artifact = decode_binary_artifact(bytes, &self.inner.config)?;
                let compiled = compile_cached_binary_module(&artifact, &self.inner.config, None)?;
                cache.insert_binary_artifact(&key, artifact);
                cache.insert(&key, bytes);
                compiled
            }
        } else {
            compile_binary_module(bytes, &self.inner.config)?
        };
        self.precompile_with_context(ctx, &compiled)?;
        Ok(compiled)
    }

    pub fn compile_precompiled_artifact(
        &self,
        artifact: &PrecompiledArtifact,
    ) -> Result<CompiledModule> {
        self.compile_precompiled_artifact_with_context(&Context::default(), artifact)
    }

    pub fn compile_precompiled_artifact_with_context(
        &self,
        ctx: &Context,
        artifact: &PrecompiledArtifact,
    ) -> Result<CompiledModule> {
        self.fail_if_closed()?;
        let decoded = decode_binary_artifact(artifact.wasm_bytes(), &self.inner.config)?;
        let compiled = compile_cached_binary_module(
            &decoded,
            &self.inner.config,
            Some(artifact.compiled_bytes().to_vec()),
        )?;
        self.precompile_with_context(ctx, &compiled)?;
        Ok(compiled)
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
        self.instantiate_binary_with_context(&Context::default(), bytes, config)
    }

    pub fn instantiate_precompiled_artifact(
        &self,
        artifact: &PrecompiledArtifact,
        config: ModuleConfig,
    ) -> Result<Module> {
        self.instantiate_precompiled_artifact_with_context(&Context::default(), artifact, config)
    }

    pub fn instantiate_precompiled_artifact_with_context(
        &self,
        ctx: &Context,
        artifact: &PrecompiledArtifact,
        config: ModuleConfig,
    ) -> Result<Module> {
        let compiled = self.compile_precompiled_artifact_with_context(ctx, artifact)?;
        self.instantiate_with_context(ctx, &compiled, config)
    }

    pub fn instantiate_binary_with_context(
        &self,
        ctx: &Context,
        bytes: &[u8],
        config: ModuleConfig,
    ) -> Result<Module> {
        let compiled = self.compile_with_context(ctx, bytes)?;
        self.instantiate_with_context(ctx, &compiled, config)
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
        let module = Module::new(
            name,
            compiled.inner().exported_functions.clone(),
            compiled.inner().host_callbacks.clone(),
            true,
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
            self.inner.config.host_call_policy(),
            self.inner.config.close_on_context_done(),
            self.inner.config.yield_policy(),
            Some(Arc::downgrade(&self.inner.modules)),
            Some(Arc::downgrade(&self.inner.store)),
            compiled.inner().lower_module.clone(),
            compiled
                .inner()
                .lower_module
                .as_ref()
                .map(exported_function_source_offsets)
                .unwrap_or_default(),
            store_module_id,
        );
        if let (Some(module_id), Some(lower_module)) =
            (store_module_id, compiled.inner().lower_module.as_ref())
        {
            for export in &lower_module.export_section {
                if export.ty != razero_wasm::module::ExternType::FUNC {
                    continue;
                }
                let Some(callback) = compiled.inner().host_callbacks.get(&export.name) else {
                    continue;
                };
                let function_index = export.index;
                let signature = lower_module
                    .type_of_function(function_index)
                    .map(|ty| {
                        InterpSignature::new(
                            ty.params
                                .iter()
                                .map(|ty| match *ty {
                                    WasmValueType::I32 => InterpValueType::I32,
                                    WasmValueType::I64 => InterpValueType::I64,
                                    WasmValueType::F32 => InterpValueType::F32,
                                    WasmValueType::F64 => InterpValueType::F64,
                                    WasmValueType::V128 => InterpValueType::V128,
                                    WasmValueType::FUNCREF => InterpValueType::FuncRef,
                                    WasmValueType::EXTERNREF => InterpValueType::ExternRef,
                                    _ => InterpValueType::I64,
                                })
                                .collect(),
                            ty.results
                                .iter()
                                .map(|ty| match *ty {
                                    WasmValueType::I32 => InterpValueType::I32,
                                    WasmValueType::I64 => InterpValueType::I64,
                                    WasmValueType::F32 => InterpValueType::F32,
                                    WasmValueType::F64 => InterpValueType::F64,
                                    WasmValueType::V128 => InterpValueType::V128,
                                    WasmValueType::FUNCREF => InterpValueType::FuncRef,
                                    WasmValueType::EXTERNREF => InterpValueType::ExternRef,
                                    _ => InterpValueType::I64,
                                })
                                .collect(),
                        )
                    })
                    .unwrap_or_default();
                register_guest_callback(
                    module_id,
                    function_index,
                    signature.clone(),
                    Arc::new({
                        let callback = callback.clone();
                        let definition = compiled
                            .inner()
                            .exported_functions
                            .get(&export.name)
                            .cloned()
                            .expect("host function definition missing from compiled module");
                        move |params: &[u64]| {
                            let (ctx, module) = active_invocation()
                                .ok_or_else(|| "active invocation is unavailable".to_string())?;
                            let ctx = host_callback_context(&ctx, &module, &definition, params);
                            enforce_host_call_policy(&ctx, &module, &definition)
                                .map_err(|err| err.to_string())?;
                            callback(ctx, module, params).map_err(|err| err.to_string())
                        }
                    }),
                );
            }
        }
        Ok(module)
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
        let module_id = instantiate_in_store_with_options(
            &self.inner.store,
            lower_module.clone(),
            name.as_deref(),
            compile_options_for_context(ctx),
        )?;
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
            false,
            self.inner.config.fuel(),
            memory,
            globals,
            all_globals,
            ctx.close_notifier.clone(),
            close_hook,
            self.inner.config.host_call_policy(),
            self.inner.config.close_on_context_done(),
            self.inner.config.yield_policy(),
            Some(Arc::downgrade(&self.inner.modules)),
            Some(Arc::downgrade(&self.inner.store)),
            Some(lower_module.clone()),
            exported_function_source_offsets(&lower_module),
            Some(module_id),
        );

        let function_count = lower_module
            .all_declarations()
            .map_err(|err| RuntimeError::new(err.to_string()))?
            .functions
            .len() as u32;
        for function_index in 0..function_count {
            let signature = lower_module
                .type_of_function(function_index)
                .map(|ty| {
                    InterpSignature::new(
                        ty.params
                            .iter()
                            .map(|ty| match *ty {
                                WasmValueType::I32 => InterpValueType::I32,
                                WasmValueType::I64 => InterpValueType::I64,
                                WasmValueType::F32 => InterpValueType::F32,
                                WasmValueType::F64 => InterpValueType::F64,
                                WasmValueType::V128 => InterpValueType::V128,
                                WasmValueType::FUNCREF => InterpValueType::FuncRef,
                                WasmValueType::EXTERNREF => InterpValueType::ExternRef,
                                _ => InterpValueType::I64,
                            })
                            .collect(),
                        ty.results
                            .iter()
                            .map(|ty| match *ty {
                                WasmValueType::I32 => InterpValueType::I32,
                                WasmValueType::I64 => InterpValueType::I64,
                                WasmValueType::F32 => InterpValueType::F32,
                                WasmValueType::F64 => InterpValueType::F64,
                                WasmValueType::V128 => InterpValueType::V128,
                                WasmValueType::FUNCREF => InterpValueType::FuncRef,
                                WasmValueType::EXTERNREF => InterpValueType::ExternRef,
                                _ => InterpValueType::I64,
                            })
                            .collect(),
                    )
                })
                .unwrap_or_default();
            let callback = guest_callback_for_function_index(
                self.inner.store.clone(),
                module_id,
                function_index,
            );
            register_guest_callback(
                module_id,
                function_index,
                signature.clone(),
                Arc::new(move |params: &[u64]| {
                    let (ctx, module) = active_invocation()
                        .ok_or_else(|| "active invocation is unavailable".to_string())?;
                    callback(ctx, module, params).map_err(|err| err.to_string())
                }),
            );
        }

        if let Some(start_index) = lower_module.start_section {
            let start =
                guest_callback_for_function_index(self.inner.store.clone(), module_id, start_index);
            let close_on_context_done =
                install_close_on_context_done(ctx, &module, module.close_on_context_done())?;
            let start_result = start(ctx.clone(), module.clone(), &[]);
            if let Some(stop) = close_on_context_done {
                stop.store(true, Ordering::SeqCst);
            }
            if let Err(err) = start_result {
                return Err(err);
            }
        }

        Ok(module)
    }

    fn precompile_with_context(&self, ctx: &Context, compiled: &CompiledModule) -> Result<()> {
        let Some(lower_module) = compiled.inner().lower_module.as_ref() else {
            return Ok(());
        };
        let mut store = self.inner.store.lock().expect("runtime store poisoned");
        let result = if let Some(precompiled_bytes) = compiled.inner().precompiled_bytes.as_deref()
        {
            store
                .engine
                .load_precompiled_module(lower_module, precompiled_bytes)
        } else {
            store
                .engine
                .compile_module_with_options(lower_module, &compile_options_for_context(ctx))
        };
        result.map_err(|err| RuntimeError::new(err.to_string()))
    }
}

fn resolve_runtime_engine_kind(config: &RuntimeConfig) -> RuntimeEngineKind {
    match config.engine_kind() {
        RuntimeEngineKind::Auto => {
            if compiler_supported() {
                RuntimeEngineKind::Compiler
            } else {
                RuntimeEngineKind::Interpreter
            }
        }
        RuntimeEngineKind::Compiler => {
            assert!(
                compiler_supported(),
                "compiler runtime is not supported on this host"
            );
            RuntimeEngineKind::Compiler
        }
        RuntimeEngineKind::Interpreter => RuntimeEngineKind::Interpreter,
    }
}

fn resolve_imports_with_context(
    ctx: &Context,
    _store: &Arc<Mutex<RuntimeStore>>,
    module: &WasmModule,
) -> Result<()> {
    let Some(cfg) = ctx.import_resolver.as_ref() else {
        return Ok(());
    };

    let module_name = module
        .name_section
        .as_ref()
        .map(|names| names.module_name.clone())
        .unwrap_or_default();
    let observer = ctx.import_resolver_observer.clone();

    for import_name in module
        .import_section
        .iter()
        .map(|import| import.module.as_str())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if let Some(acl) = cfg.acl.as_ref() {
            if let Err(err) = acl.check_import(import_name) {
                notify_import_resolver_observer(
                    &observer,
                    ctx,
                    &module_name,
                    import_name,
                    None,
                    crate::ImportResolverEvent::AclDenied,
                );
                return Err(err);
            }
            notify_import_resolver_observer(
                &observer,
                ctx,
                &module_name,
                import_name,
                None,
                crate::ImportResolverEvent::AclAllowed,
            );
        }
        if let Some(resolver) = cfg.resolver.as_ref() {
            notify_import_resolver_observer(
                &observer,
                ctx,
                &module_name,
                import_name,
                None,
                crate::ImportResolverEvent::ResolverAttempted,
            );
            if let Some(module) = resolver(import_name) {
                notify_import_resolver_observer(
                    &observer,
                    ctx,
                    &module_name,
                    import_name,
                    Some(module.clone()),
                    crate::ImportResolverEvent::ResolverResolved,
                );
                module.register_import_alias(import_name)?;
                continue;
            }
        }
        if cfg.fail_closed {
            notify_import_resolver_observer(
                &observer,
                ctx,
                &module_name,
                import_name,
                None,
                crate::ImportResolverEvent::FailClosedDenied,
            );
            return Err(RuntimeError::new(format!(
                "module[{import_name}] unresolved by import resolver"
            )));
        }
        notify_import_resolver_observer(
            &observer,
            ctx,
            &module_name,
            import_name,
            None,
            crate::ImportResolverEvent::StoreFallback,
        );
    }

    Ok(())
}

fn notify_import_resolver_observer(
    observer: &Option<Arc<dyn crate::ImportResolverObserver>>,
    ctx: &Context,
    module_name: &str,
    import_module: &str,
    resolved_module: Option<Module>,
    event: crate::ImportResolverEvent,
) {
    let Some(observer) = observer else {
        return;
    };
    observer.observe_import_resolution(
        ctx,
        crate::ImportResolverObservation {
            module_name: module_name.to_string(),
            import_module: import_module.to_string(),
            resolved_module,
            event,
        },
    );
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
    } else if secure_mode_uses_guard_pages(config) {
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

fn secure_mode_uses_guard_pages(config: &RuntimeConfig) -> bool {
    config.secure_mode() && supports_guard_pages()
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
        match GuardPageAllocator.allocate_zeroed(max) {
            Ok(allocation) => Some(crate::experimental::memory::LinearMemory::from_guarded(
                allocation, cap, max,
            )),
            Err(razero_secmem::SecMemError::Platform(
                razero_platform::GuardPageError::Unsupported(_),
            )) => DefaultMemoryAllocator.allocate(cap, max),
            Err(_) => None,
        }
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
    let artifact = decode_binary_artifact(bytes, config)?;
    compile_cached_binary_module(&artifact, config, None)
}

fn decode_binary_artifact(
    bytes: &[u8],
    config: &RuntimeConfig,
) -> Result<BinaryCompilationArtifact> {
    let module = decode_module(bytes, config.core_features())
        .map_err(|err| RuntimeError::new(err.to_string()))?;
    Ok(BinaryCompilationArtifact::new(bytes.to_vec(), module))
}

fn compile_cached_binary_module(
    artifact: &BinaryCompilationArtifact,
    config: &RuntimeConfig,
    precompiled_bytes: Option<Vec<u8>>,
) -> Result<CompiledModule> {
    let mut module = artifact.module.clone();
    module.enabled_features = config.core_features();
    apply_memory_config(&mut module, config);
    module.ensure_termination = config.close_on_context_done();
    module
        .validate(config.core_features(), config.memory_limit_pages())
        .map_err(|err| RuntimeError::new(err.to_string()))?;
    module.assign_module_id(&artifact.bytes, &[], config.close_on_context_done());
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
        bytes: artifact.bytes.clone(),
        precompiled_bytes,
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

fn read_u64_artifact(
    bytes: &[u8],
    cursor: &mut usize,
    message: &str,
) -> std::result::Result<u64, PrecompiledArtifactError> {
    let end = cursor.saturating_add(8);
    let slice = bytes
        .get(*cursor..end)
        .ok_or_else(|| PrecompiledArtifactError::InvalidHeader(message.to_string()))?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(slice);
    *cursor = end;
    Ok(u64::from_le_bytes(raw))
}

fn read_vec_artifact(
    bytes: &[u8],
    cursor: &mut usize,
    len: usize,
    message: &str,
) -> std::result::Result<Vec<u8>, PrecompiledArtifactError> {
    let end = cursor.saturating_add(len);
    let slice = bytes
        .get(*cursor..end)
        .ok_or_else(|| PrecompiledArtifactError::InvalidHeader(message.to_string()))?;
    *cursor = end;
    Ok(slice.to_vec())
}

pub(crate) fn lower_host_function_callback(
    callback: HostCallback,
    definition: FunctionDefinition,
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
        let ctx = host_callback_context(&ctx, &module, &definition, &params);
        enforce_host_call_policy(&ctx, &module, &definition)
            .map_err(|err| razero_wasm::host_func::HostFuncError::new(err.to_string()))?;
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
    store: &Arc<Mutex<RuntimeStore>>,
    module: WasmModule,
    name: Option<&str>,
) -> Result<ModuleInstanceId> {
    instantiate_in_store_with_options(
        store,
        module,
        name,
        razero_wasm::engine::CompileOptions::default(),
    )
}

fn instantiate_in_store_with_options(
    store: &Arc<Mutex<RuntimeStore>>,
    module: WasmModule,
    name: Option<&str>,
    options: razero_wasm::engine::CompileOptions,
) -> Result<ModuleInstanceId> {
    store
        .lock()
        .expect("runtime store poisoned")
        .instantiate_with_options(module, name.unwrap_or_default(), None, options)
        .map_err(|err| RuntimeError::new(err.to_string()))
}

fn compile_options_for_context(ctx: &Context) -> razero_wasm::engine::CompileOptions {
    razero_wasm::engine::CompileOptions::new(get_compilation_workers(ctx))
}

fn delete_from_store(store: &Arc<Mutex<RuntimeStore>>, module_id: ModuleInstanceId) -> Result<()> {
    store
        .lock()
        .expect("runtime store poisoned")
        .delete_module(module_id)
        .map_err(|err| RuntimeError::new(err.to_string()))
}

fn build_guest_callbacks(
    store: &Arc<Mutex<RuntimeStore>>,
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
    store: Arc<Mutex<RuntimeStore>>,
    module_id: ModuleInstanceId,
    function_index: u32,
) -> HostCallback {
    Arc::new(move |ctx: Context, module: Module, params: &[u64]| {
        let params = params.to_vec();
        with_active_invocation(&ctx, &module, || {
            let (interp_engine, compiler_engine) = {
                let store = store.lock().expect("runtime store poisoned");
                let engine = store
                    .module_engine(module_id)
                    .ok_or_else(|| RuntimeError::new("module engine is unavailable"))?;
                (
                    engine
                        .as_any()
                        .downcast_ref::<InterpModuleEngine>()
                        .cloned(),
                    engine
                        .as_any()
                        .downcast_ref::<CompilerModuleEngine>()
                        .cloned(),
                )
            };
            if let Some(engine) = interp_engine {
                return engine.clone().call(function_index, &params).map_err(|err| {
                    if module.is_closed() {
                        ExitError::new(module.exit_code()).into()
                    } else {
                        let err = RuntimeError::new(err.to_string());
                        notify_trap_observer(&ctx, &module, &err);
                        err
                    }
                });
            }
            if let Some(engine) = compiler_engine {
                let mut call_engine = engine.new_compiler_function(function_index);
                let mut stack = params;
                return call_engine
                    .call(&mut stack)
                    .map(|results| results.to_vec())
                    .map_err(|err| {
                        if module.is_closed() {
                            ExitError::new(module.exit_code()).into()
                        } else {
                            notify_compiler_trap_observer(&ctx, &module, &err);
                            compiler_call_error(err)
                        }
                    });
            }
            Err(RuntimeError::new("module engine is unavailable"))
        })
    }) as HostCallback
}

fn guest_memory(
    store: Arc<Mutex<RuntimeStore>>,
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
    store: &Arc<Mutex<RuntimeStore>>,
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
        let getter_store = store.clone();
        let setter_store = store.clone();
        globals.insert(
            export.name.clone(),
            Global::dynamic_with_setter(
                ty.mutable,
                move || current_global_value(&getter_store, module_id, index).unwrap_or(fallback),
                ty.mutable.then(|| {
                    Arc::new(move |value| {
                        set_current_global_value(&setter_store, module_id, index, value)
                            .expect("guest global setter should be available");
                    }) as Arc<dyn Fn(GlobalValue) + Send + Sync>
                }),
            ),
        );
    }
    Ok(globals)
}

fn guest_all_globals(
    store: &Arc<Mutex<RuntimeStore>>,
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
        let getter_store = store.clone();
        let setter_store = store.clone();
        globals.push(Global::dynamic_with_setter(
            ty.mutable,
            move || current_global_value(&getter_store, module_id, index).unwrap_or(fallback),
            ty.mutable.then(|| {
                Arc::new(move |value| {
                    set_current_global_value(&setter_store, module_id, index, value)
                        .expect("guest global setter should be available");
                }) as Arc<dyn Fn(GlobalValue) + Send + Sync>
            }),
        ));
    }
    Ok(globals)
}

fn interp_memory_size(
    store: &Arc<Mutex<RuntimeStore>>,
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
    if let Some(engine) = store
        .module_engine(module_id)?
        .as_any()
        .downcast_ref::<InterpModuleEngine>()
    {
        return engine.memory_size();
    }
    store
        .instance(module_id)?
        .memory_instance
        .as_ref()
        .map(|memory| memory.len() as u32)
}

fn interp_memory_read(
    store: &Arc<Mutex<RuntimeStore>>,
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
    if let Some(engine) = store
        .module_engine(module_id)?
        .as_any()
        .downcast_ref::<InterpModuleEngine>()
    {
        return engine.memory_read(offset, len);
    }
    let memory = store.instance(module_id)?.memory_instance.as_ref()?;
    let end = offset.checked_add(len)?;
    memory.bytes.get(offset..end).map(ToOwned::to_owned)
}

fn interp_memory_write_u32(
    store: &Arc<Mutex<RuntimeStore>>,
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
        let mut bytes = memory.bytes_mut();
        let Some(slice) = bytes.get_mut(start..end) else {
            return false;
        };
        slice.copy_from_slice(&value.to_le_bytes());
        true
    }) {
        return wrote;
    }
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return false,
    };
    if let Some(engine) = store
        .module_engine(module_id)
        .and_then(|engine| engine.as_any().downcast_ref::<InterpModuleEngine>())
    {
        return engine.memory_write_u32(offset, value);
    }
    store
        .instance_mut(module_id)
        .and_then(|instance| instance.memory_instance.as_mut())
        .is_some_and(|memory| memory.write_u32_le(offset, value))
}

fn interp_memory_write(
    store: &Arc<Mutex<RuntimeStore>>,
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
        let mut bytes = memory.bytes_mut();
        let Some(slice) = bytes.get_mut(offset..end) else {
            return false;
        };
        slice.copy_from_slice(values);
        true
    }) {
        return wrote;
    }
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return false,
    };
    if let Some(engine) = store
        .module_engine(module_id)
        .and_then(|engine| engine.as_any().downcast_ref::<InterpModuleEngine>())
    {
        return engine.memory_write(offset, values);
    }
    let Some(offset) = u32::try_from(offset).ok() else {
        return false;
    };
    store
        .instance_mut(module_id)
        .and_then(|instance| instance.memory_instance.as_mut())
        .is_some_and(|memory| memory.write(offset, values))
}

fn interp_memory_grow(
    store: &Arc<Mutex<RuntimeStore>>,
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
    let mut store = store.lock().ok()?;
    if let Some(engine) = store
        .module_engine(module_id)?
        .as_any()
        .downcast_ref::<InterpModuleEngine>()
    {
        return engine.memory_grow(delta, maximum);
    }
    let (previous, memory_snapshot) = {
        let instance = store.instance_mut(module_id)?;
        let memory = instance.memory_instance.as_mut()?;
        if let Some(maximum) = maximum {
            memory.max = memory.max.min(maximum);
        }
        let previous = memory.grow(delta)?;
        let snapshot = (
            memory.bytes.to_vec(),
            instance
                .memory_type
                .as_ref()
                .and_then(|memory_type| memory_type.is_max_encoded.then_some(memory_type.max)),
            memory.shared,
        );
        (previous, snapshot)
    };
    if let Some(engine) = store.module_engine_mut(module_id) {
        if !engine.overwrite_memory(&memory_snapshot.0, memory_snapshot.1, memory_snapshot.2) {
            engine.memory_grown();
        }
    }
    Some(previous)
}

fn current_global_value(
    store: &Arc<Mutex<RuntimeStore>>,
    module_id: ModuleInstanceId,
    index: usize,
) -> Option<GlobalValue> {
    let store = store.lock().ok()?;
    let instance = store.instance(module_id)?;
    let ty = instance.global_types.get(index)?.val_type;
    let (lo, _hi) = store
        .module_engine(module_id)?
        .get_global_value(index as u32);
    Some(convert_global_value(ty, lo))
}

fn set_current_global_value(
    store: &Arc<Mutex<RuntimeStore>>,
    module_id: ModuleInstanceId,
    index: usize,
    value: GlobalValue,
) -> Result<()> {
    let mut store = store
        .lock()
        .map_err(|_| RuntimeError::new("runtime store poisoned"))?;
    let (lo, hi) = encode_global_value(value);
    let owns_globals = {
        let engine = store
            .module_engine_mut(module_id)
            .ok_or_else(|| RuntimeError::new("module engine is unavailable"))?;
        engine.set_global_value(index as u32, lo, hi);
        engine.owns_globals()
    };
    if !owns_globals {
        let global = store
            .instance_mut(module_id)
            .and_then(|instance| instance.globals.get_mut(index))
            .ok_or_else(|| RuntimeError::new(format!("global[{index}] is unavailable")))?;
        global.set_value(lo, hi);
    }
    Ok(())
}

fn export_global_value(
    instance: &razero_wasm::module_instance::ModuleInstance,
    index: usize,
) -> Option<GlobalValue> {
    let global = instance.globals.get(index)?;
    Some(convert_global_value(global.ty.val_type, global.value().0))
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

fn compiler_call_error(err: CallEngineError) -> RuntimeError {
    match err {
        CallEngineError::ModuleExit(err) => ExitError::new(err.exit_code).into(),
        other => RuntimeError::new(other.to_string()),
    }
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

fn notify_compiler_trap_observer(ctx: &Context, module: &Module, err: &CallEngineError) {
    let Some(observer) = get_trap_observer(ctx) else {
        return;
    };
    let Some(cause) = trap_cause_of_call_engine_error(err) else {
        return;
    };
    observer.observe_trap(
        ctx,
        TrapObservation {
            module: module.clone(),
            cause,
            err: compiler_call_error(err.clone()),
        },
    );
}

fn encode_global_value(value: GlobalValue) -> (u64, u64) {
    match value {
        GlobalValue::I32(value) => (value as u32 as u64, 0),
        GlobalValue::I64(value) => (value as u64, 0),
        GlobalValue::F32(value) => (u64::from(value), 0),
        GlobalValue::F64(value) => (value, 0),
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

pub(crate) fn convert_memory_definition(definition: &WasmMemoryDefinition) -> MemoryDefinition {
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

pub(crate) fn convert_ref_type(ref_type: razero_wasm::module::RefType) -> ValueType {
    match ref_type {
        razero_wasm::module::RefType::FUNCREF => ValueType::FuncRef,
        razero_wasm::module::RefType::EXTERNREF => ValueType::ExternRef,
        _ => ValueType::FuncRef,
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
    use razero_compiler::module_engine::CompilerModuleEngine;
    use razero_interp::engine::InterpModuleEngine;
    use razero_platform::{compiler_supported, supports_guard_pages};
    use razero_wasm::engine::Engine as _;
    use std::sync::{
        atomic::{AtomicI64, AtomicU32, Ordering},
        Arc, Mutex,
    };
    use std::{collections::HashMap, thread};

    use super::{secure_mode_uses_guard_pages, Runtime};
    use crate::{
        api::{
            error::{RuntimeError, EXIT_CODE_CONTEXT_CANCELED, EXIT_CODE_DEADLINE_EXCEEDED},
            wasm::{FunctionDefinition, GlobalValue, ValueType},
        },
        cache::{BinaryCompilationArtifact, CompilationCache},
        config::ModuleConfig,
        ctx_keys::Context,
        experimental::{
            add_fuel, get_snapshotter, get_yielder, remaining_fuel, trap_cause_of,
            with_close_notifier, with_compilation_workers, with_fuel_controller,
            with_function_listener_factory, with_host_call_policy, with_host_call_policy_observer,
            with_snapshotter, with_trap_observer, with_yield_observer, with_yield_policy,
            with_yield_policy_observer, with_yielder, CloseNotifier, FuelController,
            FunctionListener, FunctionListenerFactory, HostCallPolicyDecision,
            HostCallPolicyObservation, HostCallPolicyRequest, Snapshot, StackIterator, TrapCause,
            TrapObservation, YieldEvent, YieldObservation, YieldPolicyDecision,
            YieldPolicyObservation, YieldPolicyRequest,
        },
        CompiledModule, RuntimeConfig,
    };
    use std::time::Duration;

    const SIMPLE_EXPORT_WASM: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
        0x03, 0x02, 0x01, 0x00, 0x07, 0x05, 0x01, 0x01, b'f', 0x00, 0x00, 0x0a, 0x06, 0x01, 0x04,
        0x00, 0x41, 0x2a, 0x0b,
    ];
    const LOOP_EXPORT_WASM: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03,
        0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x09, 0x01,
        0x07, 0x00, 0x03, 0x40, 0x0c, 0x00, 0x0b, 0x0b,
    ];
    use razero_wasm::memory::MemoryBytes;

    fn close_on_context_done_runtime_configs() -> Vec<RuntimeConfig> {
        let mut configs = vec![
            RuntimeConfig::new_interpreter().with_close_on_context_done(true),
            RuntimeConfig::new_auto().with_close_on_context_done(true),
        ];
        if compiler_supported() {
            configs.push(RuntimeConfig::new_compiler().with_close_on_context_done(true));
        }
        configs
    }

    fn policy_runtime_configs() -> Vec<RuntimeConfig> {
        let mut configs = vec![RuntimeConfig::new_interpreter(), RuntimeConfig::new_auto()];
        if compiler_supported() {
            configs.push(RuntimeConfig::new_compiler());
        }
        configs
    }

    fn secure_memory_runtime_configs() -> Vec<RuntimeConfig> {
        let mut configs = vec![RuntimeConfig::new_interpreter().with_secure_mode(true)];
        if compiler_supported() {
            configs.push(RuntimeConfig::new_compiler().with_secure_mode(true));
        }
        configs
    }

    fn deny_env_host_calls(_ctx: &Context, request: &HostCallPolicyRequest) -> bool {
        request
            .function
            .as_ref()
            .and_then(FunctionDefinition::module_name)
            != Some("env")
    }

    fn deny_hook_impl_host_call(_ctx: &Context, request: &HostCallPolicyRequest) -> bool {
        request.function.as_ref().map(FunctionDefinition::name) != Some("hook_impl")
    }

    fn deny_start_host_call(_ctx: &Context, request: &HostCallPolicyRequest) -> bool {
        request.function.as_ref().map(FunctionDefinition::name) != Some("start")
    }

    fn deny_untrusted_caller_module(_ctx: &Context, request: &HostCallPolicyRequest) -> bool {
        request.caller_module_name() != Some("untrusted")
    }

    fn deny_high_arity_host_calls(_ctx: &Context, request: &HostCallPolicyRequest) -> bool {
        request.param_count() <= 2
    }

    fn deny_multi_result_host_calls(_ctx: &Context, request: &HostCallPolicyRequest) -> bool {
        request.result_count() <= 1
    }

    fn allow_only_zero_arg_host_calls(_ctx: &Context, request: &HostCallPolicyRequest) -> bool {
        request.param_count() == 0
    }

    fn allow_all_host_calls(_ctx: &Context, _request: &HostCallPolicyRequest) -> bool {
        true
    }

    fn deny_all_yields(_ctx: &Context, _request: &YieldPolicyRequest) -> bool {
        false
    }

    fn allow_all_yields(_ctx: &Context, _request: &YieldPolicyRequest) -> bool {
        true
    }

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

    struct RecordingDenyYieldPolicy {
        observed: Arc<Mutex<Vec<FunctionDefinition>>>,
    }

    impl crate::experimental::YieldPolicy for RecordingDenyYieldPolicy {
        fn allow_yield(&self, _ctx: &Context, request: &YieldPolicyRequest) -> bool {
            self.observed
                .lock()
                .expect("observed yield metadata poisoned")
                .push(
                    request
                        .function
                        .clone()
                        .expect("yield policy should receive metadata"),
                );
            false
        }
    }

    struct RecordingYieldPolicyWithCaller {
        observed: Arc<Mutex<Vec<(Option<String>, FunctionDefinition)>>>,
    }

    impl crate::experimental::YieldPolicy for RecordingYieldPolicyWithCaller {
        fn allow_yield(&self, _ctx: &Context, request: &YieldPolicyRequest) -> bool {
            self.observed
                .lock()
                .expect("observed yield metadata poisoned")
                .push((
                    request.caller_module_name().map(str::to_string),
                    request
                        .function
                        .clone()
                        .expect("yield policy should receive metadata"),
                ));
            false
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
        raw_modules: Mutex<HashMap<String, Vec<u8>>>,
        binary_artifacts: Mutex<HashMap<String, BinaryCompilationArtifact>>,
        raw_gets: AtomicU32,
        raw_inserts: AtomicU32,
        artifact_gets: AtomicU32,
        artifact_inserts: AtomicU32,
        poison_raw_bytes: bool,
    }

    impl CountingCache {
        fn with_poisoned_raw_bytes() -> Self {
            Self {
                poison_raw_bytes: true,
                ..Self::default()
            }
        }
    }

    impl CompilationCache for CountingCache {
        fn get(&self, key: &str) -> Option<Vec<u8>> {
            self.raw_gets.fetch_add(1, Ordering::SeqCst);
            self.raw_modules
                .lock()
                .expect("cache poisoned")
                .get(key)
                .cloned()
        }

        fn insert(&self, key: &str, bytes: &[u8]) {
            self.raw_inserts.fetch_add(1, Ordering::SeqCst);
            let bytes = if self.poison_raw_bytes {
                b"not-wasm".to_vec()
            } else {
                bytes.to_vec()
            };
            self.raw_modules
                .lock()
                .expect("cache poisoned")
                .insert(key.to_string(), bytes);
        }

        fn get_binary_artifact(&self, key: &str) -> Option<BinaryCompilationArtifact> {
            self.artifact_gets.fetch_add(1, Ordering::SeqCst);
            self.binary_artifacts
                .lock()
                .expect("cache poisoned")
                .get(key)
                .cloned()
        }

        fn insert_binary_artifact(&self, key: &str, artifact: BinaryCompilationArtifact) {
            self.artifact_inserts.fetch_add(1, Ordering::SeqCst);
            self.binary_artifacts
                .lock()
                .expect("cache poisoned")
                .insert(key.to_string(), artifact);
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

        assert_eq!(2, cache.artifact_gets.load(Ordering::SeqCst));
        assert_eq!(1, cache.artifact_inserts.load(Ordering::SeqCst));
        assert_eq!(1, cache.raw_gets.load(Ordering::SeqCst));
        assert_eq!(1, cache.raw_inserts.load(Ordering::SeqCst));
    }

    #[test]
    fn warm_binary_cache_prefers_compiled_artifact_over_raw_wasm_bytes() {
        let cache = Arc::new(CountingCache::with_poisoned_raw_bytes());
        let config = RuntimeConfig::new().with_compilation_cache(cache.clone());
        let runtime_a = Runtime::with_config(config.clone());
        let runtime_b = Runtime::with_config(config);
        let bytes = b"\0asm\x01\0\0\0";

        runtime_a.compile(bytes).unwrap();
        let module = runtime_b
            .instantiate_binary(bytes, ModuleConfig::new())
            .unwrap();

        assert!(!module.is_closed());
        assert_eq!(2, cache.artifact_gets.load(Ordering::SeqCst));
        assert_eq!(1, cache.artifact_inserts.load(Ordering::SeqCst));
        assert_eq!(1, cache.raw_gets.load(Ordering::SeqCst));
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

        assert_eq!(0, cache.raw_gets.load(Ordering::SeqCst));
        assert_eq!(0, cache.raw_inserts.load(Ordering::SeqCst));
        assert_eq!(0, cache.artifact_gets.load(Ordering::SeqCst));
        assert_eq!(0, cache.artifact_inserts.load(Ordering::SeqCst));
    }

    #[test]
    fn secure_mode_uses_guarded_guest_memory_only_when_supported_and_preserves_oob_traps() {
        let module = include_bytes!("../../testdata/oob_load.wasm");

        for secure_mode in [false, true] {
            let runtime = Runtime::with_config(RuntimeConfig::new().with_secure_mode(secure_mode));
            let compiled = runtime.compile(module).unwrap();
            let instance = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();

            if secure_mode {
                let store = runtime.inner.store.lock().expect("runtime store poisoned");
                let module_id = instance.store_module_id().expect("guest module id");
                let memory = store
                    .instance(module_id)
                    .and_then(|module| module.memory_instance.as_ref())
                    .expect("guest memory instance");
                if supports_guard_pages() {
                    assert!(matches!(memory.bytes, MemoryBytes::Guarded { .. }));
                } else {
                    assert!(matches!(memory.bytes, MemoryBytes::Plain(..)));
                }
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
    fn secure_mode_uses_guard_pages_matches_platform_capability() {
        assert!(!secure_mode_uses_guard_pages(
            &RuntimeConfig::new().with_secure_mode(false)
        ));
        assert_eq!(
            supports_guard_pages(),
            secure_mode_uses_guard_pages(&RuntimeConfig::new().with_secure_mode(true))
        );
    }

    #[test]
    fn runtime_config_propagates_secure_memory_to_store() {
        for secure_mode in [false, true] {
            let runtime = Runtime::with_config(RuntimeConfig::new().with_secure_mode(secure_mode));
            let store = runtime.inner.store.lock().expect("runtime store poisoned");
            assert_eq!(secure_mode, store.secure_memory);
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn secure_mode_falls_back_to_plain_guest_memory_when_guard_pages_are_unsupported() {
        let runtime = Runtime::with_config(RuntimeConfig::new().with_secure_mode(true));
        let compiled = runtime
            .compile(include_bytes!("../../testdata/oob_load.wasm"))
            .unwrap();
        let instance = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let store = runtime.inner.store.lock().expect("runtime store poisoned");
        let module_id = instance.store_module_id().expect("guest module id");
        let memory = store
            .instance(module_id)
            .and_then(|module| module.memory_instance.as_ref())
            .expect("guest memory instance");

        assert!(matches!(memory.bytes, MemoryBytes::Plain(..)));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn compiler_secure_mode_falls_back_to_plain_guest_memory_when_guard_pages_are_unsupported() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler().with_secure_mode(true));
        let compiled = runtime
            .compile(include_bytes!("../../testdata/oob_load.wasm"))
            .unwrap();
        let instance = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let store = runtime.inner.store.lock().expect("runtime store poisoned");
        let module_id = instance.store_module_id().expect("guest module id");
        let memory = store
            .instance(module_id)
            .and_then(|module| module.memory_instance.as_ref())
            .expect("guest memory instance");

        assert!(matches!(memory.bytes, MemoryBytes::Plain(..)));
    }

    #[test]
    fn secure_mode_memory_growth_preserves_expected_backing() {
        let runtime = Runtime::with_config(RuntimeConfig::new().with_secure_mode(true));
        let bytes = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x04, 0x01, 0x01, 0x01, 0x03,
            0x07, 0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
        ];
        let compiled = runtime.compile(&bytes).unwrap();
        let instance = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let module_id = instance.store_module_id().expect("guest module id");
        let memory = instance.exported_memory("memory").expect("exported memory");
        let reserved_len_before = {
            let store = runtime.inner.store.lock().expect("runtime store poisoned");
            let guest_memory = store
                .instance(module_id)
                .and_then(|module| module.memory_instance.as_ref())
                .expect("guest memory instance");

            if supports_guard_pages() {
                assert!(matches!(guest_memory.bytes, MemoryBytes::Guarded { .. }));
            } else {
                assert!(matches!(guest_memory.bytes, MemoryBytes::Plain(..)));
            }

            guest_memory.bytes.reserved_len()
        };

        assert_eq!(65_536, memory.size());
        assert!(memory.write_u32_le(8, 0x1122_3344));
        assert_eq!(Some(0x1122_3344), memory.read_u32_le(8));
        assert_eq!(Some(1), memory.grow(1));
        assert_eq!(131_072, memory.size());
        assert!(memory.write_u32_le(65_536, 0x5566_7788));
        assert_eq!(Some(0x5566_7788), memory.read_u32_le(65_536));

        let store = runtime.inner.store.lock().expect("runtime store poisoned");
        let guest_memory = store
            .instance(module_id)
            .and_then(|module| module.memory_instance.as_ref())
            .expect("guest memory instance");
        if supports_guard_pages() {
            assert!(matches!(guest_memory.bytes, MemoryBytes::Guarded { .. }));
            assert_eq!(reserved_len_before, guest_memory.bytes.reserved_len());
        } else {
            assert!(matches!(guest_memory.bytes, MemoryBytes::Plain(..)));
        }
    }

    #[test]
    fn secure_mode_oob_after_memory_growth_still_traps() {
        let runtime = Runtime::with_config(RuntimeConfig::new().with_secure_mode(true));
        let bytes = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f,
            0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x05, 0x04, 0x01, 0x01, 0x01, 0x02, 0x07, 0x10,
            0x02, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, 0x03, b'o', b'o', b'b',
            0x00, 0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x28, 0x02, 0x00, 0x0b,
        ];
        let compiled = runtime.compile(&bytes).unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let memory = module.exported_memory("memory").unwrap();
        let oob = module.exported_function("oob").unwrap();

        assert_eq!(Some(1), memory.grow(1));
        assert_eq!(131_072, memory.size());
        assert_eq!(vec![0], oob.call(&[131_068]).unwrap());

        let err = oob
            .call(&[131_072])
            .expect_err("load at new boundary should still trap");
        assert!(err.to_string().contains("out of bounds memory access"));
    }

    #[test]
    fn runtime_defaults_to_interpreter_until_auto_is_safe() {
        let runtime = Runtime::new();
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f,
                0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00,
                0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let store = runtime.inner.store.lock().expect("runtime store poisoned");
        let engine = store
            .module_engine(module.store_module_id().expect("guest module id"))
            .expect("module engine should exist");

        assert!(engine.as_any().is::<InterpModuleEngine>());
    }

    #[test]
    fn auto_runtime_config_selects_compiler_engine_when_supported() {
        let runtime = Runtime::with_config(RuntimeConfig::new_auto());
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f,
                0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00,
                0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let store = runtime.inner.store.lock().expect("runtime store poisoned");
        let engine = store
            .module_engine(module.store_module_id().expect("guest module id"))
            .expect("module engine should exist");

        assert_eq!(
            compiler_supported(),
            engine.as_any().is::<CompilerModuleEngine>()
        );
        assert_eq!(
            !compiler_supported(),
            engine.as_any().is::<InterpModuleEngine>()
        );
    }

    #[test]
    fn close_on_context_done_auto_runtime_still_uses_compiler_when_supported() {
        let runtime =
            Runtime::with_config(RuntimeConfig::new_auto().with_close_on_context_done(true));
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f,
                0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00,
                0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let store = runtime.inner.store.lock().expect("runtime store poisoned");
        let engine = store
            .module_engine(module.store_module_id().expect("guest module id"))
            .expect("module engine should exist");

        assert_eq!(
            compiler_supported(),
            engine.as_any().is::<CompilerModuleEngine>()
        );
        assert_eq!(
            !compiler_supported(),
            engine.as_any().is::<InterpModuleEngine>()
        );
    }

    #[test]
    fn compiler_runtime_is_selectable_through_public_config() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler());
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f,
                0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00,
                0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let store = runtime.inner.store.lock().expect("runtime store poisoned");
        let engine = store
            .module_engine(module.store_module_id().expect("guest module id"))
            .expect("module engine should exist");

        assert!(engine.as_any().is::<CompilerModuleEngine>());
        assert_eq!(
            1,
            engine
                .as_any()
                .downcast_ref::<CompilerModuleEngine>()
                .expect("compiler engine")
                .parent()
                .function_offsets
                .len()
        );
    }

    #[test]
    fn close_on_context_done_compiler_runtime_remains_compiler() {
        if !compiler_supported() {
            return;
        }

        let runtime =
            Runtime::with_config(RuntimeConfig::new_compiler().with_close_on_context_done(true));
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f,
                0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00,
                0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let store = runtime.inner.store.lock().expect("runtime store poisoned");
        let engine = store
            .module_engine(module.store_module_id().expect("guest module id"))
            .and_then(|engine| engine.as_any().downcast_ref::<CompilerModuleEngine>())
            .expect("compiler module engine should exist");

        assert!(engine.parent().ensure_termination);
    }

    #[test]
    fn compiler_secure_mode_enables_memory_isolation_when_supported() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler().with_secure_mode(true));
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f,
                0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00,
                0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let store = runtime.inner.store.lock().expect("runtime store poisoned");
        let engine: &CompilerModuleEngine = store
            .module_engine(module.store_module_id().expect("guest module id"))
            .and_then(|engine| engine.as_any().downcast_ref::<CompilerModuleEngine>())
            .expect("compiler module engine should exist");
        let expected_memory_isolation = cfg!(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ));

        assert_eq!(
            expected_memory_isolation,
            engine.parent().memory_isolation_enabled
        );
    }

    #[test]
    fn compiler_trap_observer_reports_secure_mode_memory_faults() {
        if !compiler_supported()
            || !cfg!(target_os = "linux")
            || !cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
        {
            return;
        }

        let observations = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::with_config(RuntimeConfig::new_compiler().with_secure_mode(true));
        let compiled = runtime
            .compile(include_bytes!("../../testdata/oob_load.wasm"))
            .unwrap();
        let module = runtime
            .instantiate(&compiled, ModuleConfig::new().with_name("secure-guest"))
            .unwrap();
        let ctx = with_trap_observer(&Context::default(), {
            let observations = observations.clone();
            move |_ctx: &Context, observation: TrapObservation| {
                observations
                    .lock()
                    .expect("trap observations poisoned")
                    .push((
                        observation.module.name().unwrap_or_default().to_string(),
                        observation.cause,
                        observation.err.exit_code(),
                    ));
            }
        });

        let err = module
            .exported_function("oob")
            .unwrap()
            .call_with_context(&ctx, &[])
            .expect_err("oob should trap");

        assert!(!err.to_string().is_empty());
        assert_eq!(
            vec![("secure-guest".to_string(), TrapCause::MemoryFault, None,)],
            *observations.lock().expect("trap observations poisoned")
        );
    }

    #[test]
    fn compiler_trap_observer_reports_memory_faults_after_memory_growth() {
        if !compiler_supported()
            || !cfg!(target_os = "linux")
            || !cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
        {
            return;
        }

        let bytes = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f,
            0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x05, 0x04, 0x01, 0x01, 0x01, 0x02, 0x07, 0x10,
            0x02, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, 0x03, b'o', b'o', b'b',
            0x00, 0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x28, 0x02, 0x00, 0x0b,
        ];
        let observations = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::with_config(RuntimeConfig::new_compiler().with_secure_mode(true));
        let compiled = runtime.compile(&bytes).unwrap();
        let module = runtime
            .instantiate(
                &compiled,
                ModuleConfig::new().with_name("grown-secure-guest"),
            )
            .unwrap();
        let memory = module.exported_memory("memory").unwrap();
        let ctx = with_trap_observer(&Context::default(), {
            let observations = observations.clone();
            move |_ctx: &Context, observation: TrapObservation| {
                observations
                    .lock()
                    .expect("trap observations poisoned")
                    .push((
                        observation.module.name().unwrap_or_default().to_string(),
                        observation.cause,
                        observation.err.exit_code(),
                    ));
            }
        });

        assert_eq!(Some(1), memory.grow(1));
        assert_eq!(131_072, memory.size());
        assert_eq!(
            vec![0],
            module
                .exported_function("oob")
                .unwrap()
                .call_with_context(&ctx, &[131_068])
                .unwrap()
        );

        let err = module
            .exported_function("oob")
            .unwrap()
            .call_with_context(&ctx, &[131_072])
            .expect_err("load at grown boundary should trap");

        assert!(!err.to_string().is_empty());
        assert_eq!(
            vec![(
                "grown-secure-guest".to_string(),
                TrapCause::MemoryFault,
                None,
            )],
            *observations.lock().expect("trap observations poisoned")
        );
    }

    #[test]
    fn trap_observer_reports_start_function_traps() {
        let module = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00, 0x08, 0x01, 0x00, 0x0a, 0x05, 0x01, 0x03, 0x00, 0x00, 0x0b,
        ];
        let observations = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::with_config(RuntimeConfig::new_interpreter());
        let compiled = runtime.compile(&module).unwrap();
        let ctx = with_trap_observer(&Context::default(), {
            let observations = observations.clone();
            move |_ctx: &Context, observation: TrapObservation| {
                observations
                    .lock()
                    .expect("trap observations poisoned")
                    .push((
                        observation.module.name().unwrap_or_default().to_string(),
                        observation.cause,
                        observation.err.exit_code(),
                    ));
            }
        });

        let err = match runtime.instantiate_with_context(
            &ctx,
            &compiled,
            ModuleConfig::new().with_name("start-guest"),
        ) {
            Ok(_) => panic!("start should trap"),
            Err(err) => err,
        };

        assert!(!err.to_string().is_empty());
        assert_eq!(
            vec![("start-guest".to_string(), TrapCause::Unreachable, None)],
            *observations.lock().expect("trap observations poisoned")
        );
    }

    #[test]
    fn compiler_guest_execution_executes_through_public_runtime() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler());
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f,
                0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00,
                0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let store = runtime.inner.store.lock().expect("runtime store poisoned");
        let engine = store
            .module_engine(module.store_module_id().expect("guest module id"))
            .and_then(|engine| engine.as_any().downcast_ref::<CompilerModuleEngine>())
            .expect("compiler module engine should exist");
        let mut direct = engine.clone().new_compiler_function(0);
        assert_ne!(
            0,
            direct.executable_ptr(),
            "direct call engine lost executable"
        );
        drop(store);
        assert_eq!(vec![42], direct.call(&mut [41]).unwrap().to_vec());
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

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct ImportObserverRecord {
        import_module: String,
        event: crate::ImportResolverEvent,
        resolved: bool,
    }

    #[derive(Clone)]
    struct RecordingImportResolverObserver {
        events: Arc<Mutex<Vec<ImportObserverRecord>>>,
    }

    impl crate::ImportResolverObserver for RecordingImportResolverObserver {
        fn observe_import_resolution(
            &self,
            _ctx: &Context,
            observation: crate::ImportResolverObservation,
        ) {
            self.events
                .lock()
                .expect("import observer events poisoned")
                .push(ImportObserverRecord {
                    import_module: observation.import_module,
                    event: observation.event,
                    resolved: observation.resolved_module.is_some(),
                });
        }
    }

    fn compile_guest_with_start_import(runtime: &Runtime, import_module: &str) -> CompiledModule {
        let mut module = vec![
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x02,
        ];
        let import_len = import_module.len();
        assert!(
            import_len < 0x80,
            "test helper only supports short module names"
        );
        module.push((import_len + 10) as u8);
        module.push(0x01);
        module.push(import_len as u8);
        module.extend_from_slice(import_module.as_bytes());
        module.extend_from_slice(&[
            0x05, b's', b't', b'a', b'r', b't', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07,
            0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01, 0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00,
            0x0b,
        ]);
        runtime.compile(&module).unwrap()
    }

    #[test]
    fn import_resolver_acl_allows_store_fallback_by_exact_name() {
        let runtime = Runtime::new();
        let store_count = Arc::new(AtomicU32::new(0));
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                {
                    let store_count = store_count.clone();
                    move |_ctx, _module, _params| {
                        store_count.fetch_add(1, Ordering::SeqCst);
                        Ok(Vec::new())
                    }
                },
                &[],
                &[],
            )
            .with_name("start")
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = crate::experimental::with_import_resolver_acl(
            &Context::default(),
            crate::experimental::ImportACL::new().allow_modules(["env"]),
        );
        let compiled_guest = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
                0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r', b't', 0x00,
                0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01,
                0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
            ])
            .unwrap();

        let guest = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .unwrap();
        guest.exported_function("run").unwrap().call(&[]).unwrap();

        assert_eq!(1, store_count.load(Ordering::SeqCst));
    }

    #[test]
    fn import_resolver_observer_reports_acl_allowed_then_store_fallback() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = crate::with_import_resolver_observer(
            &crate::experimental::with_import_resolver_acl(
                &Context::default(),
                crate::experimental::ImportACL::new().allow_modules(["env"]),
            ),
            RecordingImportResolverObserver {
                events: events.clone(),
            },
        );
        let compiled_guest = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
                0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r', b't', 0x00,
                0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01,
                0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
            ])
            .unwrap();

        let guest = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .unwrap();
        guest.exported_function("run").unwrap().call(&[]).unwrap();

        assert_eq!(
            vec![
                ImportObserverRecord {
                    import_module: "env".to_string(),
                    event: crate::ImportResolverEvent::AclAllowed,
                    resolved: false,
                },
                ImportObserverRecord {
                    import_module: "env".to_string(),
                    event: crate::ImportResolverEvent::StoreFallback,
                    resolved: false,
                },
            ],
            *events.lock().expect("import observer events poisoned")
        );
    }

    #[test]
    fn import_resolver_acl_allows_store_fallback_by_prefix() {
        let runtime = Runtime::new();
        let store_count = Arc::new(AtomicU32::new(0));
        runtime
            .new_host_module_builder("wasi_snapshot_preview1")
            .new_function_builder()
            .with_func(
                {
                    let store_count = store_count.clone();
                    move |_ctx, _module, _params| {
                        store_count.fetch_add(1, Ordering::SeqCst);
                        Ok(Vec::new())
                    }
                },
                &[],
                &[],
            )
            .with_name("start")
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = crate::experimental::with_import_resolver_acl(
            &Context::default(),
            crate::experimental::ImportACL::new().allow_module_prefixes(["wasi_"]),
        );
        let compiled_guest = compile_guest_with_start_import(&runtime, "wasi_snapshot_preview1");

        let guest = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .unwrap();
        guest.exported_function("run").unwrap().call(&[]).unwrap();

        assert_eq!(1, store_count.load(Ordering::SeqCst));
    }

    #[test]
    fn import_resolver_acl_denies_store_import_by_exact_name() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = crate::experimental::with_import_resolver_acl(
            &Context::default(),
            crate::experimental::ImportACL::new().deny_modules(["env"]),
        );
        let compiled_guest = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
                0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r', b't', 0x00,
                0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01,
                0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
            ])
            .unwrap();

        let err = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .err()
            .expect("instantiation should fail");
        assert!(err.to_string().contains("module[env] denied by import ACL"));
    }

    #[test]
    fn import_resolver_observer_reports_acl_denial() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = crate::with_import_resolver_observer(
            &crate::experimental::with_import_resolver_acl(
                &Context::default(),
                crate::experimental::ImportACL::new().deny_modules(["env"]),
            ),
            RecordingImportResolverObserver {
                events: events.clone(),
            },
        );
        let compiled_guest = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
                0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r', b't', 0x00,
                0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01,
                0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
            ])
            .unwrap();

        let err = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .err()
            .expect("instantiation should fail");
        assert!(err.to_string().contains("module[env] denied by import ACL"));
        assert_eq!(
            vec![ImportObserverRecord {
                import_module: "env".to_string(),
                event: crate::ImportResolverEvent::AclDenied,
                resolved: false,
            }],
            *events.lock().expect("import observer events poisoned")
        );
    }

    #[test]
    fn import_resolver_acl_denies_store_import_by_prefix() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("private.env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = crate::experimental::with_import_resolver_acl(
            &Context::default(),
            crate::experimental::ImportACL::new().deny_module_prefixes(["private."]),
        );
        let compiled_guest = compile_guest_with_start_import(&runtime, "private.env");

        let err = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .err()
            .expect("instantiation should fail");
        assert!(err
            .to_string()
            .contains("module[private.env] denied by import ACL"));
    }

    #[test]
    fn import_resolver_acl_deny_prefix_takes_precedence_over_allow_prefix() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env.internal")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = crate::experimental::with_import_resolver_acl(
            &Context::default(),
            crate::experimental::ImportACL::new()
                .allow_module_prefixes(["env."])
                .deny_module_prefixes(["env.internal"]),
        );
        let compiled_guest = compile_guest_with_start_import(&runtime, "env.internal");

        let err = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .err()
            .expect("instantiation should fail");
        assert!(err
            .to_string()
            .contains("module[env.internal] denied by import ACL"));
    }

    #[test]
    fn import_resolver_acl_allowlist_blocks_unlisted_import() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = crate::experimental::with_import_resolver_acl(
            &Context::default(),
            crate::experimental::ImportACL::new().allow_modules(["wasi_snapshot_preview1"]),
        );
        let compiled_guest = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
                0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r', b't', 0x00,
                0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01,
                0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
            ])
            .unwrap();

        let err = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .err()
            .expect("instantiation should fail");
        assert!(err
            .to_string()
            .contains("module[env] not allowed by import ACL"));
    }

    #[test]
    fn import_resolver_observer_reports_acl_allowed_then_store_fallback_by_prefix() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("wasi_snapshot_preview1")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = crate::with_import_resolver_observer(
            &crate::experimental::with_import_resolver_acl(
                &Context::default(),
                crate::experimental::ImportACL::new().allow_module_prefixes(["wasi_"]),
            ),
            RecordingImportResolverObserver {
                events: events.clone(),
            },
        );
        let compiled_guest = compile_guest_with_start_import(&runtime, "wasi_snapshot_preview1");

        let guest = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .unwrap();
        guest.exported_function("run").unwrap().call(&[]).unwrap();

        assert_eq!(
            vec![
                ImportObserverRecord {
                    import_module: "wasi_snapshot_preview1".to_string(),
                    event: crate::ImportResolverEvent::AclAllowed,
                    resolved: false,
                },
                ImportObserverRecord {
                    import_module: "wasi_snapshot_preview1".to_string(),
                    event: crate::ImportResolverEvent::StoreFallback,
                    resolved: false,
                },
            ],
            *events.lock().expect("import observer events poisoned")
        );
    }

    #[test]
    fn import_resolver_config_fail_closed_blocks_store_fallback() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = crate::experimental::with_import_resolver_config(
            &Context::default(),
            crate::experimental::ImportResolverConfig {
                acl: Some(crate::experimental::ImportACL::new().allow_modules(["env"])),
                fail_closed: true,
                ..crate::experimental::ImportResolverConfig::default()
            },
        );
        let compiled_guest = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
                0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r', b't', 0x00,
                0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01,
                0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
            ])
            .unwrap();

        let err = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .err()
            .expect("instantiation should fail");
        assert!(err
            .to_string()
            .contains("module[env] unresolved by import resolver"));
    }

    #[test]
    fn import_resolver_observer_reports_fail_closed_denial() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = crate::with_import_resolver_observer(
            &crate::experimental::with_import_resolver_config(
                &Context::default(),
                crate::experimental::ImportResolverConfig {
                    acl: Some(crate::experimental::ImportACL::new().allow_modules(["env"])),
                    fail_closed: true,
                    ..crate::experimental::ImportResolverConfig::default()
                },
            ),
            RecordingImportResolverObserver {
                events: events.clone(),
            },
        );
        let compiled_guest = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
                0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r', b't', 0x00,
                0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01,
                0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
            ])
            .unwrap();

        let err = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .err()
            .expect("instantiation should fail");
        assert!(err
            .to_string()
            .contains("module[env] unresolved by import resolver"));
        assert_eq!(
            vec![
                ImportObserverRecord {
                    import_module: "env".to_string(),
                    event: crate::ImportResolverEvent::AclAllowed,
                    resolved: false,
                },
                ImportObserverRecord {
                    import_module: "env".to_string(),
                    event: crate::ImportResolverEvent::FailClosedDenied,
                    resolved: false,
                },
            ],
            *events.lock().expect("import observer events poisoned")
        );
    }

    #[test]
    fn import_resolver_config_resolver_links_anonymous_module_instances() {
        let runtime = Runtime::new();
        let resolved_count = Arc::new(AtomicU32::new(0));
        let compiled_host = runtime
            .new_host_module_builder("env0")
            .new_function_builder()
            .with_func(
                {
                    let resolved_count = resolved_count.clone();
                    move |_ctx, _module, _params| {
                        resolved_count.fetch_add(1, Ordering::SeqCst);
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

        let ctx = crate::experimental::with_import_resolver_config(
            &Context::default(),
            crate::experimental::ImportResolverConfig {
                resolver: Some(Arc::new(move |name| {
                    (name == "env").then_some(anonymous_import.clone())
                })),
                ..crate::experimental::ImportResolverConfig::default()
            },
        );
        let compiled_guest = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
                0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r', b't', 0x00,
                0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01,
                0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
            ])
            .unwrap();

        let guest = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .unwrap();
        guest.exported_function("run").unwrap().call(&[]).unwrap();

        assert_eq!(1, resolved_count.load(Ordering::SeqCst));
    }

    #[test]
    fn import_resolver_observer_reports_resolver_resolution() {
        let runtime = Runtime::new();
        let resolved_count = Arc::new(AtomicU32::new(0));
        let compiled_host = runtime
            .new_host_module_builder("env0")
            .new_function_builder()
            .with_func(
                {
                    let resolved_count = resolved_count.clone();
                    move |_ctx, _module, _params| {
                        resolved_count.fetch_add(1, Ordering::SeqCst);
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

        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = crate::with_import_resolver_observer(
            &crate::experimental::with_import_resolver_config(
                &Context::default(),
                crate::experimental::ImportResolverConfig {
                    resolver: Some(Arc::new(move |name| {
                        (name == "env").then_some(anonymous_import.clone())
                    })),
                    ..crate::experimental::ImportResolverConfig::default()
                },
            ),
            RecordingImportResolverObserver {
                events: events.clone(),
            },
        );
        let compiled_guest = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
                0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r', b't', 0x00,
                0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01,
                0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
            ])
            .unwrap();

        let guest = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .unwrap();
        guest.exported_function("run").unwrap().call(&[]).unwrap();

        assert_eq!(1, resolved_count.load(Ordering::SeqCst));
        assert_eq!(
            vec![
                ImportObserverRecord {
                    import_module: "env".to_string(),
                    event: crate::ImportResolverEvent::ResolverAttempted,
                    resolved: false,
                },
                ImportObserverRecord {
                    import_module: "env".to_string(),
                    event: crate::ImportResolverEvent::ResolverResolved,
                    resolved: true,
                },
            ],
            *events.lock().expect("import observer events poisoned")
        );
    }

    #[test]
    fn import_resolver_observer_reports_resolver_attempt_before_store_fallback() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = crate::with_import_resolver_observer(
            &crate::experimental::with_import_resolver_config(
                &Context::default(),
                crate::experimental::ImportResolverConfig {
                    resolver: Some(Arc::new(|_| None)),
                    ..crate::experimental::ImportResolverConfig::default()
                },
            ),
            RecordingImportResolverObserver {
                events: events.clone(),
            },
        );
        let compiled_guest = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
                0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r', b't', 0x00,
                0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01,
                0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
            ])
            .unwrap();

        let guest = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .unwrap();
        guest.exported_function("run").unwrap().call(&[]).unwrap();

        assert_eq!(
            vec![
                ImportObserverRecord {
                    import_module: "env".to_string(),
                    event: crate::ImportResolverEvent::ResolverAttempted,
                    resolved: false,
                },
                ImportObserverRecord {
                    import_module: "env".to_string(),
                    event: crate::ImportResolverEvent::StoreFallback,
                    resolved: false,
                },
            ],
            *events.lock().expect("import observer events poisoned")
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
    fn guest_host_callbacks_receive_host_function_definition_metadata() {
        let mut configs = vec![RuntimeConfig::new_interpreter(), RuntimeConfig::new_auto()];
        if compiler_supported() {
            configs.push(RuntimeConfig::new_compiler());
        }

        for config in configs {
            let runtime = Runtime::with_config(config);
            let observed = Arc::new(Mutex::new(Vec::new()));
            runtime
                .new_host_module_builder("env")
                .new_function_builder()
                .with_func(
                    {
                        let observed = observed.clone();
                        move |ctx, _module, _params| {
                            observed.lock().expect("observed metadata poisoned").push(
                                ctx.invocation
                                    .as_ref()
                                    .and_then(|invocation| invocation.function_definition.clone())
                                    .expect("host callback should receive function definition"),
                            );
                            Ok(vec![7])
                        }
                    },
                    &[ValueType::I32],
                    &[ValueType::I32],
                )
                .with_name("hook_impl")
                .with_parameter_names(&["value"])
                .with_result_names(&["result"])
                .export("hook")
                .instantiate(&Context::default())
                .unwrap();

            let guest = runtime
                .instantiate_binary(
                    &[
                        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60,
                        0x01, 0x7f, 0x01, 0x7f, 0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04,
                        b'h', b'o', b'o', b'k', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07,
                        0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01, 0x0a, 0x08, 0x01, 0x06, 0x00,
                        0x41, 0x2a, 0x10, 0x00, 0x0b,
                    ],
                    ModuleConfig::new().with_name("guest"),
                )
                .unwrap();

            let results = guest.exported_function("run").unwrap().call(&[0]).unwrap();
            assert_eq!(vec![7], results);

            let observed = observed.lock().expect("observed metadata poisoned");
            assert_eq!(1, observed.len());
            let definition = &observed[0];
            assert_eq!(Some("env"), definition.module_name());
            assert_eq!("hook_impl", definition.name());
            assert_eq!(&[ValueType::I32], definition.param_types());
            assert_eq!(&[ValueType::I32], definition.result_types());
            assert_eq!(&["hook".to_string()], definition.export_names());
            assert_eq!(&["value".to_string()], definition.param_names());
            assert_eq!(&["result".to_string()], definition.result_names());
        }
    }

    #[test]
    fn host_call_policy_denies_guest_host_callbacks_across_runtimes() {
        for config in policy_runtime_configs() {
            let runtime = Runtime::with_config(config.with_host_call_policy(deny_env_host_calls));
            let called = Arc::new(AtomicU32::new(0));
            let observations = Arc::new(Mutex::new(Vec::new()));
            runtime
                .new_host_module_builder("env")
                .new_function_builder()
                .with_func(
                    {
                        let called = called.clone();
                        move |_ctx, _module, _params| {
                            called.fetch_add(1, Ordering::SeqCst);
                            Ok(vec![7])
                        }
                    },
                    &[ValueType::I32],
                    &[ValueType::I32],
                )
                .with_name("hook_impl")
                .export("hook")
                .instantiate(&Context::default())
                .unwrap();

            let guest = runtime
                .instantiate_binary(
                    &[
                        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60,
                        0x01, 0x7f, 0x01, 0x7f, 0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04,
                        b'h', b'o', b'o', b'k', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07,
                        0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01, 0x0a, 0x08, 0x01, 0x06, 0x00,
                        0x41, 0x2a, 0x10, 0x00, 0x0b,
                    ],
                    ModuleConfig::new().with_name("guest"),
                )
                .unwrap();
            let ctx = with_trap_observer(&Context::default(), {
                let observations = observations.clone();
                move |_ctx: &Context, observation: TrapObservation| {
                    observations
                        .lock()
                        .expect("trap observations poisoned")
                        .push((observation.cause, observation.err.to_string()));
                }
            });

            let err = guest
                .exported_function("run")
                .unwrap()
                .call_with_context(&ctx, &[0])
                .unwrap_err();
            assert_eq!("policy denied: host call", err.to_string());
            assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
            assert_eq!(0, called.load(Ordering::SeqCst));
            assert_eq!(
                vec![(
                    TrapCause::PolicyDenied,
                    "policy denied: host call".to_string()
                )],
                *observations.lock().expect("trap observations poisoned")
            );
        }
    }

    #[test]
    fn module_imported_function_definitions_expose_import_metadata() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                |_ctx, _module, _params| Ok(vec![7]),
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("hook_impl")
            .with_parameter_names(&["value"])
            .with_result_names(&["result"])
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01,
                    0x7f, 0x01, 0x7f, 0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04, b'h', b'o',
                    b'o', b'k', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r',
                    b'u', b'n', 0x00, 0x01, 0x0a, 0x08, 0x01, 0x06, 0x00, 0x41, 0x2a, 0x10, 0x00,
                    0x0b,
                ],
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let imported = guest.imported_function_definitions();
        assert_eq!(1, imported.len());
        let definition = &imported[0];
        assert_eq!("", definition.name());
        assert_eq!(None, definition.module_name());
        assert_eq!(Some(("env", "hook")), definition.import());
        assert_eq!(&[ValueType::I32], definition.param_types());
        assert_eq!(&[ValueType::I32], definition.result_types());
    }

    #[test]
    fn module_imported_memory_definitions_expose_import_metadata() {
        let runtime = Runtime::new();
        runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01,
                    0x07, 0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
                ],
                ModuleConfig::new().with_name("env"),
            )
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00,
                    0x00, 0x02, 0x0f, 0x01, 0x03, b'e', b'n', b'v', 0x06, b'm', b'e', b'm', b'o',
                    b'r', b'y', 0x02, 0x00, 0x01, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03,
                    b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b,
                ],
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let imported = guest.imported_memory_definitions();
        assert_eq!(1, imported.len());
        let definition = &imported[0];
        assert_eq!(None, definition.module_name());
        assert_eq!(Some(("env", "memory")), definition.import());
        assert_eq!(1, definition.minimum_pages());
        assert_eq!(None, definition.maximum_pages());
    }

    #[test]
    fn module_imported_global_definitions_expose_import_metadata() {
        let runtime = Runtime::new();
        runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x06, 0x06, 0x01, 0x7e, 0x01,
                    0x42, 0x2a, 0x0b, 0x07, 0x0b, 0x01, 0x07, b'c', b'o', b'u', b'n', b't', b'e',
                    b'r', 0x03, 0x00,
                ],
                ModuleConfig::new().with_name("env"),
            )
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x02, 0x10, 0x01, 0x03, b'e',
                    b'n', b'v', 0x07, b'c', b'o', b'u', b'n', b't', b'e', b'r', 0x03, 0x7e, 0x01,
                ],
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let imported = guest.imported_global_definitions();
        assert_eq!(1, imported.len());
        let definition = &imported[0];
        assert_eq!(ValueType::I64, definition.value_type());
        assert!(definition.is_mutable());
        assert_eq!(None, definition.module_name());
        assert_eq!(Some(("env", "counter")), definition.import());
        assert!(definition.export_names().is_empty());
    }

    #[test]
    fn module_imported_table_definitions_expose_import_metadata() {
        let runtime = Runtime::new();
        runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x04, 0x05, 0x01, 0x70, 0x01,
                    0x01, 0x02, 0x07, 0x09, 0x01, 0x05, b't', b'a', b'b', b'l', b'e', 0x01, 0x00,
                ],
                ModuleConfig::new().with_name("env"),
            )
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x02, 0x10, 0x01, 0x03, b'e',
                    b'n', b'v', 0x05, b't', b'a', b'b', b'l', b'e', 0x01, 0x70, 0x01, 0x01, 0x02,
                ],
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let imported = guest.imported_table_definitions();
        assert_eq!(1, imported.len());
        let definition = &imported[0];
        assert_eq!(ValueType::FuncRef, definition.ref_type());
        assert_eq!(1, definition.minimum());
        assert_eq!(Some(2), definition.maximum());
        assert_eq!(None, definition.module_name());
        assert_eq!(Some(("env", "table")), definition.import());
        assert!(definition.export_names().is_empty());
    }

    #[test]
    fn module_exported_global_definitions_expose_export_metadata() {
        let runtime = Runtime::new();
        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x06, 0x06, 0x01, 0x7e, 0x01,
                    0x42, 0x2a, 0x0b, 0x07, 0x13, 0x02, 0x07, b'c', b'o', b'u', b'n', b't', b'e',
                    b'r', 0x03, 0x00, 0x05, b'a', b'l', b'i', b'a', b's', 0x03, 0x00,
                ],
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let exported = guest.exported_global_definitions();
        assert_eq!(2, exported.len());
        let counter = exported.get("counter").unwrap();
        let alias = exported.get("alias").unwrap();
        assert_eq!(ValueType::I64, counter.value_type());
        assert!(counter.is_mutable());
        assert_eq!(Some("guest"), counter.module_name());
        assert_eq!(None, counter.import());
        assert_eq!(
            &["counter".to_string(), "alias".to_string()],
            counter.export_names()
        );
        assert_eq!(counter, alias);
    }

    #[test]
    fn module_exported_table_definitions_expose_export_metadata() {
        let runtime = Runtime::new();
        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x04, 0x05, 0x01, 0x70, 0x01,
                    0x01, 0x02, 0x07, 0x11, 0x02, 0x05, b't', b'a', b'b', b'l', b'e', 0x01, 0x00,
                    0x05, b'a', b'l', b'i', b'a', b's', 0x01, 0x00,
                ],
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let exported = guest.exported_table_definitions();
        assert_eq!(2, exported.len());
        let table = exported.get("table").unwrap();
        let alias = exported.get("alias").unwrap();
        assert_eq!(ValueType::FuncRef, table.ref_type());
        assert_eq!(1, table.minimum());
        assert_eq!(Some(2), table.maximum());
        assert_eq!(Some("guest"), table.module_name());
        assert_eq!(None, table.import());
        assert_eq!(
            &["table".to_string(), "alias".to_string()],
            table.export_names()
        );
        assert_eq!(table, alias);
    }

    #[test]
    fn host_call_policy_denies_direct_host_function_calls() {
        let runtime = Runtime::new();
        let called = Arc::new(AtomicU32::new(0));
        let observations = Arc::new(Mutex::new(Vec::new()));
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                {
                    let called = called.clone();
                    move |_ctx, _module, params| {
                        called.fetch_add(1, Ordering::SeqCst);
                        Ok(vec![params[0]])
                    }
                },
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("hook_impl")
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let policy_ctx = with_host_call_policy(&Context::default(), deny_hook_impl_host_call);
        let ctx = with_trap_observer(&policy_ctx, {
            let observations = observations.clone();
            move |_ctx: &Context, observation: TrapObservation| {
                observations
                    .lock()
                    .expect("trap observations poisoned")
                    .push((observation.cause, observation.err.to_string()));
            }
        });
        let err = module
            .exported_function("hook")
            .unwrap()
            .call_with_context(&ctx, &[1])
            .unwrap_err();
        assert_eq!("policy denied: host call", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
        assert_eq!(0, called.load(Ordering::SeqCst));
        assert_eq!(
            vec![(
                TrapCause::PolicyDenied,
                "policy denied: host call".to_string()
            )],
            *observations.lock().expect("trap observations poisoned")
        );
    }

    #[test]
    fn host_call_policy_observer_fires_before_direct_host_call_denial_trap() {
        let runtime = Runtime::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                |_ctx, _module, params| Ok(vec![params[0]]),
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("hook_impl")
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let policy_ctx = with_host_call_policy(&Context::default(), deny_hook_impl_host_call);
        let observer_ctx = with_host_call_policy_observer(&policy_ctx, {
            let events = events.clone();
            move |_ctx: &Context, observation: HostCallPolicyObservation| {
                events
                    .lock()
                    .expect("host-call observer events poisoned")
                    .push((
                        "policy".to_string(),
                        observation.module.name().map(str::to_string),
                        observation.request.name().map(str::to_string),
                        observation.request.caller_module_name().map(str::to_string),
                        observation.decision,
                    ));
            }
        });
        let ctx = with_trap_observer(&observer_ctx, {
            let events = events.clone();
            move |_ctx: &Context, observation: TrapObservation| {
                events
                    .lock()
                    .expect("host-call observer events poisoned")
                    .push((
                        "trap".to_string(),
                        observation.module.name().map(str::to_string),
                        None,
                        None,
                        HostCallPolicyDecision::Denied,
                    ));
            }
        });

        let err = module
            .exported_function("hook")
            .unwrap()
            .call_with_context(&ctx, &[1])
            .unwrap_err();
        assert_eq!("policy denied: host call", err.to_string());
        assert_eq!(
            vec![
                (
                    "policy".to_string(),
                    Some("env".to_string()),
                    Some("hook_impl".to_string()),
                    Some("env".to_string()),
                    HostCallPolicyDecision::Denied,
                ),
                (
                    "trap".to_string(),
                    Some("env".to_string()),
                    None,
                    None,
                    HostCallPolicyDecision::Denied,
                ),
            ],
            *events.lock().expect("host-call observer events poisoned")
        );
    }

    #[test]
    fn host_call_policy_tracks_caller_module_name_on_direct_host_export() {
        let runtime = Runtime::new();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let module = runtime
            .new_host_module_builder("example")
            .new_function_builder()
            .with_func(
                |_ctx, _module, params| Ok(vec![params[0]]),
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("hook_impl")
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = with_host_call_policy(&Context::default(), {
            let observed = observed.clone();
            move |_ctx: &Context, request: &HostCallPolicyRequest| {
                observed
                    .lock()
                    .expect("observed host-call metadata poisoned")
                    .push((
                        request.caller_module_name().map(str::to_string),
                        request
                            .function
                            .clone()
                            .expect("host call policy should receive metadata"),
                    ));
                false
            }
        });
        let err = module
            .exported_function("hook")
            .unwrap()
            .call_with_context(&ctx, &[1])
            .unwrap_err();
        assert_eq!("policy denied: host call", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));

        let observed = observed
            .lock()
            .expect("observed host-call metadata poisoned");
        assert_eq!(1, observed.len());
        let (caller_module, definition) = &observed[0];
        assert_eq!(Some("example".to_string()), *caller_module);
        assert_eq!("hook_impl", definition.name());
        assert_eq!(Some("example"), definition.module_name());
        assert_eq!(&["hook".to_string()], definition.export_names());
    }

    #[test]
    fn host_call_policy_observer_records_allowed_guest_host_callbacks() {
        let runtime = Runtime::new();
        let called = Arc::new(AtomicU32::new(0));
        let observations = Arc::new(Mutex::new(Vec::new()));
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                {
                    let called = called.clone();
                    move |_ctx, _module, _params| {
                        called.fetch_add(1, Ordering::SeqCst);
                        Ok(vec![7])
                    }
                },
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("hook_impl")
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01,
                    0x7f, 0x01, 0x7f, 0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04, b'h', b'o',
                    b'o', b'k', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r',
                    b'u', b'n', 0x00, 0x01, 0x0a, 0x08, 0x01, 0x06, 0x00, 0x41, 0x2a, 0x10, 0x00,
                    0x0b,
                ],
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let policy_ctx = with_host_call_policy(&Context::default(), allow_all_host_calls);
        let ctx = with_host_call_policy_observer(&policy_ctx, {
            let observations = observations.clone();
            move |_ctx: &Context, observation: HostCallPolicyObservation| {
                observations
                    .lock()
                    .expect("host-call observer observations poisoned")
                    .push((
                        observation.module.name().map(str::to_string),
                        observation.request.name().map(str::to_string),
                        observation.request.caller_module_name().map(str::to_string),
                        observation.decision,
                    ));
            }
        });

        let results = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&ctx, &[0])
            .unwrap();
        assert_eq!(vec![7], results);
        assert_eq!(1, called.load(Ordering::SeqCst));
        assert_eq!(
            vec![(
                Some("guest".to_string()),
                Some("hook_impl".to_string()),
                Some("guest".to_string()),
                HostCallPolicyDecision::Allowed,
            )],
            *observations
                .lock()
                .expect("host-call observer observations poisoned")
        );
    }

    #[test]
    fn host_call_policy_observer_fires_before_trap_for_denied_guest_host_callbacks() {
        let runtime = Runtime::new();
        let called = Arc::new(AtomicU32::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                {
                    let called = called.clone();
                    move |_ctx, _module, _params| {
                        called.fetch_add(1, Ordering::SeqCst);
                        Ok(vec![7])
                    }
                },
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("hook_impl")
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01,
                    0x7f, 0x01, 0x7f, 0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04, b'h', b'o',
                    b'o', b'k', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r',
                    b'u', b'n', 0x00, 0x01, 0x0a, 0x08, 0x01, 0x06, 0x00, 0x41, 0x2a, 0x10, 0x00,
                    0x0b,
                ],
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let policy_ctx = with_host_call_policy(&Context::default(), deny_env_host_calls);
        let observer_ctx = with_host_call_policy_observer(&policy_ctx, {
            let events = events.clone();
            move |_ctx: &Context, observation: HostCallPolicyObservation| {
                events
                    .lock()
                    .expect("host-call observer events poisoned")
                    .push((
                        "policy".to_string(),
                        observation.module.name().map(str::to_string),
                        observation.request.name().map(str::to_string),
                        observation.request.caller_module_name().map(str::to_string),
                        observation.decision,
                    ));
            }
        });
        let ctx = with_trap_observer(&observer_ctx, {
            let events = events.clone();
            move |_ctx: &Context, observation: TrapObservation| {
                events
                    .lock()
                    .expect("host-call observer events poisoned")
                    .push((
                        "trap".to_string(),
                        observation.module.name().map(str::to_string),
                        None,
                        None,
                        HostCallPolicyDecision::Denied,
                    ));
            }
        });

        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&ctx, &[0])
            .unwrap_err();
        assert_eq!("policy denied: host call", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
        assert_eq!(0, called.load(Ordering::SeqCst));
        assert_eq!(
            vec![
                (
                    "policy".to_string(),
                    Some("guest".to_string()),
                    Some("hook_impl".to_string()),
                    Some("guest".to_string()),
                    HostCallPolicyDecision::Denied,
                ),
                (
                    "trap".to_string(),
                    Some("guest".to_string()),
                    None,
                    None,
                    HostCallPolicyDecision::Denied,
                ),
            ],
            *events.lock().expect("host-call observer events poisoned")
        );
    }

    #[test]
    fn host_call_policy_denial_short_circuits_yield_policy_and_yield_observer() {
        let runtime = Runtime::new();
        let called = Arc::new(AtomicU32::new(0));
        let yield_policy_events = Arc::new(Mutex::new(Vec::new()));
        let yield_observer_events = Arc::new(Mutex::new(Vec::new()));
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                {
                    let called = called.clone();
                    move |ctx, _module, _params| {
                        called.fetch_add(1, Ordering::SeqCst);
                        get_yielder(&ctx)
                            .expect("yielder should be injected")
                            .r#yield();
                        Ok(Vec::new())
                    }
                },
                &[],
                &[],
            )
            .with_name("hook_impl")
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00,
                    0x00, 0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04, b'h', b'o', b'o', b'k',
                    0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n',
                    0x00, 0x01, 0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
                ],
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let policy_ctx =
            with_host_call_policy(&with_yielder(&Context::default()), deny_hook_impl_host_call);
        let yield_policy_ctx = with_yield_policy(&policy_ctx, allow_all_yields);
        let yield_policy_observer_ctx = with_yield_policy_observer(&yield_policy_ctx, {
            let yield_policy_events = yield_policy_events.clone();
            move |_ctx: &Context, observation: YieldPolicyObservation| {
                yield_policy_events
                    .lock()
                    .expect("yield-policy events poisoned")
                    .push((
                        observation.module.name().map(str::to_string),
                        observation.request.name().map(str::to_string),
                        observation.decision,
                    ));
            }
        });
        let ctx = with_yield_observer(&yield_policy_observer_ctx, {
            let yield_observer_events = yield_observer_events.clone();
            move |_ctx: &Context, observation: YieldObservation| {
                yield_observer_events
                    .lock()
                    .expect("yield observer events poisoned")
                    .push((
                        observation.module.name().map(str::to_string),
                        observation.event,
                        observation.yield_count,
                    ));
            }
        });

        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();
        assert_eq!("policy denied: host call", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
        assert_eq!(0, called.load(Ordering::SeqCst));
        assert!(
            yield_policy_events
                .lock()
                .expect("yield-policy events poisoned")
                .is_empty()
        );
        assert!(
            yield_observer_events
                .lock()
                .expect("yield observer events poisoned")
                .is_empty()
        );
    }

    #[test]
    fn host_call_policy_can_filter_direct_host_exports_by_caller_module_name() {
        let runtime = Runtime::with_config(
            RuntimeConfig::new().with_host_call_policy(deny_untrusted_caller_module),
        );
        let called = Arc::new(AtomicU32::new(0));
        let module = runtime
            .new_host_module_builder("untrusted")
            .new_function_builder()
            .with_func(
                {
                    let called = called.clone();
                    move |_ctx, _module, params| {
                        called.fetch_add(1, Ordering::SeqCst);
                        Ok(vec![params[0]])
                    }
                },
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("hook_impl")
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let err = module
            .exported_function("hook")
            .unwrap()
            .call_with_context(&Context::default(), &[7])
            .unwrap_err();
        assert_eq!("policy denied: host call", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
        assert_eq!(0, called.load(Ordering::SeqCst));
    }

    #[test]
    fn host_call_policy_context_overrides_runtime_config_policy() {
        let runtime =
            Runtime::with_config(RuntimeConfig::new().with_host_call_policy(deny_env_host_calls));
        let called = Arc::new(AtomicU32::new(0));
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                {
                    let called = called.clone();
                    move |_ctx, _module, _params| {
                        called.fetch_add(1, Ordering::SeqCst);
                        Ok(vec![7])
                    }
                },
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("hook_impl")
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01,
                    0x7f, 0x01, 0x7f, 0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04, b'h', b'o',
                    b'o', b'k', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r',
                    b'u', b'n', 0x00, 0x01, 0x0a, 0x08, 0x01, 0x06, 0x00, 0x41, 0x2a, 0x10, 0x00,
                    0x0b,
                ],
                ModuleConfig::new(),
            )
            .unwrap();

        let ctx = with_host_call_policy(&Context::default(), allow_all_host_calls);
        let results = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&ctx, &[0])
            .unwrap();
        assert_eq!(vec![7], results);
        assert_eq!(1, called.load(Ordering::SeqCst));
    }

    #[test]
    fn host_call_policy_denies_high_arity_direct_host_function_calls() {
        let runtime = Runtime::new();
        let called = Arc::new(AtomicU32::new(0));
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                {
                    let called = called.clone();
                    move |_ctx, _module, params| {
                        called.fetch_add(1, Ordering::SeqCst);
                        Ok(vec![params[0]])
                    }
                },
                &[ValueType::I32, ValueType::I32, ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("sum3")
            .export("sum3")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = with_host_call_policy(&Context::default(), deny_high_arity_host_calls);
        let err = module
            .exported_function("sum3")
            .unwrap()
            .call_with_context(&ctx, &[1, 2, 3])
            .unwrap_err();

        assert_eq!("policy denied: host call", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
        assert_eq!(0, called.load(Ordering::SeqCst));
    }

    #[test]
    fn host_call_policy_denies_multi_result_direct_host_function_calls() {
        let runtime = Runtime::new();
        let called = Arc::new(AtomicU32::new(0));
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                {
                    let called = called.clone();
                    move |_ctx, _module, _params| {
                        called.fetch_add(1, Ordering::SeqCst);
                        Ok(vec![7, 9])
                    }
                },
                &[],
                &[ValueType::I32, ValueType::I32],
            )
            .with_name("pair")
            .export("pair")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = with_host_call_policy(&Context::default(), deny_multi_result_host_calls);
        let err = module
            .exported_function("pair")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();

        assert_eq!("policy denied: host call", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
        assert_eq!(0, called.load(Ordering::SeqCst));
    }

    #[test]
    fn host_call_policy_denies_nonzero_param_guest_host_callbacks_across_runtimes() {
        for config in policy_runtime_configs() {
            let runtime =
                Runtime::with_config(config.with_host_call_policy(allow_only_zero_arg_host_calls));
            let called = Arc::new(AtomicU32::new(0));
            runtime
                .new_host_module_builder("env")
                .new_function_builder()
                .with_func(
                    {
                        let called = called.clone();
                        move |_ctx, _module, _params| {
                            called.fetch_add(1, Ordering::SeqCst);
                            Ok(vec![7])
                        }
                    },
                    &[ValueType::I32],
                    &[ValueType::I32],
                )
                .with_name("hook_impl")
                .export("hook")
                .instantiate(&Context::default())
                .unwrap();

            let guest = runtime
                .instantiate_binary(
                    &[
                        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60,
                        0x01, 0x7f, 0x01, 0x7f, 0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04,
                        b'h', b'o', b'o', b'k', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07,
                        0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01, 0x0a, 0x08, 0x01, 0x06, 0x00,
                        0x41, 0x2a, 0x10, 0x00, 0x0b,
                    ],
                    ModuleConfig::new().with_name("guest"),
                )
                .unwrap();

            let err = guest
                .exported_function("run")
                .unwrap()
                .call(&[0])
                .unwrap_err();
            assert_eq!("policy denied: host call", err.to_string());
            assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
            assert_eq!(0, called.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn import_acl_resolution_can_succeed_before_host_call_policy_denies_callback() {
        let runtime = Runtime::new();
        let called = Arc::new(AtomicU32::new(0));
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                {
                    let called = called.clone();
                    move |_ctx, _module, _params| {
                        called.fetch_add(1, Ordering::SeqCst);
                        Ok(Vec::new())
                    }
                },
                &[],
                &[],
            )
            .with_name("start")
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let import_events = Arc::new(Mutex::new(Vec::new()));
        let trap_events = Arc::new(Mutex::new(Vec::new()));
        let ctx = with_trap_observer(
            &with_host_call_policy(
                &crate::with_import_resolver_observer(
                    &crate::experimental::with_import_resolver_acl(
                        &Context::default(),
                        crate::experimental::ImportACL::new().allow_modules(["env"]),
                    ),
                    RecordingImportResolverObserver {
                        events: import_events.clone(),
                    },
                ),
                deny_env_host_calls,
            ),
            {
                let trap_events = trap_events.clone();
                move |_ctx: &Context, observation: TrapObservation| {
                    trap_events
                        .lock()
                        .expect("trap observations poisoned")
                        .push((observation.cause, observation.err.to_string()));
                }
            },
        );
        let compiled_guest = compile_guest_with_start_import(&runtime, "env");

        let guest = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .unwrap();
        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();

        assert_eq!("policy denied: host call", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
        assert_eq!(0, called.load(Ordering::SeqCst));
        assert_eq!(
            vec![
                ImportObserverRecord {
                    import_module: "env".to_string(),
                    event: crate::ImportResolverEvent::AclAllowed,
                    resolved: false,
                },
                ImportObserverRecord {
                    import_module: "env".to_string(),
                    event: crate::ImportResolverEvent::StoreFallback,
                    resolved: false,
                },
            ],
            *import_events
                .lock()
                .expect("import observer events poisoned")
        );
        assert_eq!(
            vec![(
                TrapCause::PolicyDenied,
                "policy denied: host call".to_string()
            )],
            *trap_events.lock().expect("trap observations poisoned")
        );
    }

    #[test]
    fn import_acl_prefix_resolution_can_succeed_before_host_call_policy_denies_callback() {
        let runtime = Runtime::new();
        let called = Arc::new(AtomicU32::new(0));
        runtime
            .new_host_module_builder("wasi_snapshot_preview1")
            .new_function_builder()
            .with_func(
                {
                    let called = called.clone();
                    move |_ctx, _module, _params| {
                        called.fetch_add(1, Ordering::SeqCst);
                        Ok(Vec::new())
                    }
                },
                &[],
                &[],
            )
            .with_name("start")
            .export("start")
            .instantiate(&Context::default())
            .unwrap();

        let import_events = Arc::new(Mutex::new(Vec::new()));
        let ctx = with_host_call_policy(
            &crate::with_import_resolver_observer(
                &crate::experimental::with_import_resolver_acl(
                    &Context::default(),
                    crate::experimental::ImportACL::new().allow_module_prefixes(["wasi_"]),
                ),
                RecordingImportResolverObserver {
                    events: import_events.clone(),
                },
            ),
            deny_start_host_call,
        );
        let compiled_guest = compile_guest_with_start_import(&runtime, "wasi_snapshot_preview1");

        let guest = runtime
            .instantiate_with_context(&ctx, &compiled_guest, ModuleConfig::new())
            .unwrap();
        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();

        assert_eq!("policy denied: host call", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
        assert_eq!(0, called.load(Ordering::SeqCst));
        assert_eq!(
            vec![
                ImportObserverRecord {
                    import_module: "wasi_snapshot_preview1".to_string(),
                    event: crate::ImportResolverEvent::AclAllowed,
                    resolved: false,
                },
                ImportObserverRecord {
                    import_module: "wasi_snapshot_preview1".to_string(),
                    event: crate::ImportResolverEvent::StoreFallback,
                    resolved: false,
                },
            ],
            *import_events
                .lock()
                .expect("import observer events poisoned")
        );
    }

    #[test]
    fn host_call_policy_can_filter_guest_callbacks_by_caller_module_name() {
        for config in policy_runtime_configs() {
            let runtime =
                Runtime::with_config(config.with_host_call_policy(deny_untrusted_caller_module));
            let called = Arc::new(AtomicU32::new(0));
            runtime
                .new_host_module_builder("env")
                .new_function_builder()
                .with_func(
                    {
                        let called = called.clone();
                        move |_ctx, _module, _params| {
                            called.fetch_add(1, Ordering::SeqCst);
                            Ok(vec![7])
                        }
                    },
                    &[ValueType::I32],
                    &[ValueType::I32],
                )
                .with_name("hook_impl")
                .export("hook")
                .instantiate(&Context::default())
                .unwrap();

            let compiled_guest = runtime
                .compile(&[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01,
                    0x7f, 0x01, 0x7f, 0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04, b'h', b'o',
                    b'o', b'k', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r',
                    b'u', b'n', 0x00, 0x01, 0x0a, 0x08, 0x01, 0x06, 0x00, 0x41, 0x2a, 0x10, 0x00,
                    0x0b,
                ])
                .unwrap();
            let trusted = runtime
                .instantiate_with_context(
                    &Context::default(),
                    &compiled_guest,
                    ModuleConfig::new().with_name("trusted"),
                )
                .unwrap();
            let untrusted = runtime
                .instantiate_with_context(
                    &Context::default(),
                    &compiled_guest,
                    ModuleConfig::new().with_name("untrusted"),
                )
                .unwrap();

            assert_eq!(
                vec![7],
                trusted
                    .exported_function("run")
                    .unwrap()
                    .call(&[0])
                    .unwrap()
            );

            let err = untrusted
                .exported_function("run")
                .unwrap()
                .call(&[0])
                .unwrap_err();
            assert_eq!("policy denied: host call", err.to_string());
            assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
            assert_eq!(1, called.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn host_call_policy_tracks_memory_metadata_on_guest_callbacks() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .with_name("write_impl")
            .export("write")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_with_context(
                &Context::default(),
                &runtime
                    .compile(&[
                        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60,
                        0x00, 0x00, 0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b'w', b'r',
                        b'i', b't', b'e', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x05, 0x03, 0x01,
                        0x00, 0x01, 0x07, 0x10, 0x02, 0x03, b'r', b'u', b'n', 0x00, 0x01, 0x06,
                        b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, 0x0a, 0x06, 0x01, 0x04,
                        0x00, 0x10, 0x00, 0x0b,
                    ])
                    .unwrap(),
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let observed = Arc::new(Mutex::new(Vec::new()));
        let ctx = with_host_call_policy(&Context::default(), {
            let observed = observed.clone();
            move |_ctx: &Context, request: &HostCallPolicyRequest| {
                observed
                    .lock()
                    .expect("observed host-call metadata poisoned")
                    .push((
                        request.caller_module_name().map(str::to_string),
                        request.memory().map(|memory| {
                            (
                                memory.minimum_pages(),
                                memory.maximum_pages(),
                                memory.module_name().map(str::to_string),
                                memory.export_names().to_vec(),
                            )
                        }),
                        request
                            .function
                            .clone()
                            .expect("host call policy should receive metadata"),
                    ));
                false
            }
        });
        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();
        assert_eq!("policy denied: host call", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));

        let observed = observed
            .lock()
            .expect("observed host-call metadata poisoned");
        assert_eq!(1, observed.len());
        let (caller_module, memory, definition) = &observed[0];
        assert_eq!(Some("guest".to_string()), *caller_module);
        assert_eq!(Some((1, None, None, vec!["memory".to_string()],)), *memory);
        assert_eq!("write_impl", definition.name());
        assert_eq!(Some("env"), definition.module_name());
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
    fn mutable_exported_globals_round_trip_through_public_api() {
        let runtime = Runtime::new();
        let bytes = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x06, 0x06, 0x01, 0x7e, 0x01, 0x42,
            0x2a, 0x0b, 0x07, 0x0a, 0x01, 0x06, b'g', b'l', b'o', b'b', b'a', b'l', 0x03, 0x00,
        ];
        let compiled = runtime.compile(&bytes).unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let global = module.exported_global("global").unwrap();

        assert!(global.is_mutable());
        assert_eq!(GlobalValue::I64(42), global.get());

        global.set(GlobalValue::I64(2));

        assert_eq!(GlobalValue::I64(2), global.get());
        assert_eq!(GlobalValue::I64(2), module.global(0).get());
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
    fn close_on_context_done_cancels_running_guest_loop() {
        for config in close_on_context_done_runtime_configs() {
            let runtime = Runtime::with_config(config);
            let compiled = runtime.compile(LOOP_EXPORT_WASM).unwrap();
            let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
            let run = module.exported_function("run").unwrap();
            let (ctx, cancel) = Context::default().with_cancel();
            let fallback_module = module.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(500));
                let _ = fallback_module.close_with_exit_code(&Context::default(), 99);
            });

            let handle = thread::spawn(move || run.call_with_context(&ctx, &[]));
            thread::sleep(Duration::from_millis(20));
            cancel.cancel();

            let err = handle
                .join()
                .expect("loop call thread should not panic")
                .unwrap_err();
            assert_eq!(Some(EXIT_CODE_CONTEXT_CANCELED), err.exit_code());
            assert!(module.is_closed());
            assert_eq!(EXIT_CODE_CONTEXT_CANCELED, module.exit_code());
        }
    }

    #[test]
    fn close_on_context_done_honors_deadlines() {
        for config in close_on_context_done_runtime_configs() {
            let runtime = Runtime::with_config(config);
            let compiled = runtime.compile(LOOP_EXPORT_WASM).unwrap();
            let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
            let run = module.exported_function("run").unwrap();
            let ctx = Context::default().with_timeout(Duration::from_millis(20));
            let fallback_module = module.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(500));
                let _ = fallback_module.close_with_exit_code(&Context::default(), 98);
            });

            let err = thread::spawn(move || run.call_with_context(&ctx, &[]))
                .join()
                .expect("loop call thread should not panic")
                .unwrap_err();
            assert_eq!(Some(EXIT_CODE_DEADLINE_EXCEEDED), err.exit_code());
            assert!(module.is_closed());
            assert_eq!(EXIT_CODE_DEADLINE_EXCEEDED, module.exit_code());
        }
    }

    #[test]
    fn close_on_context_done_still_interrupts_on_explicit_module_close() {
        for config in close_on_context_done_runtime_configs() {
            let runtime = Runtime::with_config(config);
            let compiled = runtime.compile(LOOP_EXPORT_WASM).unwrap();
            let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
            let run = module.exported_function("run").unwrap();

            let handle = thread::spawn({
                let ctx = Context::default();
                move || run.call_with_context(&ctx, &[])
            });
            thread::sleep(Duration::from_millis(20));
            module.close_with_exit_code(&Context::default(), 7).unwrap();

            let err = handle
                .join()
                .expect("loop call thread should not panic")
                .unwrap_err();
            assert_eq!(Some(7), err.exit_code());
            assert!(module.is_closed());
            assert_eq!(7, module.exit_code());
        }
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
    fn imported_host_function_oob_writes_are_rejected_in_secure_mode() {
        let guest_bytes = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b'w', b'r', b'i', b't', b'e', 0x00,
            0x00, 0x03, 0x02, 0x01, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x10, 0x02, 0x03,
            b'r', b'u', b'n', 0x00, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
            0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
        ];

        for config in secure_memory_runtime_configs() {
            let runtime = Runtime::with_config(config);
            runtime
                .new_host_module_builder("env")
                .new_function_builder()
                .with_callback(
                    |_ctx, module, _params| {
                        let memory = module.memory().expect("guest memory should be present");
                        let size = memory.size();
                        assert!(memory.write_u32_le(size - 4, 42));
                        assert!(!memory.write_u32_le(size - 3, 99));
                        Ok(Vec::new())
                    },
                    &[],
                    &[],
                )
                .export("write")
                .instantiate(&Context::default())
                .unwrap();

            let guest = runtime
                .instantiate_binary(&guest_bytes, ModuleConfig::new())
                .unwrap();

            guest.exported_function("run").unwrap().call(&[]).unwrap();

            let memory = guest.memory().expect("guest memory should be exported");
            let size = memory.size();
            assert_eq!(Some(42), memory.read_u32_le(size - 4));
            assert_eq!(None, memory.read_u32_le(size - 3));
        }
    }

    #[test]
    fn imported_host_function_oob_reads_return_none_in_secure_mode() {
        if !compiler_supported() {
            return;
        }

        let guest_bytes = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04, b'r', b'e', b'a', b'd', 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x10, 0x02, 0x03, b'r',
            b'u', b'n', 0x00, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, 0x0a,
            0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
        ];

        for config in [RuntimeConfig::new_compiler().with_secure_mode(true)] {
            let runtime = Runtime::with_config(config);
            let observed = Arc::new(Mutex::new(Vec::new()));
            runtime
                .new_host_module_builder("env")
                .new_function_builder()
                .with_callback(
                    {
                        let observed = observed.clone();
                        move |_ctx, module, _params| {
                            let memory = module.memory().expect("guest memory should be present");
                            let size = memory.size();
                            assert!(memory.write_u32_le(size - 4, 0x1122_3344));
                            observed
                                .lock()
                                .expect("observations poisoned")
                                .push((memory.read_u32_le(size - 4), memory.read_u32_le(size - 3)));
                            Ok(Vec::new())
                        }
                    },
                    &[],
                    &[],
                )
                .export("read")
                .instantiate(&Context::default())
                .unwrap();

            let guest = runtime
                .instantiate_binary(&guest_bytes, ModuleConfig::new())
                .unwrap();

            guest.exported_function("run").unwrap().call(&[]).unwrap();
            assert_eq!(
                vec![(Some(0x1122_3344), None)],
                *observed.lock().expect("observations poisoned")
            );
        }
    }

    #[test]
    fn public_memory_generic_read_write_returns_none_oob_in_secure_mode() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler().with_secure_mode(true));
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07,
                0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let memory = module.exported_memory("memory").unwrap();

        let size = memory.size() as usize;
        let valid_data = vec![0x42; 8];

        assert!(memory.write(size - 8, &valid_data));
        assert_eq!(Some(valid_data.clone()), memory.read(size - 8, 8));

        assert_eq!(None, memory.read(size - 4, 8));
        assert!(!memory.write(size - 4, &[0xff; 8]));
    }

    #[test]
    fn public_memory_read_write_u32_le_returns_none_oob_in_secure_mode() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler().with_secure_mode(true));
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07,
                0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let memory = module.exported_memory("memory").unwrap();

        let size = memory.size();
        assert!(memory.write_u32_le(size - 4, 0x1122_3344));
        assert_eq!(Some(0x1122_3344), memory.read_u32_le(size - 4));

        assert_eq!(None, memory.read_u32_le(size - 3));
        assert!(!memory.write_u32_le(size - 3, 0xffff_ffff));
    }

    #[test]
    fn public_memory_read_write_u64_le_returns_none_oob_in_secure_mode() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler().with_secure_mode(true));
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07,
                0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let memory = module.exported_memory("memory").unwrap();

        let size = memory.size();
        assert!(memory.write_u64_le(size - 8, 0x1122_3344_5566_7788));
        assert_eq!(Some(0x1122_3344_5566_7788), memory.read_u64_le(size - 8));

        assert_eq!(None, memory.read_u64_le(size - 7));
        assert!(!memory.write_u64_le(size - 7, 0xffff_ffff_ffff_ffff));
    }

    #[test]
    fn public_memory_read_write_f32_le_returns_none_oob_in_secure_mode() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler().with_secure_mode(true));
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07,
                0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let memory = module.exported_memory("memory").unwrap();

        let size = memory.size();
        assert!(memory.write_f32_le(size - 4, 12.5));
        assert_eq!(Some(12.5), memory.read_f32_le(size - 4));

        assert_eq!(None, memory.read_f32_le(size - 3));
        assert!(!memory.write_f32_le(size - 3, 1.25));
    }

    #[test]
    fn public_memory_read_write_f64_le_returns_none_oob_in_secure_mode() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler().with_secure_mode(true));
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07,
                0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let memory = module.exported_memory("memory").unwrap();

        let size = memory.size();
        assert!(memory.write_f64_le(size - 8, 24.5));
        assert_eq!(Some(24.5), memory.read_f64_le(size - 8));

        assert_eq!(None, memory.read_f64_le(size - 7));
        assert!(!memory.write_f64_le(size - 7, 2.5));
    }

    #[test]
    fn public_memory_pages_returns_size_divided_by_page_size() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler());
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07,
                0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let memory = module.exported_memory("memory").unwrap();

        assert_eq!(1, memory.pages());
        assert_eq!(65_536, memory.size());

        let _ = memory.grow(1);
        assert_eq!(2, memory.pages());
        assert_eq!(131_072, memory.size());
    }

    #[test]
    fn public_memory_grow_returns_previous_page_count() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler());
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07,
                0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let memory = module.exported_memory("memory").unwrap();

        assert_eq!(Some(1), memory.grow(1));
        assert_eq!(Some(2), memory.grow(1));
        assert_eq!(Some(3), memory.grow(2));
    }

    #[test]
    fn public_memory_grow_exceeding_maximum_pages_returns_none() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler());
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x04, 0x01, 0x01, 0x01, 0x02,
                0x07, 0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
            ])
            .unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let memory = module.exported_memory("memory").unwrap();

        assert_eq!(Some(1), memory.grow(1));
        assert_eq!(None, memory.grow(1));
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
    fn yield_policy_denies_cooperative_suspension_across_runtimes() {
        for config in policy_runtime_configs() {
            let runtime = Runtime::with_config(config.with_yield_policy(deny_all_yields));
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
            assert_eq!("policy denied: cooperative yield", err.to_string());
            assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
        }
    }

    #[test]
    fn yield_policy_denies_follow_on_suspension_during_resume() {
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

        let resume_ctx = with_yield_policy(&with_yielder(&Context::default()), deny_all_yields);
        let err = first_yield
            .resumer()
            .expect("resumer should be present")
            .resume(&resume_ctx, &[40])
            .unwrap_err();
        assert_eq!("policy denied: cooperative yield", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
    }

    #[test]
    fn yield_policy_denies_direct_host_function_suspension_with_function_metadata() {
        let runtime = Runtime::new();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let module = runtime
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
            .with_name("async_work_impl")
            .export("async_work")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = with_yield_policy(
            &with_yielder(&Context::default()),
            RecordingDenyYieldPolicy {
                observed: observed.clone(),
            },
        );
        let err = module
            .exported_function("async_work")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();
        assert_eq!("policy denied: cooperative yield", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));

        let observed = observed.lock().expect("observed yield metadata poisoned");
        assert_eq!(1, observed.len());
        let definition = &observed[0];
        assert_eq!(Some("example"), definition.module_name());
        assert_eq!("async_work_impl", definition.name());
        assert_eq!(&["async_work".to_string()], definition.export_names());
    }

    #[test]
    fn yield_policy_observer_fires_before_direct_host_yield_denial_trap() {
        let runtime = Runtime::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let module = runtime
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
            .with_name("async_work_impl")
            .export("async_work")
            .instantiate(&Context::default())
            .unwrap();

        let policy_ctx = with_yield_policy(&with_yielder(&Context::default()), deny_all_yields);
        let observer_ctx = with_yield_policy_observer(&policy_ctx, {
            let events = events.clone();
            move |_ctx: &Context, observation: YieldPolicyObservation| {
                events
                    .lock()
                    .expect("yield observer events poisoned")
                    .push((
                        "policy".to_string(),
                        observation.module.name().map(str::to_string),
                        observation.request.name().map(str::to_string),
                        observation.request.caller_module_name().map(str::to_string),
                        observation.decision,
                    ));
            }
        });
        let ctx = with_trap_observer(&observer_ctx, {
            let events = events.clone();
            move |_ctx: &Context, observation: TrapObservation| {
                events
                    .lock()
                    .expect("yield observer events poisoned")
                    .push((
                        "trap".to_string(),
                        observation.module.name().map(str::to_string),
                        None,
                        None,
                        YieldPolicyDecision::Denied,
                    ));
            }
        });

        let err = module
            .exported_function("async_work")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();
        assert_eq!("policy denied: cooperative yield", err.to_string());
        assert_eq!(
            vec![
                (
                    "policy".to_string(),
                    Some("example".to_string()),
                    Some("async_work_impl".to_string()),
                    Some("example".to_string()),
                    YieldPolicyDecision::Denied,
                ),
                (
                    "trap".to_string(),
                    Some("example".to_string()),
                    None,
                    None,
                    YieldPolicyDecision::Denied,
                ),
            ],
            *events.lock().expect("yield observer events poisoned")
        );
    }

    #[test]
    fn yield_policy_observer_fires_before_yield_observer_on_allowed_direct_host_yield() {
        let runtime = Runtime::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let module = runtime
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
            .with_name("async_work_impl")
            .export("async_work")
            .instantiate(&Context::default())
            .unwrap();

        let policy_ctx = with_yield_policy(&with_yielder(&Context::default()), allow_all_yields);
        let observer_ctx = with_yield_policy_observer(&policy_ctx, {
            let events = events.clone();
            move |_ctx: &Context, observation: YieldPolicyObservation| {
                events
                    .lock()
                    .expect("yield observer events poisoned")
                    .push((
                        "policy".to_string(),
                        observation.module.name().map(str::to_string),
                        observation.request.name().map(str::to_string),
                        observation.request.caller_module_name().map(str::to_string),
                        Some(observation.decision),
                        None,
                    ));
            }
        });
        let ctx = with_yield_observer(&observer_ctx, {
            let events = events.clone();
            move |_ctx: &Context, observation: YieldObservation| {
                events
                    .lock()
                    .expect("yield observer events poisoned")
                    .push((
                        "yield".to_string(),
                        observation.module.name().map(str::to_string),
                        None,
                        None,
                        None,
                        Some(observation.event),
                    ));
            }
        });

        let err = module
            .exported_function("async_work")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();
        let RuntimeError::Yield(yield_error) = err else {
            panic!("expected yield error");
        };
        yield_error
            .resumer()
            .expect("resumer should be present")
            .cancel();

        assert_eq!(
            vec![
                (
                    "policy".to_string(),
                    Some("example".to_string()),
                    Some("async_work_impl".to_string()),
                    Some("example".to_string()),
                    Some(YieldPolicyDecision::Allowed),
                    None,
                ),
                (
                    "yield".to_string(),
                    Some("example".to_string()),
                    None,
                    None,
                    None,
                    Some(YieldEvent::Yielded),
                ),
            ],
            *events.lock().expect("yield observer events poisoned")
        );
    }

    #[test]
    fn yield_resume_validation_error_does_not_emit_resumed_event() {
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

        let observations = Arc::new(Mutex::new(Vec::new()));
        let initial_ctx = with_yield_observer(
            &with_yielder(&Context::default()),
            {
                let observations = observations.clone();
                move |_ctx: &Context, observation: YieldObservation| {
                    observations
                        .lock()
                        .expect("yield observations poisoned")
                        .push((
                            observation.event,
                            observation.yield_count,
                            observation.expected_host_results,
                        ));
                }
            },
        );

        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&initial_ctx, &[])
            .unwrap_err();
        let RuntimeError::Yield(yield_error) = err else {
            panic!("expected yield error");
        };
        let resumer = yield_error.resumer().expect("resumer should be present");

        let err = resumer
            .resume(&with_yielder(&Context::default()), &[])
            .unwrap_err();
        assert_eq!(
            "cannot resume: expected 1 host results, but got 0",
            err.to_string()
        );
        assert_eq!(
            vec![(YieldEvent::Yielded, 1, 1)],
            *observations.lock().expect("yield observations poisoned")
        );
        assert_eq!(
            vec![142],
            resumer
                .resume(&with_yielder(&Context::default()), &[42])
                .unwrap()
        );
        assert_eq!(
            vec![(YieldEvent::Yielded, 1, 1)],
            *observations.lock().expect("yield observations poisoned")
        );
    }

    #[test]
    fn yield_policy_context_overrides_runtime_config_policy() {
        let runtime = Runtime::with_config(RuntimeConfig::new().with_yield_policy(deny_all_yields));
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

        let ctx = with_yield_policy(&with_yielder(&Context::default()), allow_all_yields);
        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();
        let RuntimeError::Yield(yield_error) = err else {
            panic!("expected yield error");
        };
        yield_error
            .resumer()
            .expect("resumer should be present")
            .cancel();
    }

    #[test]
    fn yield_policy_tracks_caller_module_name_on_direct_host_yield() {
        let runtime = Runtime::new();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let module = runtime
            .new_host_module_builder("guest_wrapper")
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
            .with_name("async_work_impl")
            .export("async_work")
            .instantiate(&Context::default())
            .unwrap();

        let ctx = with_yield_policy(
            &with_yielder(&Context::default()),
            RecordingYieldPolicyWithCaller {
                observed: observed.clone(),
            },
        );
        let err = module
            .exported_function("async_work")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();
        assert_eq!("policy denied: cooperative yield", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));

        let observed = observed.lock().expect("observed yield metadata poisoned");
        assert_eq!(1, observed.len());
        let (caller_module, definition) = &observed[0];
        assert_eq!(Some("guest_wrapper".to_string()), *caller_module);
        assert_eq!("async_work_impl", definition.name());
        assert_eq!(Some("guest_wrapper"), definition.module_name());
    }

    #[test]
    fn yield_policy_allows_missing_caller_module_name_for_unnamed_guest() {
        let runtime = Runtime::new();
        let observed = Arc::new(Mutex::new(Vec::new()));
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                |ctx, _module, _params| {
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(Vec::new())
                },
                &[],
                &[],
            )
            .with_name("yield_impl")
            .export("yield")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00,
                    0x00, 0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b'y', b'i', b'e', b'l',
                    b'd', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u',
                    b'n', 0x00, 0x01, 0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
                ],
                ModuleConfig::new(),
            )
            .unwrap();

        let ctx = with_yield_policy(
            &with_yielder(&Context::default()),
            RecordingYieldPolicyWithCaller {
                observed: observed.clone(),
            },
        );
        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();
        assert_eq!("policy denied: cooperative yield", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));

        let observed = observed.lock().expect("observed yield metadata poisoned");
        assert_eq!(1, observed.len());
        let (caller_module, definition) = &observed[0];
        assert_eq!(None, *caller_module);
        assert_eq!("run", definition.name());
        assert_eq!(None, definition.module_name());
    }

    #[test]
    fn yield_policy_tracks_memory_metadata_on_guest_callbacks() {
        let runtime = Runtime::new();
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                |ctx, _module, _params| {
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(Vec::new())
                },
                &[],
                &[],
            )
            .with_name("yield_impl")
            .export("yield")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_with_context(
                &Context::default(),
                &runtime
                    .compile(&[
                        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60,
                        0x00, 0x00, 0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b'y', b'i',
                        b'e', b'l', b'd', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x05, 0x03, 0x01,
                        0x00, 0x01, 0x07, 0x10, 0x02, 0x03, b'r', b'u', b'n', 0x00, 0x01, 0x06,
                        b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, 0x0a, 0x06, 0x01, 0x04,
                        0x00, 0x10, 0x00, 0x0b,
                    ])
                    .unwrap(),
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let observed = Arc::new(Mutex::new(Vec::new()));
        let ctx = with_yield_policy(&with_yielder(&Context::default()), {
            let observed = observed.clone();
            move |_ctx: &Context, request: &YieldPolicyRequest| {
                observed
                    .lock()
                    .expect("observed yield metadata poisoned")
                    .push((
                        request.caller_module_name().map(str::to_string),
                        request.memory().map(|memory| {
                            (
                                memory.minimum_pages(),
                                memory.maximum_pages(),
                                memory.module_name().map(str::to_string),
                                memory.export_names().to_vec(),
                            )
                        }),
                        request
                            .function
                            .clone()
                            .expect("yield policy should receive metadata"),
                    ));
                false
            }
        });
        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();
        assert_eq!("policy denied: cooperative yield", err.to_string());
        assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));

        let observed = observed.lock().expect("observed yield metadata poisoned");
        assert_eq!(1, observed.len());
        let (caller_module, memory, definition) = &observed[0];
        assert_eq!(Some("guest".to_string()), *caller_module);
        assert_eq!(Some((1, None, None, vec!["memory".to_string()],)), *memory);
        assert_eq!("run", definition.name());
        assert_eq!(&["run".to_string()], definition.export_names());
    }

    #[test]
    fn yield_policy_observer_records_allowed_guest_callbacks() {
        let runtime = Runtime::new();
        let observations = Arc::new(Mutex::new(Vec::new()));
        runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                |ctx, _module, _params| {
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(Vec::new())
                },
                &[],
                &[],
            )
            .with_name("yield_impl")
            .export("yield")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00,
                    0x00, 0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b'y', b'i', b'e', b'l',
                    b'd', 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u',
                    b'n', 0x00, 0x01, 0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b,
                ],
                ModuleConfig::new().with_name("guest"),
            )
            .unwrap();

        let policy_ctx = with_yield_policy(&with_yielder(&Context::default()), allow_all_yields);
        let ctx = with_yield_policy_observer(&policy_ctx, {
            let observations = observations.clone();
            move |_ctx: &Context, observation: YieldPolicyObservation| {
                observations
                    .lock()
                    .expect("yield observer observations poisoned")
                    .push((
                        observation.module.name().map(str::to_string),
                        observation.request.name().map(str::to_string),
                        observation.request.caller_module_name().map(str::to_string),
                        observation.decision,
                    ));
            }
        });

        let err = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap_err();
        let RuntimeError::Yield(yield_error) = err else {
            panic!("expected yield error");
        };
        yield_error
            .resumer()
            .expect("resumer should be present")
            .cancel();
        assert_eq!(
            vec![(
                Some("guest".to_string()),
                Some("run".to_string()),
                Some("guest".to_string()),
                YieldPolicyDecision::Allowed,
            )],
            *observations
                .lock()
                .expect("yield observer observations poisoned")
        );
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

    #[test]
    fn compile_with_context_eagerly_compiles_with_requested_workers() {
        if !compiler_supported() {
            return;
        }

        let runtime = Runtime::with_config(RuntimeConfig::new_compiler());
        let ctx = with_compilation_workers(&Context::default(), 4);
        let compiled = runtime
            .compile_with_context(&ctx, SIMPLE_EXPORT_WASM)
            .unwrap();

        let compiled_count = runtime
            .inner
            .store
            .lock()
            .expect("runtime store poisoned")
            .engine
            .compiled_module_count();
        assert_eq!(1, compiled_count);

        let module = runtime
            .instantiate_with_context(&ctx, &compiled, ModuleConfig::new())
            .unwrap();
        assert!(module.exported_function("f").is_some());

        let compiled_count = runtime
            .inner
            .store
            .lock()
            .expect("runtime store poisoned")
            .engine
            .compiled_module_count();
        assert_eq!(1, compiled_count);
    }

    // ---------------------------------------------------------------
    // Runtime-level fuel exhaustion E2E tests
    // ---------------------------------------------------------------

    /// (module (func (export "spin") (loop (br 0))))
    const INFINITE_LOOP_WASM: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03,
        0x02, 0x01, 0x00, 0x07, 0x08, 0x01, 0x04, 0x73, 0x70, 0x69, 0x6e, 0x00, 0x00, 0x0a, 0x09,
        0x01, 0x07, 0x00, 0x03, 0x40, 0x0c, 0x00, 0x0b, 0x0b,
    ];

    #[test]
    fn fuel_exhaustion_stops_infinite_loop_interpreter() {
        let runtime = Runtime::with_config(RuntimeConfig::new_interpreter().with_fuel(10));
        let compiled = runtime.compile(INFINITE_LOOP_WASM).unwrap();
        let module = runtime
            .instantiate(&compiled, ModuleConfig::new().with_name("spin"))
            .unwrap();
        let func = module.exported_function("spin").unwrap();

        let consumed = Arc::new(AtomicI64::new(0));
        let ctx = with_fuel_controller(
            &Context::default(),
            TestFuelController {
                budget: 10,
                consumed: consumed.clone(),
            },
        );

        let err = func.call_with_context(&ctx, &[]).unwrap_err();
        assert!(
            err.to_string().contains("fuel exhausted"),
            "expected 'fuel exhausted', got: {}",
            err
        );
        // The controller's consumed() callback should have been called.
        assert!(consumed.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn fuel_exhaustion_stops_infinite_loop_compiler() {
        if !compiler_supported() || !supports_guard_pages() {
            return;
        }
        let runtime = Runtime::with_config(
            RuntimeConfig::new_compiler()
                .with_fuel(10)
                .with_secure_mode(true),
        );
        let compiled = runtime.compile(INFINITE_LOOP_WASM).unwrap();
        let module = runtime
            .instantiate(&compiled, ModuleConfig::new().with_name("spin"))
            .unwrap();
        let func = module.exported_function("spin").unwrap();

        let consumed = Arc::new(AtomicI64::new(0));
        let ctx = with_fuel_controller(
            &Context::default(),
            TestFuelController {
                budget: 10,
                consumed: consumed.clone(),
            },
        );

        let err = func.call_with_context(&ctx, &[]).unwrap_err();
        assert_eq!("fuel exhausted", err.to_string());
        assert_eq!(Some(TrapCause::FuelExhausted), trap_cause_of(&err));
    }

    #[test]
    fn fuel_not_configured_allows_finite_function_to_complete() {
        // With no fuel setting (fuel=0, disabled), a normal function should run fine.
        let runtime = Runtime::new();
        let compiled = runtime.compile(SIMPLE_EXPORT_WASM).unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let func = module.exported_function("f").unwrap();

        let results = func.call(&[]).unwrap();
        assert_eq!(vec![42], results);
    }
}
