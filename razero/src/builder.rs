use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use crate::{
    api::{
        error::Result,
        wasm::{FunctionDefinition, HostCallback, Module, ValueType},
    },
    config::{CompiledModule, CompiledModuleInner, ModuleConfig},
    ctx_keys::Context,
    runtime::Runtime,
};

#[derive(Clone)]
pub struct HostFunction {
    definition: FunctionDefinition,
    callback: HostCallback,
}

impl HostFunction {
    pub fn new(definition: FunctionDefinition, callback: HostCallback) -> Self {
        Self {
            definition,
            callback,
        }
    }

    pub fn definition(&self) -> &FunctionDefinition {
        &self.definition
    }
}

#[derive(Clone)]
pub struct HostModuleBuilder {
    inner: Arc<Mutex<HostModuleBuilderInner>>,
}

struct HostModuleBuilderInner {
    runtime: Runtime,
    module_name: String,
    export_order: Vec<String>,
    functions: BTreeMap<String, HostFunction>,
}

impl HostModuleBuilder {
    pub(crate) fn attached(runtime: Runtime, module_name: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HostModuleBuilderInner {
                runtime,
                module_name: module_name.into(),
                export_order: Vec::new(),
                functions: BTreeMap::new(),
            })),
        }
    }

    pub fn new(module_name: impl Into<String>) -> Self {
        Self::attached(Runtime::new(), module_name)
    }

    pub fn module_name(&self) -> String {
        self.inner
            .lock()
            .expect("host module builder poisoned")
            .module_name
            .clone()
    }

    pub fn new_function_builder(&self) -> HostFunctionBuilder {
        HostFunctionBuilder::new(self.clone())
    }

    pub fn compile(&self, _ctx: &Context) -> Result<CompiledModule> {
        let inner = self.inner.lock().expect("host module builder poisoned");
        let mut exported_functions = BTreeMap::new();
        let mut callbacks = BTreeMap::new();
        for export_name in &inner.export_order {
            let host_function = inner
                .functions
                .get(export_name)
                .expect("host function missing from builder");
            exported_functions.insert(export_name.clone(), host_function.definition.clone());
            callbacks.insert(export_name.clone(), host_function.callback.clone());
        }
        Ok(CompiledModule::new(CompiledModuleInner {
            name: Some(inner.module_name.clone()),
            bytes: Vec::new(),
            imported_functions: Vec::new(),
            exported_functions,
            imported_memories: Vec::new(),
            exported_memories: BTreeMap::new(),
            exported_globals: BTreeMap::new(),
            custom_sections: Vec::new(),
            host_callbacks: callbacks,
            closed: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    pub fn instantiate(&self, ctx: &Context) -> Result<Module> {
        let runtime = self
            .inner
            .lock()
            .expect("host module builder poisoned")
            .runtime
            .clone();
        let compiled = self.compile(ctx)?;
        runtime.instantiate_with_context(ctx, &compiled, ModuleConfig::new())
    }

    fn export_host_function(&self, export_name: String, function: HostFunction) {
        let mut inner = self.inner.lock().expect("host module builder poisoned");
        if !inner.functions.contains_key(&export_name) {
            inner.export_order.push(export_name.clone());
        }
        inner.functions.insert(export_name, function);
    }
}

pub struct HostFunctionBuilder {
    builder: HostModuleBuilder,
    callback: Option<HostCallback>,
    name: Option<String>,
    param_types: Vec<ValueType>,
    result_types: Vec<ValueType>,
    param_names: Vec<String>,
    result_names: Vec<String>,
}

impl HostFunctionBuilder {
    fn new(builder: HostModuleBuilder) -> Self {
        Self {
            builder,
            callback: None,
            name: None,
            param_types: Vec::new(),
            result_types: Vec::new(),
            param_names: Vec::new(),
            result_names: Vec::new(),
        }
    }

    pub fn with_callback<F>(mut self, callback: F, params: &[ValueType], results: &[ValueType]) -> Self
    where
        F: Fn(Context, Module, &[u64]) -> Result<Vec<u64>> + Send + Sync + 'static,
    {
        self.callback = Some(Arc::new(callback));
        self.param_types = params.to_vec();
        self.result_types = results.to_vec();
        self
    }

    pub fn with_func<F>(self, callback: F, params: &[ValueType], results: &[ValueType]) -> Self
    where
        F: Fn(Context, Module, &[u64]) -> Result<Vec<u64>> + Send + Sync + 'static,
    {
        self.with_callback(callback, params, results)
    }

    pub fn with_stack_callback<F>(
        mut self,
        callback: F,
        params: &[ValueType],
        results: &[ValueType],
    ) -> Self
    where
        F: Fn(Context, Module, &mut [u64]) -> Result<()> + Send + Sync + 'static,
    {
        let result_len = results.len();
        self.callback = Some(Arc::new(move |ctx, module, params| {
            let mut stack = params.to_vec();
            let slots = stack.len().max(result_len);
            stack.resize(slots, 0);
            callback(ctx, module, &mut stack)?;
            Ok(stack.into_iter().take(result_len).collect())
        }));
        self.param_types = params.to_vec();
        self.result_types = results.to_vec();
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_parameter_names(mut self, names: &[&str]) -> Self {
        self.param_names = names.iter().map(|name| (*name).to_string()).collect();
        self
    }

    pub fn with_result_names(mut self, names: &[&str]) -> Self {
        self.result_names = names.iter().map(|name| (*name).to_string()).collect();
        self
    }

    pub fn export(self, export_name: impl Into<String>) -> HostModuleBuilder {
        let export_name = export_name.into();
        let function_name = self.name.clone().unwrap_or_else(|| export_name.clone());
        let definition = FunctionDefinition::new(function_name)
            .with_module_name(Some(self.builder.module_name()))
            .with_export_name(export_name.clone())
            .with_signature(self.param_types, self.result_types)
            .with_parameter_names(self.param_names)
            .with_result_names(self.result_names);
        let host_function = HostFunction::new(
            definition,
            self.callback
                .expect("host function builder requires a callback before export"),
        );
        self.builder.export_host_function(export_name, host_function);
        self.builder
    }
}
