use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use crate::{
    api::{
        features::CoreFeatures,
        wasm::{CustomSection, FunctionDefinition, Global, HostCallback, MemoryDefinition},
    },
    cache::CompilationCache,
    experimental::{
        host_call_policy::{HostCallPolicy, IntoHostCallPolicy},
        yield_policy::{IntoYieldPolicy, YieldPolicy},
    },
};
use razero_wasm::module::Module as WasmModule;

pub const MEMORY_LIMIT_PAGES: u32 = 65_536;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RuntimeEngineKind {
    Auto,
    Compiler,
    #[default]
    Interpreter,
}

#[derive(Clone, Default)]
pub struct RuntimeConfig {
    core_features: CoreFeatures,
    memory_limit_pages: u32,
    memory_capacity_from_max: bool,
    debug_info_enabled: bool,
    compilation_cache: Option<Arc<dyn CompilationCache>>,
    custom_sections: bool,
    close_on_context_done: bool,
    host_call_policy: Option<Arc<dyn HostCallPolicy>>,
    secure_mode: bool,
    fuel: i64,
    engine_kind: RuntimeEngineKind,
    yield_policy: Option<Arc<dyn YieldPolicy>>,
}

impl RuntimeConfig {
    pub fn new() -> Self {
        Self {
            core_features: CoreFeatures::V2,
            memory_limit_pages: MEMORY_LIMIT_PAGES,
            debug_info_enabled: true,
            ..Self::default()
        }
    }

    pub fn new_compiler() -> Self {
        Self {
            engine_kind: RuntimeEngineKind::Compiler,
            ..Self::new()
        }
    }

    pub fn new_auto() -> Self {
        Self {
            engine_kind: RuntimeEngineKind::Auto,
            ..Self::new()
        }
    }

    pub fn new_interpreter() -> Self {
        Self {
            engine_kind: RuntimeEngineKind::Interpreter,
            ..Self::new()
        }
    }

    pub fn with_core_features(mut self, features: CoreFeatures) -> Self {
        self.core_features = features;
        self
    }

    pub fn with_memory_limit_pages(mut self, memory_limit_pages: u32) -> Self {
        assert!(
            memory_limit_pages <= MEMORY_LIMIT_PAGES,
            "memory_limit_pages invalid: {memory_limit_pages} > {MEMORY_LIMIT_PAGES}"
        );
        self.memory_limit_pages = memory_limit_pages;
        self
    }

    pub fn with_memory_capacity_from_max(mut self, enabled: bool) -> Self {
        self.memory_capacity_from_max = enabled;
        self
    }

    pub fn with_debug_info_enabled(mut self, enabled: bool) -> Self {
        self.debug_info_enabled = enabled;
        self
    }

    pub fn with_compilation_cache(mut self, cache: Arc<dyn CompilationCache>) -> Self {
        self.compilation_cache = Some(cache);
        self
    }

    pub fn with_custom_sections(mut self, enabled: bool) -> Self {
        self.custom_sections = enabled;
        self
    }

    pub fn with_close_on_context_done(mut self, enabled: bool) -> Self {
        self.close_on_context_done = enabled;
        self
    }

    pub fn with_secure_mode(mut self, enabled: bool) -> Self {
        self.secure_mode = enabled;
        self
    }

    pub fn with_host_call_policy(mut self, policy: impl IntoHostCallPolicy) -> Self {
        if let Some(policy) = policy.into_host_call_policy() {
            self.host_call_policy = Some(policy);
        }
        self
    }

    pub fn with_fuel(mut self, fuel: i64) -> Self {
        self.fuel = fuel.max(0);
        self
    }

    pub fn with_yield_policy(mut self, policy: impl IntoYieldPolicy) -> Self {
        if let Some(policy) = policy.into_yield_policy() {
            self.yield_policy = Some(policy);
        }
        self
    }

    pub fn core_features(&self) -> CoreFeatures {
        self.core_features
    }

    pub fn memory_limit_pages(&self) -> u32 {
        self.memory_limit_pages
    }

    pub fn memory_capacity_from_max(&self) -> bool {
        self.memory_capacity_from_max
    }

    pub fn debug_info_enabled(&self) -> bool {
        self.debug_info_enabled
    }

    pub fn compilation_cache(&self) -> Option<Arc<dyn CompilationCache>> {
        self.compilation_cache.clone()
    }

    pub fn custom_sections(&self) -> bool {
        self.custom_sections
    }

    pub fn close_on_context_done(&self) -> bool {
        self.close_on_context_done
    }

    pub fn secure_mode(&self) -> bool {
        self.secure_mode
    }

    pub fn host_call_policy(&self) -> Option<Arc<dyn HostCallPolicy>> {
        self.host_call_policy.clone()
    }

    pub fn fuel(&self) -> i64 {
        self.fuel
    }

    pub fn engine_kind(&self) -> RuntimeEngineKind {
        self.engine_kind
    }

    pub fn yield_policy(&self) -> Option<Arc<dyn YieldPolicy>> {
        self.yield_policy.clone()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModuleConfig {
    name: Option<String>,
    name_set: bool,
}

impl ModuleConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self.name_set = true;
        self
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub(crate) fn name_set(&self) -> bool {
        self.name_set
    }
}

#[derive(Clone)]
pub struct CompiledModule {
    inner: Arc<CompiledModuleInner>,
}

pub(crate) struct CompiledModuleInner {
    pub(crate) name: Option<String>,
    pub(crate) bytes: Vec<u8>,
    pub(crate) precompiled_bytes: Option<Vec<u8>>,
    pub(crate) imported_functions: Vec<FunctionDefinition>,
    pub(crate) exported_functions: BTreeMap<String, FunctionDefinition>,
    pub(crate) imported_memories: Vec<MemoryDefinition>,
    pub(crate) exported_memories: BTreeMap<String, MemoryDefinition>,
    pub(crate) exported_globals: BTreeMap<String, Global>,
    pub(crate) custom_sections: Vec<CustomSection>,
    pub(crate) host_callbacks: BTreeMap<String, HostCallback>,
    pub(crate) lower_module: Option<WasmModule>,
    pub(crate) closed: AtomicBool,
}

impl CompiledModule {
    pub(crate) fn new(inner: CompiledModuleInner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    pub(crate) fn inner(&self) -> &CompiledModuleInner {
        &self.inner
    }

    pub fn name(&self) -> Option<&str> {
        self.inner.name.as_deref()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.inner.bytes
    }

    pub fn imported_functions(&self) -> &[FunctionDefinition] {
        &self.inner.imported_functions
    }

    pub fn exported_functions(&self) -> &BTreeMap<String, FunctionDefinition> {
        &self.inner.exported_functions
    }

    pub fn imported_memories(&self) -> &[MemoryDefinition] {
        &self.inner.imported_memories
    }

    pub fn exported_memories(&self) -> &BTreeMap<String, MemoryDefinition> {
        &self.inner.exported_memories
    }

    pub fn custom_sections(&self) -> &[CustomSection] {
        &self.inner.custom_sections
    }

    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::{RuntimeConfig, RuntimeEngineKind};
    use crate::{
        experimental::{HostCallPolicyRequest, YieldPolicyRequest},
        Context,
    };

    fn allow_host_calls(_ctx: &Context, _request: &HostCallPolicyRequest) -> bool {
        true
    }

    fn allow_yields(_ctx: &Context, _request: &YieldPolicyRequest) -> bool {
        true
    }

    #[test]
    fn runtime_config_engine_constructors_select_expected_kind() {
        assert_eq!(
            RuntimeEngineKind::Interpreter,
            RuntimeConfig::new().engine_kind()
        );
        assert_eq!(
            RuntimeEngineKind::Compiler,
            RuntimeConfig::new_compiler().engine_kind()
        );
        assert_eq!(
            RuntimeEngineKind::Auto,
            RuntimeConfig::new_auto().engine_kind()
        );
        assert_eq!(
            RuntimeEngineKind::Interpreter,
            RuntimeConfig::new_interpreter().engine_kind()
        );
    }

    #[test]
    fn runtime_config_host_call_policy_round_trips() {
        let config = RuntimeConfig::new().with_host_call_policy(allow_host_calls);
        let policy = config
            .host_call_policy()
            .expect("host call policy should be present");

        assert!(policy.allow_host_call(&Context::default(), &HostCallPolicyRequest::new()));
    }

    #[test]
    fn runtime_config_yield_policy_round_trips() {
        let config = RuntimeConfig::new().with_yield_policy(allow_yields);
        let policy = config
            .yield_policy()
            .expect("yield policy should be present");

        assert!(policy.allow_yield(&Context::default(), &YieldPolicyRequest::new()));
    }
}
