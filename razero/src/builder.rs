use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use razero_wasm::{
    host::{new_host_module, HostFunc as WasmHostFunc},
    module::ValueType as WasmValueType,
};

use crate::{
    api::{
        error::Result,
        features::CoreFeatures,
        wasm::{FunctionDefinition, HostCallback, Module, ValueType},
    },
    config::{CompiledModule, CompiledModuleInner, ModuleConfig},
    ctx_keys::Context,
    runtime::{lower_host_function_callback, Runtime},
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
        let mut lower_functions = BTreeMap::new();
        for export_name in &inner.export_order {
            let host_function = inner
                .functions
                .get(export_name)
                .expect("host function missing from builder");
            exported_functions.insert(export_name.clone(), host_function.definition.clone());
            callbacks.insert(export_name.clone(), host_function.callback.clone());
            lower_functions.insert(
                export_name.clone(),
                WasmHostFunc {
                    export_name: export_name.clone(),
                    name: host_function.definition.name().to_string(),
                    param_types: host_function
                        .definition
                        .param_types()
                        .iter()
                        .copied()
                        .map(to_wasm_value_type)
                        .collect(),
                    param_names: host_function.definition.param_names().to_vec(),
                    result_types: host_function
                        .definition
                        .result_types()
                        .iter()
                        .copied()
                        .map(to_wasm_value_type)
                        .collect(),
                    result_names: host_function.definition.result_names().to_vec(),
                    code: razero_wasm::module::Code {
                        body_kind: razero_wasm::module::CodeBody::Host,
                        host_func: Some(lower_host_function_callback(
                            host_function.callback.clone(),
                            host_function.definition.param_types().len(),
                            host_function.definition.result_types().len(),
                        )),
                        ..razero_wasm::module::Code::default()
                    },
                },
            );
        }
        let lower_module = new_host_module(
            inner.module_name.clone(),
            &inner.export_order,
            &lower_functions,
            inner
                .runtime
                .config()
                .core_features()
                .contains(CoreFeatures::MULTI_VALUE),
        )
        .map_err(|err| crate::RuntimeError::new(err.to_string()))?;
        Ok(CompiledModule::new(CompiledModuleInner {
            name: Some(inner.module_name.clone()),
            bytes: Vec::new(),
            precompiled_bytes: None,
            imported_functions: Vec::new(),
            exported_functions,
            imported_memories: Vec::new(),
            exported_memories: BTreeMap::new(),
            exported_globals: BTreeMap::new(),
            custom_sections: Vec::new(),
            host_callbacks: callbacks,
            lower_module: Some(lower_module),
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

fn to_wasm_value_type(value_type: ValueType) -> WasmValueType {
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

    pub fn with_callback<F>(
        mut self,
        callback: F,
        params: &[ValueType],
        results: &[ValueType],
    ) -> Self
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
        self.builder
            .export_host_function(export_name, host_function);
        self.builder
    }
}

#[cfg(test)]
mod tests {
    use super::HostModuleBuilder;
    use crate::{api::wasm::ValueType, ctx_keys::Context};

    #[test]
    fn compile_preserves_function_metadata() {
        let compiled = HostModuleBuilder::new("host")
            .new_function_builder()
            .with_func(
                |_ctx, _module, params| Ok(vec![params[0]]),
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("get")
            .with_parameter_names(&["x"])
            .with_result_names(&["y"])
            .export("run")
            .compile(&Context::default())
            .unwrap();

        let definition = compiled
            .exported_functions()
            .get("run")
            .expect("run export should be compiled");
        assert_eq!(Some("host"), definition.module_name());
        assert_eq!("get", definition.name());
        assert_eq!(&[ValueType::I32], definition.param_types());
        assert_eq!(&[ValueType::I32], definition.result_types());
        assert_eq!(&["run".to_string()], definition.export_names());
        assert_eq!(&["x".to_string()], definition.param_names());
        assert_eq!(&["y".to_string()], definition.result_names());
    }

    #[test]
    fn reexport_overwrites_existing_function_definition() {
        let compiled = HostModuleBuilder::new("host")
            .new_function_builder()
            .with_func(
                |_ctx, _module, _params| Ok(vec![0]),
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .export("same")
            .new_function_builder()
            .with_func(
                |_ctx, _module, _params| Ok(vec![0]),
                &[ValueType::I64],
                &[ValueType::I32],
            )
            .export("same")
            .compile(&Context::default())
            .unwrap();

        assert_eq!(1, compiled.exported_functions().len());
        let definition = compiled
            .exported_functions()
            .get("same")
            .expect("same export should exist");
        assert_eq!(&[ValueType::I64], definition.param_types());
        assert_eq!(&[ValueType::I32], definition.result_types());
        assert_eq!("same", definition.name());
    }
}
