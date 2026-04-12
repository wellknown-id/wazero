#![doc = "Compiler module-engine glue."]

use std::sync::Arc;

use razero_wasm::engine::{FunctionHandle, FunctionTypeId, ModuleEngine as WasmModuleEngine};
use razero_wasm::module::Index;
use razero_wasm::module_instance::ModuleInstance;
use razero_wasm::table::{
    decode_function_reference, encode_function_reference, Reference, TableInstance,
};

use crate::call_engine::CallEngine;
use crate::engine::{AlignedBytes, CompiledModule};
use crate::hostmodule::build_host_module_opaque;

#[derive(Clone, Default)]
struct ImportedFunction {
    executable_ptr: usize,
    preamble_executable_ptr: usize,
    module_context_ptr: usize,
    type_id: FunctionTypeId,
    host_func: Option<razero_wasm::host_func::HostFuncRef>,
    compiled_module: Option<Arc<CompiledModule>>,
}

impl std::fmt::Debug for ImportedFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportedFunction")
            .field("executable_ptr", &self.executable_ptr)
            .field("preamble_executable_ptr", &self.preamble_executable_ptr)
            .field("module_context_ptr", &self.module_context_ptr)
            .field("type_id", &self.type_id)
            .field("has_host_func", &self.host_func.is_some())
            .field("has_compiled_module", &self.compiled_module.is_some())
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct CompilerModuleEngine {
    opaque: AlignedBytes,
    opaque_ptr: usize,
    parent: Arc<CompiledModule>,
    module: ModuleInstance,
    imported_functions: Vec<ImportedFunction>,
}

impl CompilerModuleEngine {
    pub fn new(parent: Arc<CompiledModule>, module: ModuleInstance) -> Self {
        let imported_functions =
            vec![ImportedFunction::default(); module.source.import_function_count as usize];
        Self {
            opaque: AlignedBytes::zeroed(0),
            opaque_ptr: 0,
            parent,
            module,
            imported_functions,
        }
    }

    pub fn opaque(&self) -> &[u8] {
        self.opaque.as_slice()
    }

    pub fn opaque_ptr(&self) -> usize {
        self.opaque_ptr
    }

    pub fn module(&self) -> &ModuleInstance {
        &self.module
    }

    pub fn parent(&self) -> &Arc<CompiledModule> {
        &self.parent
    }

    pub fn init_opaque(&mut self) {
        if self.parent.module.is_host_module {
            self.opaque = build_host_module_opaque(&self.parent.module);
        } else {
            self.opaque = AlignedBytes::zeroed(self.parent.offsets.total_size);
            self.setup_opaque();
        }
        self.opaque_ptr = self.opaque.as_ptr() as usize;
    }

    pub fn setup_opaque(&mut self) {
        let offsets = self.parent.offsets;
        if offsets.total_size == 0 {
            return;
        }
        if offsets.local_memory_begin.raw() >= 0 {
            self.put_local_memory();
        }
        let opaque = self.opaque.as_mut_slice();

        write_u64(
            opaque,
            offsets.module_instance_offset.raw() as usize,
            &self.module as *const ModuleInstance as usize as u64,
        );

        if offsets.globals_begin.raw() >= 0 {
            let mut cursor = offsets.globals_begin.raw() as usize;
            for global in &self.module.globals {
                let (value, value_hi) = global.value();
                write_u64(opaque, cursor, value);
                write_u64(opaque, cursor + 8, value_hi);
                cursor += 16;
            }
        }

        if offsets.type_ids_1st_element.raw() >= 0 && !self.module.type_ids.is_empty() {
            write_u64(
                opaque,
                offsets.type_ids_1st_element.raw() as usize,
                self.module.type_ids.as_ptr() as usize as u64,
            );
        }

        if offsets.tables_begin.raw() >= 0 {
            let mut cursor = offsets.tables_begin.raw() as usize;
            for table in &self.module.tables {
                write_u64(
                    opaque,
                    cursor,
                    table as *const TableInstance as usize as u64,
                );
                cursor += 8;
            }
        }

        write_optional_slice_ptr(
            opaque,
            offsets.data_instances_1st_element.raw(),
            self.module.data_instances.as_ptr() as usize,
            self.module.data_instances.is_empty(),
        );
        write_optional_slice_ptr(
            opaque,
            offsets.element_instances_1st_element.raw(),
            self.module.element_instances.as_ptr() as usize,
            self.module.element_instances.is_empty(),
        );
    }

    pub fn put_local_memory(&mut self) {
        let Some(memory) = self.module.memory_instance.as_ref() else {
            return;
        };
        let offset = self.parent.offsets.local_memory_begin.raw() as usize;
        let bytes = self.opaque.as_mut_slice();
        write_u64(
            bytes,
            offset,
            memory
                .bytes
                .first()
                .map_or(0, |byte| byte as *const u8 as usize) as u64,
        );
        write_u64(bytes, offset + 8, memory.bytes.len() as u64);
    }

    pub fn new_compiler_function(&self, index: Index) -> CallEngine {
        if index < self.module.source.import_function_count {
            let imported = &self.imported_functions[index as usize];
            let mut typ = self
                .module
                .source
                .type_of_function(index)
                .cloned()
                .unwrap_or_default();
            typ.cache_num_in_u64();
            let slots = typ.param_num_in_u64.max(typ.result_num_in_u64);
            return CallEngine::new(
                index,
                imported.executable_ptr,
                imported.preamble_executable_ptr,
                imported.module_context_ptr,
                slots,
                typ.param_num_in_u64,
                typ.result_num_in_u64,
                imported.host_func.clone(),
                imported
                    .compiled_module
                    .clone()
                    .or_else(|| Some(self.parent.clone())),
                Some(self.module.closed.clone()),
            );
        }

        let local_index = (index - self.module.source.import_function_count) as usize;
        let mut typ = self
            .module
            .source
            .type_of_function(index)
            .cloned()
            .unwrap_or_default();
        typ.cache_num_in_u64();
        let slots = typ.param_num_in_u64.max(typ.result_num_in_u64);
        let executable_ptr = self.parent.function_ptr(local_index).unwrap_or(0);
        let preamble_ptr = self
            .parent
            .entry_preamble_ptr(self.module.source.function_section[local_index] as usize)
            .unwrap_or(0);
        let host_func = self
            .module
            .source
            .code_section
            .get(local_index)
            .and_then(|code| code.host_func.clone());
        let mut call_engine = CallEngine::new(
            index,
            executable_ptr,
            preamble_ptr,
            self.opaque_ptr,
            slots,
            typ.param_num_in_u64,
            typ.result_num_in_u64,
            host_func,
            Some(self.parent.clone()),
            Some(self.module.closed.clone()),
        );
        call_engine.exec_ctx.memory_grow_trampoline_address =
            self.parent.shared_functions.memory_grow_ptr().unwrap_or(0);
        call_engine.exec_ctx.stack_grow_call_trampoline_address =
            self.parent.shared_functions.stack_grow_ptr().unwrap_or(0);
        call_engine
            .exec_ctx
            .check_module_exit_code_trampoline_address = self
            .parent
            .shared_functions
            .check_module_exit_code_ptr()
            .unwrap_or(0);
        call_engine.exec_ctx.table_grow_trampoline_address =
            self.parent.shared_functions.table_grow_ptr().unwrap_or(0);
        call_engine.exec_ctx.ref_func_trampoline_address =
            self.parent.shared_functions.ref_func_ptr().unwrap_or(0);
        call_engine.exec_ctx.memory_wait32_trampoline_address = self
            .parent
            .shared_functions
            .memory_wait32_ptr()
            .unwrap_or(0);
        call_engine.exec_ctx.memory_wait64_trampoline_address = self
            .parent
            .shared_functions
            .memory_wait64_ptr()
            .unwrap_or(0);
        call_engine.exec_ctx.memory_notify_trampoline_address = self
            .parent
            .shared_functions
            .memory_notify_ptr()
            .unwrap_or(0);
        if self.parent.fuel_enabled {
            call_engine.exec_ctx.fuel = self.parent.fuel;
        }
        call_engine
    }

    fn resolve_imported_module_engine<'a>(
        imported_module_engine: &'a dyn WasmModuleEngine,
    ) -> &'a CompilerModuleEngine {
        unsafe {
            &*(imported_module_engine as *const dyn WasmModuleEngine as *const CompilerModuleEngine)
        }
    }
}

impl WasmModuleEngine for CompilerModuleEngine {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn done_instantiation(&mut self) {
        self.init_opaque();
    }

    fn new_function(&self, index: Index) -> Box<dyn FunctionHandle> {
        Box::new(self.new_compiler_function(index))
    }

    fn resolve_imported_function(
        &mut self,
        index: Index,
        desc_func: Index,
        index_in_imported_module: Index,
        imported_module_engine: &dyn WasmModuleEngine,
    ) {
        let imported = Self::resolve_imported_module_engine(imported_module_engine);
        let target = if index_in_imported_module < imported.imported_functions.len() as u32 {
            imported.imported_functions[index_in_imported_module as usize].clone()
        } else {
            let local =
                (index_in_imported_module - imported.module.source.import_function_count) as usize;
            ImportedFunction {
                executable_ptr: imported.parent.function_ptr(local).unwrap_or(0),
                preamble_executable_ptr: imported
                    .parent
                    .entry_preamble_ptr(imported.module.source.function_section[local] as usize)
                    .unwrap_or(0),
                module_context_ptr: imported.opaque_ptr,
                type_id: self
                    .module
                    .type_ids
                    .get(desc_func as usize)
                    .copied()
                    .unwrap_or_default(),
                host_func: imported
                    .module
                    .source
                    .code_section
                    .get(local)
                    .and_then(|code| code.host_func.clone()),
                compiled_module: Some(imported.parent.clone()),
            }
        };

        let (exec_off, module_ctx_off, type_id_off) =
            self.parent.offsets.imported_function_offset(index);
        let opaque = self.opaque.as_mut_slice();
        write_u64(
            opaque,
            exec_off.raw() as usize,
            target.executable_ptr as u64,
        );
        write_u64(
            opaque,
            module_ctx_off.raw() as usize,
            target.module_context_ptr as u64,
        );
        write_u64(opaque, type_id_off.raw() as usize, target.type_id as u64);
        self.imported_functions[index as usize] = ImportedFunction { ..target };
    }

    fn resolve_imported_memory(&mut self, imported_module_engine: &dyn WasmModuleEngine) {
        let imported = Self::resolve_imported_module_engine(imported_module_engine);
        let offset = self.parent.offsets.imported_memory_begin.raw();
        if offset < 0 {
            return;
        }
        let Some(memory) = imported.module.memory_instance.as_ref() else {
            return;
        };
        let bytes = self.opaque.as_mut_slice();
        write_u64(bytes, offset as usize, memory as *const _ as usize as u64);
        write_u64(bytes, offset as usize + 8, imported.opaque_ptr as u64);
    }

    fn memory_snapshot(&self) -> Option<(Vec<u8>, Option<u32>, bool)> {
        self.module.memory_instance.as_ref().map(|memory| {
            (
                memory.bytes.to_vec(),
                self.module
                    .memory_type
                    .as_ref()
                    .and_then(|memory_type| memory_type.is_max_encoded.then_some(memory_type.max)),
                memory.shared,
            )
        })
    }

    fn overwrite_memory(&mut self, bytes: &[u8], maximum_pages: Option<u32>, shared: bool) -> bool {
        let Some(memory) = self.module.memory_instance.as_mut() else {
            return false;
        };
        memory.bytes.resize(bytes.len(), 0);
        memory.bytes[..bytes.len()].copy_from_slice(bytes);
        memory.shared = shared;
        memory.cap = memory.pages();
        if let Some(maximum_pages) = maximum_pages {
            memory.max = maximum_pages;
        }
        self.put_local_memory();
        true
    }

    fn lookup_function(
        &self,
        table: &TableInstance,
        type_id: FunctionTypeId,
        table_offset: Index,
    ) -> Option<(&ModuleInstance, Index)> {
        let reference = table.get(table_offset as usize).flatten()?;
        let (module_id, function_index) = decode_function_reference(reference);
        if module_id != self.module.id {
            return None;
        }
        let function = self.module.functions.get(function_index as usize)?;
        (function.type_id == type_id).then_some((&self.module, function_index))
    }

    fn get_global_value(&self, index: Index) -> (u64, u64) {
        self.module
            .globals
            .get(index as usize)
            .map(|global| global.value())
            .unwrap_or((0, 0))
    }

    fn set_global_value(&mut self, index: Index, lo: u64, hi: u64) {
        if let Some(global) = self.module.globals.get_mut(index as usize) {
            global.set_value(lo, hi);
            if self.parent.offsets.globals_begin.raw() >= 0 {
                let offset = self
                    .parent
                    .offsets
                    .global_instance_offset(index as usize)
                    .raw() as usize;
                let opaque = self.opaque.as_mut_slice();
                write_u64(opaque, offset, lo);
                write_u64(opaque, offset + 8, hi);
            }
        }
    }

    fn owns_globals(&self) -> bool {
        true
    }

    fn function_instance_reference(&self, func_index: Index) -> Reference {
        Some(encode_function_reference(self.module.id, func_index))
    }

    fn memory_grown(&mut self) {
        self.put_local_memory();
    }
}

fn write_optional_slice_ptr(bytes: &mut [u8], offset: i32, ptr: usize, empty: bool) {
    if offset >= 0 && !empty {
        write_u64(bytes, offset as usize, ptr as u64);
    }
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::aot::AotCompiledMetadata;
    use razero_wasm::engine::ModuleEngine;
    use razero_wasm::global::GlobalInstance;
    use razero_wasm::host_func::stack_host_func;
    use razero_wasm::memory::MemoryInstance;
    use razero_wasm::module::{
        Code, CodeBody, FunctionType, GlobalType, Memory, Module, Table, ValueType,
    };
    use razero_wasm::module_instance::ModuleInstance;
    use razero_wasm::table::TableInstance;

    use crate::engine::{CompiledModule, Executables, SharedFunctions, SourceMap};
    use crate::wazevoapi::ModuleContextOffsetData;

    use super::CompilerModuleEngine;

    fn compiled_module_for(module: &Module) -> Arc<CompiledModule> {
        Arc::new(CompiledModule {
            executables: Executables::default(),
            function_offsets: vec![16, 48],
            module: module.clone(),
            offsets: ModuleContextOffsetData::new(module, false),
            aot: AotCompiledMetadata::default(),
            shared_functions: Arc::new(SharedFunctions::default()),
            ensure_termination: false,
            fuel_enabled: false,
            fuel: 0,
            memory_isolation_enabled: false,
            source_map: SourceMap::default(),
        })
    }

    #[test]
    fn setup_opaque_writes_memory_globals_tables_and_instances() {
        let module = Module {
            memory_section: Some(Memory {
                min: 1,
                cap: 1,
                max: 1,
                ..Memory::default()
            }),
            global_section: vec![razero_wasm::module::Global {
                ty: GlobalType {
                    val_type: ValueType::I64,
                    mutable: true,
                },
                ..razero_wasm::module::Global::default()
            }],
            table_section: vec![Table::default()],
            ..Module::default()
        };
        let parent = compiled_module_for(&module);
        let mut instance = ModuleInstance::default();
        instance.source = module.clone();
        instance.memory_instance = Some(MemoryInstance {
            bytes: vec![0; 32].into(),
            ..MemoryInstance::default()
        });
        instance.globals.push(GlobalInstance::new(
            GlobalType {
                val_type: ValueType::I64,
                mutable: true,
            },
            7,
        ));
        instance.tables.push(TableInstance::default());
        instance.type_ids = vec![1, 2, 3];
        instance.data_instances = vec![vec![1, 2, 3]];
        instance.element_instances = vec![vec![Some(0)]];

        let mut engine = CompilerModuleEngine::new(parent.clone(), instance);
        engine.init_opaque();

        let bytes = engine.opaque();
        let local_mem = parent.offsets.local_memory_begin.raw() as usize;
        assert_ne!(
            u64::from_le_bytes(bytes[local_mem..local_mem + 8].try_into().unwrap()),
            0
        );
        assert_eq!(
            u64::from_le_bytes(bytes[local_mem + 8..local_mem + 16].try_into().unwrap()),
            32
        );

        let global_offset = parent.offsets.globals_begin.raw() as usize;
        assert_eq!(
            u64::from_le_bytes(bytes[global_offset..global_offset + 8].try_into().unwrap()),
            7
        );

        let table_offset = parent.offsets.tables_begin.raw() as usize;
        assert_ne!(
            u64::from_le_bytes(bytes[table_offset..table_offset + 8].try_into().unwrap()),
            0
        );
    }

    #[test]
    fn resolve_imported_function_writes_function_instance_layout() {
        let module = Module {
            import_function_count: 1,
            function_section: vec![0],
            type_section: vec![FunctionType::default()],
            ..Module::default()
        };
        let imported_parent = Arc::new(CompiledModule {
            executables: Executables::from_executable_bytes(vec![0; 128]),
            function_offsets: vec![32],
            module: Module {
                function_section: vec![0],
                type_section: vec![FunctionType::default()],
                ..Module::default()
            },
            offsets: ModuleContextOffsetData::default(),
            aot: AotCompiledMetadata::default(),
            shared_functions: Arc::new(SharedFunctions::default()),
            ensure_termination: false,
            fuel_enabled: false,
            fuel: 0,
            memory_isolation_enabled: false,
            source_map: SourceMap::default(),
        });
        let mut imported_instance = ModuleInstance::default();
        imported_instance.source = imported_parent.module.clone();
        let mut imported_engine =
            CompilerModuleEngine::new(imported_parent.clone(), imported_instance);
        imported_engine.init_opaque();

        let parent = compiled_module_for(&module);
        let mut instance = ModuleInstance::default();
        instance.source = module.clone();
        instance.type_ids = vec![9];
        let mut engine = CompilerModuleEngine::new(parent.clone(), instance);
        engine.init_opaque();
        engine.resolve_imported_function(0, 0, 0, &imported_engine);

        let (exec_off, ctx_off, ty_off) = parent.offsets.imported_function_offset(0);
        let opaque = engine.opaque();
        assert_eq!(
            u64::from_le_bytes(
                opaque[exec_off.raw() as usize..exec_off.raw() as usize + 8]
                    .try_into()
                    .unwrap()
            ),
            imported_parent.function_ptr(0).unwrap() as u64
        );
        assert_eq!(
            u64::from_le_bytes(
                opaque[ctx_off.raw() as usize..ctx_off.raw() as usize + 8]
                    .try_into()
                    .unwrap()
            ),
            imported_engine.opaque_ptr() as u64
        );
        assert_eq!(
            u64::from_le_bytes(
                opaque[ty_off.raw() as usize..ty_off.raw() as usize + 8]
                    .try_into()
                    .unwrap()
            ),
            9
        );
    }

    #[test]
    fn imported_host_function_creates_callable_call_engine() {
        let host = stack_host_func(|stack| {
            stack[0] = stack[0].wrapping_add(1);
            Ok(())
        });
        let mut ty = FunctionType::default();
        ty.params = vec![ValueType::I64];
        ty.results = vec![ValueType::I64];
        ty.cache_num_in_u64();

        let imported_module = Module {
            is_host_module: true,
            type_section: vec![ty.clone()],
            function_section: vec![0],
            code_section: vec![Code {
                body_kind: CodeBody::Host,
                host_func: Some(host),
                ..Code::default()
            }],
            ..Module::default()
        };
        let imported_parent = Arc::new(CompiledModule {
            executables: Executables::from_executable_bytes(vec![0; 64]),
            function_offsets: vec![16],
            module: imported_module.clone(),
            offsets: ModuleContextOffsetData::new(&imported_module, false),
            aot: AotCompiledMetadata::default(),
            shared_functions: Arc::new(SharedFunctions::default()),
            ensure_termination: false,
            fuel_enabled: false,
            fuel: 0,
            memory_isolation_enabled: false,
            source_map: SourceMap::default(),
        });
        let mut imported_instance = ModuleInstance::default();
        imported_instance.source = imported_module;
        let mut imported_engine =
            CompilerModuleEngine::new(imported_parent.clone(), imported_instance);
        imported_engine.init_opaque();

        let module = Module {
            import_function_count: 1,
            import_section: vec![razero_wasm::module::Import::function("env", "host", 0)],
            type_section: vec![ty],
            ..Module::default()
        };
        let parent = compiled_module_for(&module);
        let mut instance = ModuleInstance::default();
        instance.source = module;
        let mut engine = CompilerModuleEngine::new(parent, instance);
        engine.init_opaque();
        engine.resolve_imported_function(0, 0, 0, &imported_engine);

        let mut handle = engine.new_compiler_function(0);
        let mut stack = [41u64];
        let results = handle.call(&mut stack).unwrap();
        assert_eq!(results, &[42]);
    }

    #[test]
    fn local_function_initializes_execution_context_fuel_when_enabled() {
        let module = Module {
            type_section: vec![FunctionType::default()],
            function_section: vec![0],
            code_section: vec![Code::default()],
            ..Module::default()
        };
        let mut parent = (*compiled_module_for(&module)).clone();
        parent.fuel_enabled = true;
        parent.fuel = 7;
        let parent = Arc::new(parent);
        let mut instance = ModuleInstance::default();
        instance.source = module;
        let mut engine = CompilerModuleEngine::new(parent, instance);
        engine.init_opaque();

        let handle = engine.new_compiler_function(0);

        assert_eq!(handle.exec_ctx.fuel, 7);
    }

    #[test]
    fn imported_function_does_not_receive_parent_fuel() {
        // The fuel budget is a per-invocation property set on the local call engine.
        // Imported call engines must NOT inherit the parent's fuel because the imported
        // function's own call engine should not carry fuel from the caller.
        let host = stack_host_func(|stack| {
            stack[0] = stack[0].wrapping_add(1);
            Ok(())
        });
        let mut ty = FunctionType::default();
        ty.params = vec![ValueType::I64];
        ty.results = vec![ValueType::I64];
        ty.cache_num_in_u64();

        let imported_module = Module {
            is_host_module: true,
            type_section: vec![ty.clone()],
            function_section: vec![0],
            code_section: vec![Code {
                body_kind: CodeBody::Host,
                host_func: Some(host),
                ..Code::default()
            }],
            ..Module::default()
        };
        let imported_parent = Arc::new(CompiledModule {
            executables: Executables::from_executable_bytes(vec![0; 64]),
            function_offsets: vec![16],
            module: imported_module.clone(),
            offsets: ModuleContextOffsetData::new(&imported_module, false),
            aot: AotCompiledMetadata::default(),
            shared_functions: Arc::new(SharedFunctions::default()),
            ensure_termination: false,
            fuel_enabled: false,
            fuel: 0,
            memory_isolation_enabled: false,
            source_map: SourceMap::default(),
        });
        let mut imported_instance = ModuleInstance::default();
        imported_instance.source = imported_module;
        let mut imported_engine =
            CompilerModuleEngine::new(imported_parent.clone(), imported_instance);
        imported_engine.init_opaque();

        let module = Module {
            import_function_count: 1,
            import_section: vec![razero_wasm::module::Import::function("env", "host", 0)],
            type_section: vec![ty],
            ..Module::default()
        };
        let mut parent = (*compiled_module_for(&module)).clone();
        parent.fuel_enabled = true;
        parent.fuel = 42;
        let parent = Arc::new(parent);
        let mut instance = ModuleInstance::default();
        instance.source = module;
        let mut engine = CompilerModuleEngine::new(parent, instance);
        engine.init_opaque();
        engine.resolve_imported_function(0, 0, 0, &imported_engine);

        // Imported function call engine should have exec_ctx.fuel = 0.
        let handle = engine.new_compiler_function(0);
        assert_eq!(handle.exec_ctx.fuel, 0);
    }

    #[test]
    fn local_function_fuel_zero_when_disabled() {
        // When fuel_enabled is false, even if fuel field is non-zero (stale),
        // the call engine should have fuel=0 because the enable check is false.
        let module = Module {
            type_section: vec![FunctionType::default()],
            function_section: vec![0],
            code_section: vec![Code::default()],
            ..Module::default()
        };
        let mut parent = (*compiled_module_for(&module)).clone();
        parent.fuel_enabled = false;
        parent.fuel = 999; // stale value, should be ignored
        let parent = Arc::new(parent);
        let mut instance = ModuleInstance::default();
        instance.source = module;
        let mut engine = CompilerModuleEngine::new(parent, instance);
        engine.init_opaque();

        let handle = engine.new_compiler_function(0);
        assert_eq!(handle.exec_ctx.fuel, 0);
    }
}
