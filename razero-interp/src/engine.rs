#![doc = "Interpreter engine glue for razero-wasm stores."]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};

use razero_wasm::engine::{
    Engine as WasmEngine, EngineError, FunctionHandle, FunctionTypeId,
    ModuleEngine as WasmModuleEngine,
};
use razero_wasm::host_func::{Caller, HostFuncRef as WasmHostFuncRef};
use razero_wasm::module::{
    FunctionType as WasmFunctionType, GlobalType as WasmGlobalType, ImportDesc, Index, Module,
    ModuleId, ValueType as WasmValueType,
};
use razero_wasm::module_instance::ModuleInstance;
use razero_wasm::table::{Reference, TableInstance};

use crate::compiler::{
    CompileConfig, Compiler, FunctionType as InterpFunctionType, GlobalType as InterpGlobalType,
    ValueType as InterpValueType,
};
use crate::interpreter::{
    host_function, Function, HostFuncRef, Interpreter, Memory, Module as RuntimeModule,
    RuntimeResult, Table, Trap,
};
use crate::signature::Signature;

#[derive(Clone, Debug)]
pub struct CompiledModule {
    source: Module,
    types: Vec<Signature>,
    local_functions: Vec<Function>,
}

#[derive(Clone, Debug)]
struct CompiledModuleWithCount {
    compiled_module: Arc<CompiledModule>,
    ref_count: usize,
}

#[derive(Debug, Default)]
struct ModuleRuntime {
    module: Mutex<RuntimeModule>,
    call_stack_ceiling: usize,
}

impl ModuleRuntime {
    fn new(module: RuntimeModule, call_stack_ceiling: usize) -> Self {
        Self {
            module: Mutex::new(module),
            call_stack_ceiling,
        }
    }

    fn call(&self, function_index: usize, params: &[u64]) -> RuntimeResult<Vec<u64>> {
        let mut module = lock_or_poison(&self.module);
        invoke_locked(&mut module, self.call_stack_ceiling, function_index, params)
    }

    fn call_from_import(&self, function_index: usize, params: &[u64]) -> RuntimeResult<Vec<u64>> {
        let mut module = match self.module.try_lock() {
            Ok(module) => module,
            Err(TryLockError::WouldBlock) => {
                return Err(Trap::new("reentrant imported call not supported"));
            }
            Err(TryLockError::Poisoned(err)) => err.into_inner(),
        };
        invoke_locked(&mut module, self.call_stack_ceiling, function_index, params)
    }

    fn snapshot(&self) -> RuntimeModule {
        lock_or_poison(&self.module).clone()
    }
}

fn invoke_locked(
    module: &mut RuntimeModule,
    call_stack_ceiling: usize,
    function_index: usize,
    params: &[u64],
) -> RuntimeResult<Vec<u64>> {
    let mut interpreter = Interpreter::new(std::mem::take(module));
    interpreter.call_stack_ceiling = call_stack_ceiling;
    let result = interpreter.call(function_index, params);
    *module = interpreter.module;
    result
}

fn lock_or_poison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(err) => err.into_inner(),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InterpFunctionHandle {
    index: Index,
}

impl InterpFunctionHandle {
    fn new(index: Index) -> Self {
        Self { index }
    }
}

impl FunctionHandle for InterpFunctionHandle {
    fn index(&self) -> Index {
        self.index
    }
}

#[derive(Debug, Clone)]
pub struct InterpModuleEngine {
    parent: Arc<CompiledModule>,
    instance: ModuleInstance,
    runtime: Arc<ModuleRuntime>,
}

impl InterpModuleEngine {
    pub fn new(
        parent: Arc<CompiledModule>,
        instance: ModuleInstance,
        call_stack_ceiling: usize,
    ) -> Result<Self, EngineError> {
        let module = build_runtime_module(&parent, &instance)?;
        Ok(Self {
            parent,
            instance,
            runtime: Arc::new(ModuleRuntime::new(module, call_stack_ceiling)),
        })
    }

    pub fn call(&self, function_index: Index, params: &[u64]) -> RuntimeResult<Vec<u64>> {
        self.runtime.call(function_index as usize, params)
    }

    pub fn snapshot(&self) -> RuntimeModule {
        self.runtime.snapshot()
    }

    pub fn memory_size(&self) -> Option<u32> {
        let module = lock_or_poison(&self.runtime.module);
        Some(module.memory.as_ref()?.bytes().len() as u32)
    }

    pub fn memory_read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        let module = lock_or_poison(&self.runtime.module);
        let memory = module.memory.as_ref()?;
        let end = offset.checked_add(len)?;
        memory.bytes().get(offset..end).map(ToOwned::to_owned)
    }

    pub fn memory_write_u32(&self, offset: Index, value: u32) -> bool {
        let mut module = lock_or_poison(&self.runtime.module);
        let Some(memory) = module.memory.as_mut() else {
            return false;
        };
        memory.write_u32_le(offset, value)
    }

    pub fn memory_grow(&self, delta_pages: u32, maximum_pages: Option<u32>) -> Option<u32> {
        let mut module = lock_or_poison(&self.runtime.module);
        let memory = module.memory.as_mut()?;
        if let Some(maximum_pages) = maximum_pages {
            memory.max_pages = Some(memory.max_pages.unwrap_or(maximum_pages).min(maximum_pages));
        }
        memory.grow(delta_pages)
    }

    pub fn global_value(&self, index: Index) -> Option<(u64, u64, WasmValueType)> {
        let module = lock_or_poison(&self.runtime.module);
        let global = module.globals.get(index as usize)?;
        let ty = self.instance.global_types.get(index as usize)?.val_type;
        Some((global.lo, global.hi, ty))
    }

    fn replace_function(&self, index: usize, function: Function) {
        if let Some(slot) = lock_or_poison(&self.runtime.module)
            .functions
            .get_mut(index)
        {
            *slot = function;
        }
    }

    fn signature_for_function(&self, index: Index) -> Result<Signature, EngineError> {
        let ty = self
            .parent
            .source
            .type_of_function(index)
            .ok_or_else(|| EngineError::new(format!("function[{index}] type is undefined")))?;
        Ok(signature_from_wasm_function_type(ty)?)
    }

    fn resolve_imported_module_engine<'a>(
        imported_module_engine: &'a dyn WasmModuleEngine,
    ) -> &'a InterpModuleEngine {
        unsafe {
            &*(imported_module_engine as *const dyn WasmModuleEngine as *const InterpModuleEngine)
        }
    }
}

impl WasmModuleEngine for InterpModuleEngine {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn new_function(&self, index: Index) -> Box<dyn FunctionHandle> {
        Box::new(InterpFunctionHandle::new(index))
    }

    fn resolve_imported_function(
        &mut self,
        index: Index,
        _desc_func: Index,
        index_in_imported_module: Index,
        imported_module_engine: &dyn WasmModuleEngine,
    ) {
        let imported = Self::resolve_imported_module_engine(imported_module_engine);
        let Ok(signature) = self.signature_for_function(index) else {
            return;
        };
        let runtime = imported.runtime.clone();
        let function = Function::new_host(
            signature.clone(),
            imported_host_function(signature, move |params| {
                runtime.call_from_import(index_in_imported_module as usize, params)
            }),
        );
        self.replace_function(index as usize, function);
    }

    fn resolve_imported_memory(&mut self, imported_module_engine: &dyn WasmModuleEngine) {
        let imported = Self::resolve_imported_module_engine(imported_module_engine);
        lock_or_poison(&self.runtime.module).memory = imported.snapshot().memory;
    }

    fn lookup_function(
        &self,
        table: &TableInstance,
        type_id: FunctionTypeId,
        table_offset: Index,
    ) -> Option<(&ModuleInstance, Index)> {
        let function_index = table
            .elements
            .get(table_offset as usize)
            .copied()
            .flatten()?;
        let function = self.instance.functions.get(function_index as usize)?;
        (function.type_id == type_id).then_some((&self.instance, function_index))
    }

    fn get_global_value(&self, index: Index) -> (u64, u64) {
        self.snapshot()
            .globals
            .get(index as usize)
            .map(|global| (global.lo, global.hi))
            .unwrap_or((0, 0))
    }

    fn set_global_value(&mut self, index: Index, lo: u64, hi: u64) {
        if let Some(global) = lock_or_poison(&self.runtime.module)
            .globals
            .get_mut(index as usize)
        {
            global.lo = lo;
            global.hi = hi;
            global.is_vector = self
                .instance
                .global_types
                .get(index as usize)
                .is_some_and(|ty| ty.val_type == WasmValueType::V128);
        }
    }

    fn owns_globals(&self) -> bool {
        true
    }

    fn function_instance_reference(&self, func_index: Index) -> Reference {
        Some(func_index)
    }
}

#[derive(Debug, Default)]
pub struct InterpEngine {
    compiled_modules: HashMap<ModuleId, CompiledModuleWithCount>,
    call_stack_ceiling: usize,
}

impl InterpEngine {
    pub fn new() -> Self {
        Self {
            compiled_modules: HashMap::new(),
            call_stack_ceiling: crate::interpreter::DEFAULT_CALL_STACK_CEILING,
        }
    }

    pub fn compiled_module(&self, module: &Module) -> Option<Arc<CompiledModule>> {
        self.compiled_modules
            .get(&module.id)
            .map(|entry| entry.compiled_module.clone())
    }

    fn compile_module_impl(&self, module: &Module) -> Result<CompiledModule, EngineError> {
        let types = module
            .type_section
            .iter()
            .map(signature_from_wasm_function_type)
            .collect::<Result<Vec<_>, _>>()?;
        let globals = visible_global_types(module)?;
        let interp_types = module
            .type_section
            .iter()
            .map(interp_function_type_from_wasm)
            .collect::<Result<Vec<_>, _>>()?;

        let local_functions = module
            .function_section
            .iter()
            .copied()
            .enumerate()
            .map(|(local_index, type_index)| {
                let signature = types.get(type_index as usize).cloned().ok_or_else(|| {
                    EngineError::new(format!("function type[{type_index}] out of range"))
                })?;
                let code = module
                    .code_section
                    .get(local_index)
                    .ok_or_else(|| EngineError::new(format!("code[{local_index}] missing")))?;
                if code.is_host_function() {
                    return host_function_from_code(code, signature);
                }

                let function_type =
                    interp_types
                        .get(type_index as usize)
                        .cloned()
                        .ok_or_else(|| {
                            EngineError::new(format!("function type[{type_index}] out of range"))
                        })?;
                let local_types = wasm_value_types_to_interp(&code.local_types)?;
                let lowered = Compiler
                    .lower_with_config(CompileConfig {
                        body: &code.body,
                        signature: function_type,
                        local_types: &local_types,
                        globals: &globals,
                        functions: &module.function_section,
                        types: &interp_types,
                        call_frame_stack_size_in_u64: 0,
                        ensure_termination: false,
                    })
                    .map_err(|err| EngineError::new(err.to_string()))?;
                Function::new_native(signature, lowered.operations)
                    .map_err(|err| EngineError::new(err.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(CompiledModule {
            source: module.clone(),
            types,
            local_functions,
        })
    }
}

impl WasmEngine for InterpEngine {
    fn close(&mut self) -> Result<(), EngineError> {
        self.compiled_modules.clear();
        Ok(())
    }

    fn compile_module(&mut self, module: &Module) -> Result<(), EngineError> {
        if let Some(existing) = self.compiled_modules.get_mut(&module.id) {
            existing.ref_count += 1;
            return Ok(());
        }
        let compiled = Arc::new(self.compile_module_impl(module)?);
        self.compiled_modules.insert(
            module.id,
            CompiledModuleWithCount {
                compiled_module: compiled,
                ref_count: 1,
            },
        );
        Ok(())
    }

    fn compiled_module_count(&self) -> u32 {
        self.compiled_modules.len() as u32
    }

    fn delete_compiled_module(&mut self, module: &Module) {
        let Some(entry) = self.compiled_modules.get_mut(&module.id) else {
            return;
        };
        if entry.ref_count > 1 {
            entry.ref_count -= 1;
            return;
        }
        self.compiled_modules.remove(&module.id);
    }

    fn new_module_engine(
        &self,
        module: &Module,
        instance: &ModuleInstance,
    ) -> Result<Box<dyn WasmModuleEngine>, EngineError> {
        let compiled = self.compiled_module(module).ok_or_else(|| {
            EngineError::new("source module must be compiled before instantiation")
        })?;
        Ok(Box::new(InterpModuleEngine::new(
            compiled,
            instance.clone(),
            self.call_stack_ceiling,
        )?))
    }
}

fn build_runtime_module(
    parent: &CompiledModule,
    instance: &ModuleInstance,
) -> Result<RuntimeModule, EngineError> {
    let import_function_count = instance.source.import_function_count as usize;
    let mut functions = Vec::with_capacity(instance.functions.len());
    for index in 0..instance.functions.len() {
        if index < import_function_count {
            let signature = signature_from_wasm_function_type(
                instance
                    .source
                    .type_of_function(index as u32)
                    .ok_or_else(|| {
                        EngineError::new(format!("function[{index}] type is undefined"))
                    })?,
            )?;
            functions.push(unresolved_import_function(index as u32, signature));
        } else {
            let local_index = index - import_function_count;
            functions.push(
                parent
                    .local_functions
                    .get(local_index)
                    .cloned()
                    .ok_or_else(|| {
                        EngineError::new(format!("function[{index}] missing compiled body"))
                    })?,
            );
        }
    }

    Ok(RuntimeModule {
        functions,
        globals: instance
            .globals
            .iter()
            .map(|global| crate::interpreter::GlobalValue {
                lo: global.value,
                hi: global.value_hi,
                is_vector: global.ty.val_type == WasmValueType::V128,
            })
            .collect(),
        memory: instance.memory_instance.as_ref().map(|memory| {
            Memory::from_bytes(
                memory.bytes.clone(),
                instance
                    .memory_type
                    .as_ref()
                    .and_then(|memory_type| memory_type.is_max_encoded.then_some(memory_type.max)),
            )
        }),
        tables: instance
            .tables
            .iter()
            .map(|table| Table {
                elements: table
                    .elements
                    .iter()
                    .copied()
                    .map(|reference| reference.map(|index| index as usize))
                    .collect(),
            })
            .collect(),
        types: parent.types.clone(),
        data_instances: instance.data_instances.iter().cloned().map(Some).collect(),
        exit_code: instance.exit_code(),
    })
}

fn unresolved_import_function(index: Index, signature: Signature) -> Function {
    Function::new_host(
        signature,
        host_function(move |_, _| {
            Err(Trap::new(format!(
                "function[{index}] import was not resolved"
            )))
        }),
    )
}

fn imported_host_function<F>(signature: Signature, invoke: F) -> HostFuncRef
where
    F: Fn(&[u64]) -> RuntimeResult<Vec<u64>> + Send + Sync + 'static,
{
    host_function(move |_, stack| {
        let results = invoke(&stack[..signature.param_slots])?;
        if results.len() != signature.result_slots {
            return Err(Trap::new(format!(
                "expected {} results, but imported call returned {}",
                signature.result_slots,
                results.len()
            )));
        }
        stack[..signature.result_slots].copy_from_slice(&results);
        Ok(())
    })
}

fn host_function_from_code(
    code: &razero_wasm::module::Code,
    signature: Signature,
) -> Result<Function, EngineError> {
    let host = code
        .host_func
        .clone()
        .ok_or_else(|| EngineError::new("host function body missing callback"))?;
    Ok(Function::new_host(signature, adapt_host_function(host)))
}

fn adapt_host_function(host: WasmHostFuncRef) -> HostFuncRef {
    host_function(move |_, stack| {
        let mut caller = Caller::default();
        host.call(&mut caller, stack)
            .map_err(|err| Trap::new(err.to_string()))
    })
}

fn visible_global_types(module: &Module) -> Result<Vec<InterpGlobalType>, EngineError> {
    module
        .import_section
        .iter()
        .filter_map(|import| match &import.desc {
            ImportDesc::Global(global) => Some(global_type_from_wasm(*global)),
            _ => None,
        })
        .chain(
            module
                .global_section
                .iter()
                .map(|global| global_type_from_wasm(global.ty)),
        )
        .collect()
}

fn signature_from_wasm_function_type(ty: &WasmFunctionType) -> Result<Signature, EngineError> {
    Ok(Signature::new(
        wasm_value_types_to_interp(&ty.params)?,
        wasm_value_types_to_interp(&ty.results)?,
    ))
}

fn interp_function_type_from_wasm(
    ty: &WasmFunctionType,
) -> Result<InterpFunctionType, EngineError> {
    Ok(InterpFunctionType::new(
        wasm_value_types_to_interp(&ty.params)?,
        wasm_value_types_to_interp(&ty.results)?,
    ))
}

fn global_type_from_wasm(ty: WasmGlobalType) -> Result<InterpGlobalType, EngineError> {
    Ok(InterpGlobalType {
        value_type: interp_value_type_from_wasm(ty.val_type)?,
    })
}

fn wasm_value_types_to_interp(
    types: &[WasmValueType],
) -> Result<Vec<InterpValueType>, EngineError> {
    types
        .iter()
        .copied()
        .map(interp_value_type_from_wasm)
        .collect()
}

fn interp_value_type_from_wasm(value: WasmValueType) -> Result<InterpValueType, EngineError> {
    match value {
        WasmValueType::I32 => Ok(InterpValueType::I32),
        WasmValueType::I64 => Ok(InterpValueType::I64),
        WasmValueType::F32 => Ok(InterpValueType::F32),
        WasmValueType::F64 => Ok(InterpValueType::F64),
        WasmValueType::V128 => Ok(InterpValueType::V128),
        WasmValueType::FUNCREF => Ok(InterpValueType::FuncRef),
        WasmValueType::EXTERNREF => Ok(InterpValueType::ExternRef),
        _ => Err(EngineError::new(format!(
            "unsupported interpreter value type {}",
            value.name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use razero_wasm::engine::Engine as _;
    use razero_wasm::host_func::stack_host_func;
    use razero_wasm::module::{
        Code, CodeBody, Export, ExternType, FunctionType, Import, Module, ValueType,
    };
    use razero_wasm::store::Store;

    use super::{InterpEngine, InterpModuleEngine};

    fn function_type(params: &[ValueType], results: &[ValueType]) -> FunctionType {
        let mut ty = FunctionType::default();
        ty.params.extend_from_slice(params);
        ty.results.extend_from_slice(results);
        ty.cache_num_in_u64();
        ty
    }

    #[test]
    fn compile_module_caches_and_reuses_compilation() {
        let mut engine = InterpEngine::new();
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x0b],
                ..Code::default()
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        engine.compile_module(&module).unwrap();
        assert_eq!(1, engine.compiled_module_count());
        engine.delete_compiled_module(&module);
        assert_eq!(1, engine.compiled_module_count());
        engine.delete_compiled_module(&module);
        assert_eq!(0, engine.compiled_module_count());
    }

    #[test]
    fn store_instantiation_executes_defined_function() {
        let mut store = Store::new(InterpEngine::new());
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        let module_id = store.instantiate(module, "demo", None).unwrap();
        let engine = store
            .module_engine(module_id)
            .unwrap()
            .as_any()
            .downcast_ref::<InterpModuleEngine>()
            .unwrap();

        assert_eq!(vec![42], engine.call(0, &[41]).unwrap());
    }

    #[test]
    fn store_instantiation_resolves_imported_functions() {
        let mut store = Store::new(InterpEngine::new());
        let host = Module {
            is_host_module: true,
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body_kind: CodeBody::Host,
                host_func: Some(stack_host_func(|stack| {
                    stack[0] = stack[0].wrapping_add(1);
                    Ok(())
                })),
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "inc".to_string(),
                index: 0,
            }],
            ..Module::default()
        };
        store.instantiate(host, "env", None).unwrap();

        let consumer = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            import_section: vec![Import::function("env", "inc", 0)],
            import_function_count: 1,
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x10, 0x00, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 1,
            }],
            ..Module::default()
        };

        let module_id = store.instantiate(consumer, "consumer", None).unwrap();
        let engine = store
            .module_engine(module_id)
            .unwrap()
            .as_any()
            .downcast_ref::<InterpModuleEngine>()
            .unwrap();

        assert_eq!(vec![42], engine.call(1, &[41]).unwrap());
    }
}
