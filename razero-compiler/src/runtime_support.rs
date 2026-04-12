#![doc = "Minimal Linux/ELF startup support for linked AOT objects.\n\nThe versioned ABI assumptions are documented in `../AOT_PACKAGING_ABI.md`. This module intentionally covers a much narrower runtime shape than the general linker path and does not replace `razero`'s interpreter/runtime embedding APIs."]

use std::fmt::{Display, Formatter};

use razero_wasm::module::{ExternType, FunctionType, Index};

use crate::aot::{AotCompiledMetadata, AotExecutionContextMetadata, AotFunctionTypeMetadata};
#[cfg(target_arch = "x86_64")]
use crate::backend::isa::amd64::machine::Amd64Machine as NativeMachine;
#[cfg(target_arch = "aarch64")]
use crate::backend::isa::arm64::machine::Arm64Machine as NativeMachine;
use crate::backend::machine::Machine;
use crate::call_engine::{CallEngine, CallEngineError};
use crate::engine::AlignedBytes;
use crate::frontend::signature_for_wasm_function_type;
use crate::runtime_state::{build_linked_runtime_plan, LinkedRuntimePlan};
use razero_wasm::engine::EngineError;

/// Metadata-driven startup/call surface for a narrowly supported linked AOT module.
///
/// The stable inputs are the linked raw function symbols plus the serialized AOT sidecar. This
/// helper supports the current metadata-driven linked-runtime slice: local memory/globals/tables,
/// active data/element initialization, and start-section execution for modules without imports.
#[derive(Debug)]
pub struct LinkedModule {
    metadata: AotCompiledMetadata,
    text_base: usize,
    entry_preambles: AlignedBytes,
    entry_preamble_offsets: Vec<usize>,
    runtime_state: LinkedRuntimeState,
}

#[derive(Debug)]
#[allow(dead_code)]
struct LinkedRuntimeState {
    module_context: AlignedBytes,
    memory_bytes: Option<Vec<u8>>,
    type_ids: Vec<u32>,
    function_instances: Vec<LinkedFunctionInstance>,
    table_elements: Vec<Vec<usize>>,
    tables: Vec<LinkedTableInstance>,
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
struct LinkedFunctionInstance {
    executable_ptr: usize,
    module_context_ptr: usize,
    type_id: u32,
    reserved: u32,
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
struct LinkedTableInstance {
    base_address: *const usize,
    len: u32,
    reserved: u32,
}

#[derive(Debug)]
pub enum LinkedModuleError {
    InvalidMetadata(String),
    Unsupported(String),
    Compile(EngineError),
    Call(CallEngineError),
}

impl Display for LinkedModuleError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidMetadata(message) | Self::Unsupported(message) => f.write_str(message),
            Self::Compile(err) => Display::fmt(err, f),
            Self::Call(err) => Display::fmt(err, f),
        }
    }
}

impl std::error::Error for LinkedModuleError {}

impl From<EngineError> for LinkedModuleError {
    fn from(value: EngineError) -> Self {
        Self::Compile(value)
    }
}

impl From<CallEngineError> for LinkedModuleError {
    fn from(value: CallEngineError) -> Self {
        Self::Call(value)
    }
}

impl LinkedModule {
    pub fn from_metadata_and_text_base(
        metadata: AotCompiledMetadata,
        text_base: usize,
    ) -> Result<Self, LinkedModuleError> {
        validate_metadata(&metadata)?;
        let (entry_preambles, entry_preamble_offsets) = compile_entry_preambles(&metadata)?;
        let runtime_state = LinkedRuntimeState::new(&metadata, text_base)?;
        let linked = Self {
            metadata,
            text_base,
            entry_preambles,
            entry_preamble_offsets,
            runtime_state,
        };
        if let Some(start_index) = linked.metadata.start_function_index {
            linked.run_void_function(start_index)?;
        }
        Ok(linked)
    }

    pub fn from_metadata_and_first_local_function(
        metadata: AotCompiledMetadata,
        first_local_function_ptr: usize,
    ) -> Result<Self, LinkedModuleError> {
        let first = metadata.functions.first().ok_or_else(|| {
            LinkedModuleError::InvalidMetadata(
                "linked module metadata does not describe any local functions".to_string(),
            )
        })?;
        let text_base = first_local_function_ptr
            .checked_sub(first.executable_offset)
            .ok_or_else(|| {
                LinkedModuleError::InvalidMetadata(
                    "linked function pointer is below the recorded text offset".to_string(),
                )
            })?;
        Self::from_metadata_and_text_base(metadata, text_base)
    }

    pub fn metadata(&self) -> &AotCompiledMetadata {
        &self.metadata
    }

    pub fn call_export(&self, name: &str, params: &[u64]) -> Result<Vec<u64>, LinkedModuleError> {
        let export = self
            .metadata
            .exports
            .iter()
            .find(|export| export.name == name)
            .ok_or_else(|| {
                LinkedModuleError::InvalidMetadata(format!(
                    "linked module metadata has no export named {name:?}"
                ))
            })?;
        if export.ty != ExternType::FUNC {
            return Err(LinkedModuleError::Unsupported(format!(
                "export {name:?} is not a function"
            )));
        }
        self.call_function_index(export.index, params)
    }

    pub fn start(&self) -> Result<(), LinkedModuleError> {
        if self.metadata.start_function_index.is_some() {
            return Ok(());
        }
        let target = self
            .metadata
            .exports
            .iter()
            .find(|export| export.ty == ExternType::FUNC && export.name == "_start")
            .map(|export| export.index)
            .ok_or_else(|| {
                LinkedModuleError::InvalidMetadata(
                    "linked module metadata has no start function".to_string(),
                )
            })?;
        self.run_void_function(target)
    }

    fn call_function_index(
        &self,
        wasm_function_index: Index,
        params: &[u64],
    ) -> Result<Vec<u64>, LinkedModuleError> {
        let function = self
            .metadata
            .functions
            .iter()
            .find(|function| function.wasm_function_index == wasm_function_index)
            .ok_or_else(|| {
                LinkedModuleError::InvalidMetadata(format!(
                    "linked module metadata has no local function for wasm index {wasm_function_index}"
                ))
            })?;
        let ty = self
            .metadata
            .types
            .get(function.type_index as usize)
            .ok_or_else(|| {
                LinkedModuleError::InvalidMetadata(format!(
                    "linked module metadata is missing type {}",
                    function.type_index
                ))
            })?;
        if params.len() != ty.param_num_in_u64 {
            return Err(LinkedModuleError::Unsupported(format!(
                "function {wasm_function_index} expects {} parameter slots, got {}",
                ty.param_num_in_u64,
                params.len()
            )));
        }
        let preamble_ptr = self
            .entry_preamble_offsets
            .get(function.type_index as usize)
            .and_then(|offset| self.entry_preambles.ptr_at(*offset))
            .ok_or_else(|| {
                LinkedModuleError::InvalidMetadata(format!(
                    "linked module metadata is missing an entry preamble for type {}",
                    function.type_index
                ))
            })?;
        let executable_ptr = self.text_base + function.executable_offset;
        let slots = ty.param_num_in_u64.max(ty.result_num_in_u64);
        let mut stack = vec![0u64; slots];
        stack[..params.len()].copy_from_slice(params);
        let mut call_engine = CallEngine::new(
            wasm_function_index,
            executable_ptr,
            preamble_ptr,
            self.runtime_state.module_context_ptr(),
            slots,
            ty.param_num_in_u64,
            ty.result_num_in_u64,
            None,
            None,
            None,
        );
        Ok(call_engine.call(&mut stack)?.to_vec())
    }

    fn run_void_function(&self, wasm_function_index: Index) -> Result<(), LinkedModuleError> {
        let results = self.call_function_index(wasm_function_index, &[])?;
        if !results.is_empty() {
            return Err(LinkedModuleError::Unsupported(
                "start functions must not return values".to_string(),
            ));
        }
        Ok(())
    }
}

fn validate_metadata(metadata: &AotCompiledMetadata) -> Result<(), LinkedModuleError> {
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    )))]
    {
        let _ = metadata;
        return Err(LinkedModuleError::Unsupported(
            "linked AOT startup support currently targets Linux/x86_64 and Linux/aarch64 only"
                .to_string(),
        ));
    }
    #[cfg(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    ))]
    {
        let supported_architecture = crate::aot::AotTargetArchitecture::current();
        if metadata.target.operating_system != crate::aot::AotTargetOperatingSystem::Linux
            || metadata.target.architecture != supported_architecture
        {
            return Err(LinkedModuleError::Unsupported(format!(
                "linked module metadata does not target Linux/{}",
                supported_architecture.name()
            )));
        }
        if metadata.execution_context != AotExecutionContextMetadata::current() {
            return Err(LinkedModuleError::Unsupported(
                "linked module execution-context ABI does not match this runtime".to_string(),
            ));
        }
        build_linked_runtime_plan(metadata).map_err(LinkedModuleError::Unsupported)?;
        Ok(())
    }
}

impl LinkedRuntimeState {
    fn new(metadata: &AotCompiledMetadata, text_base: usize) -> Result<Self, LinkedModuleError> {
        let plan = build_linked_runtime_plan(metadata).map_err(LinkedModuleError::Unsupported)?;
        let LinkedRuntimePlan {
            memory_bytes,
            globals,
            tables: table_plans,
            type_ids,
        } = plan;
        let mut function_instances = metadata
            .functions
            .iter()
            .map(|function| LinkedFunctionInstance {
                executable_ptr: text_base + function.executable_offset,
                module_context_ptr: 0,
                type_id: function.type_index,
                reserved: 0,
            })
            .collect::<Vec<_>>();
        let mut table_elements = table_plans
            .iter()
            .map(|table| vec![0usize; table.elements.len()])
            .collect::<Vec<_>>();
        let mut tables = table_plans
            .iter()
            .map(|table| LinkedTableInstance {
                base_address: std::ptr::null(),
                len: table.elements.len() as u32,
                reserved: 0,
            })
            .collect::<Vec<_>>();
        let mut module_context = AlignedBytes::zeroed(metadata.module_context.total_size.max(1));
        let module_context_ptr = module_context.as_ptr() as usize;
        for function_instance in &mut function_instances {
            function_instance.module_context_ptr = module_context_ptr;
        }
        for (table_index, table) in table_plans.iter().enumerate() {
            for (element_index, wasm_function_index) in table.elements.iter().enumerate() {
                table_elements[table_index][element_index] = wasm_function_index
                    .and_then(|wasm_function_index| {
                        metadata.functions.iter().position(|function| {
                            function.wasm_function_index == wasm_function_index
                        })
                    })
                    .map(|slot| &function_instances[slot] as *const LinkedFunctionInstance as usize)
                    .unwrap_or(0);
            }
            tables[table_index].base_address = table_elements[table_index].as_ptr();
        }
        initialize_module_context(
            metadata,
            memory_bytes.as_deref(),
            &globals,
            &mut module_context,
            &type_ids,
            &tables,
        );
        Ok(Self {
            module_context,
            memory_bytes,
            type_ids,
            function_instances,
            table_elements,
            tables,
        })
    }

    fn module_context_ptr(&self) -> usize {
        self.module_context.as_ptr() as usize
    }
}

fn initialize_module_context(
    metadata: &AotCompiledMetadata,
    memory_bytes: Option<&[u8]>,
    globals: &[crate::runtime_state::LinkedGlobalValue],
    module_context: &mut AlignedBytes,
    type_ids: &[u32],
    tables: &[LinkedTableInstance],
) {
    let bytes = module_context.as_mut_slice();
    if metadata.module_context.local_memory_begin >= 0 {
        let offset = metadata.module_context.local_memory_begin as usize;
        let (base, len) = memory_bytes
            .map(|memory| {
                (
                    memory
                        .first()
                        .map_or(0usize, |byte| byte as *const u8 as usize),
                    memory.len(),
                )
            })
            .unwrap_or((0, 0));
        write_u64(bytes, offset, base as u64);
        write_u64(bytes, offset + 8, len as u64);
    }
    if metadata.module_context.globals_begin >= 0 {
        let mut offset = metadata.module_context.globals_begin as usize;
        for global in globals {
            write_u64(bytes, offset, global.value_lo);
            write_u64(bytes, offset + 8, global.value_hi);
            offset += 16;
        }
    }
    if metadata.module_context.type_ids_1st_element >= 0 && !type_ids.is_empty() {
        write_u64(
            bytes,
            metadata.module_context.type_ids_1st_element as usize,
            type_ids.as_ptr() as usize as u64,
        );
    }
    if metadata.module_context.tables_begin >= 0 {
        for (index, table) in tables.iter().enumerate() {
            write_u64(
                bytes,
                metadata.module_context.tables_begin as usize + index * 8,
                table as *const LinkedTableInstance as usize as u64,
            );
        }
    }
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn compile_entry_preambles(
    metadata: &AotCompiledMetadata,
) -> Result<(AlignedBytes, Vec<usize>), EngineError> {
    let mut bytes = Vec::new();
    let mut offsets = Vec::with_capacity(metadata.types.len());
    for ty in &metadata.types {
        while bytes.len() % 16 != 0 {
            bytes.push(0);
        }
        offsets.push(bytes.len());
        bytes.extend(native_compile_entry_preamble(ty)?);
    }
    let mut preambles = AlignedBytes::from_bytes(bytes);
    preambles
        .make_executable()
        .map_err(|err| EngineError::new(err.to_string()))?;
    Ok((preambles, offsets))
}

fn native_compile_entry_preamble(ty: &AotFunctionTypeMetadata) -> Result<Vec<u8>, EngineError> {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let mut function_type = FunctionType::default();
        function_type.params = ty.params.clone();
        function_type.results = ty.results.clone();
        function_type.param_num_in_u64 = ty.param_num_in_u64;
        function_type.result_num_in_u64 = ty.result_num_in_u64;
        function_type.cache_num_in_u64();
        let signature = signature_for_wasm_function_type(&function_type);
        let mut machine = NativeMachine::new();
        Ok(machine.compile_entry_preamble(&signature, false))
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = ty;
        Err(EngineError::new("unsupported architecture"))
    }
}

#[cfg(test)]
mod tests {
    use razero_features::CoreFeatures;
    use razero_wasm::engine::Engine;
    use razero_wasm::module::{
        Code, ConstExpr, DataSegment, ElementMode, ElementSegment, Export, ExternType,
        FunctionType, Global, GlobalType, Module, RefType, Table, ValueType,
    };

    use super::LinkedModule;
    use crate::aot::{
        AotExecutionContextMetadata, AotTargetArchitecture, AotTargetOperatingSystem,
    };
    use crate::engine::CompilerEngine;

    fn function_type(params: &[ValueType], results: &[ValueType]) -> FunctionType {
        let mut ty = FunctionType::default();
        ty.params = params.to_vec();
        ty.results = results.to_vec();
        ty.cache_num_in_u64();
        ty
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_calls_exported_function() {
        let mut engine = CompilerEngine::new();
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

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        assert_eq!(linked.call_export("run", &[41]).unwrap(), vec![42]);
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_call_export_rejects_missing_export() {
        let mut engine = CompilerEngine::new();
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

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        let err = linked.call_export("missing", &[41]).unwrap_err();
        assert!(err
            .to_string()
            .contains("linked module metadata has no export named \"missing\""));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_call_export_rejects_non_function_export() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            global_section: vec![Global {
                ty: GlobalType {
                    val_type: ValueType::I32,
                    mutable: false,
                },
                init: ConstExpr::from_i32(7),
            }],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b],
                ..Code::default()
            }],
            export_section: vec![
                Export {
                    ty: ExternType::FUNC,
                    name: "run".to_string(),
                    index: 0,
                },
                Export {
                    ty: ExternType::GLOBAL,
                    name: "g".to_string(),
                    index: 0,
                },
            ],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        let err = linked.call_export("g", &[]).unwrap_err();
        assert!(err.to_string().contains("export \"g\" is not a function"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_call_export_rejects_wrong_param_arity() {
        let mut engine = CompilerEngine::new();
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

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        let err = linked.call_export("run", &[]).unwrap_err();
        assert!(err
            .to_string()
            .contains("function 0 expects 1 parameter slots, got 0"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_call_export_rejects_export_without_local_function_metadata() {
        let mut engine = CompilerEngine::new();
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

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.exports[0].index = 9;
        let linked = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        let err = linked.call_export("run", &[41]).unwrap_err();
        assert!(err
            .to_string()
            .contains("linked module metadata has no local function for wasm index 9"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_call_export_rejects_missing_type_metadata() {
        let mut engine = CompilerEngine::new();
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

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.functions[0].type_index = 7;
        let linked = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        let err = linked.call_export("run", &[41]).unwrap_err();
        assert!(err
            .to_string()
            .contains("linked module metadata is missing type 7"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_call_export_rejects_missing_entry_preamble() {
        let mut engine = CompilerEngine::new();
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

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();
        linked.entry_preamble_offsets.clear();

        let err = linked.call_export("run", &[41]).unwrap_err();
        assert!(err
            .to_string()
            .contains("linked module metadata is missing an entry preamble for type 0"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_runs_start_export_when_no_start_section_exists() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "_start".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        linked.start().unwrap();
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_construction_runs_start_once_and_start_is_idempotent() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[]), function_type(&[], &[ValueType::I32])],
            function_section: vec![0, 1],
            memory_section: Some(razero_wasm::module::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                ..razero_wasm::module::Memory::default()
            }),
            code_section: vec![
                Code {
                    body: vec![
                        0x41, 0x00, // i32.const 0
                        0x41, 0x00, // i32.const 0
                        0x28, 0x02, 0x00, // i32.load align=2 offset=0
                        0x41, 0x01, // i32.const 1
                        0x6a, // i32.add
                        0x36, 0x02, 0x00, // i32.store align=2 offset=0
                        0x0b,
                    ],
                    ..Code::default()
                },
                Code {
                    body: vec![0x41, 0x00, 0x28, 0x02, 0x00, 0x0b],
                    ..Code::default()
                },
            ],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 1,
            }],
            start_section: Some(0),
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        assert_eq!(linked.call_export("run", &[]).unwrap(), vec![1]);
        linked.start().unwrap();
        assert_eq!(linked.call_export("run", &[]).unwrap(), vec![1]);
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_preserves_runtime_state_across_repeated_calls() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(razero_wasm::module::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                ..razero_wasm::module::Memory::default()
            }),
            code_section: vec![Code {
                body: vec![
                    0x41, 0x00, // i32.const 0
                    0x41, 0x00, // i32.const 0
                    0x28, 0x02, 0x00, // i32.load align=2 offset=0
                    0x41, 0x01, // i32.const 1
                    0x6a, // i32.add
                    0x36, 0x02, 0x00, // i32.store align=2 offset=0
                    0x41, 0x00, // i32.const 0
                    0x28, 0x02, 0x00, // i32.load align=2 offset=0
                    0x0b,
                ],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        assert_eq!(linked.call_export("run", &[]).unwrap(), vec![1]);
        assert_eq!(linked.call_export("run", &[]).unwrap(), vec![2]);
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_construction_rejects_value_returning_start_section() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            start_section: Some(0),
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let err = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("start functions must use the () -> () signature"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_construction_rejects_start_section_without_local_function_metadata() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x0b],
                ..Code::default()
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.start_function_index = Some(7);
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked module metadata has no local start function 7"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_construction_rejects_parameterized_start_section() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x1a, 0x0b],
                ..Code::default()
            }],
            start_section: Some(0),
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let err = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("start functions must use the () -> () signature"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_construction_rejects_start_section_with_missing_type_metadata() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x0b],
                ..Code::default()
            }],
            start_section: Some(0),
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.functions[0].type_index = 7;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked module metadata is missing type 7"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_start_rejects_missing_start_entrypoint() {
        let mut engine = CompilerEngine::new();
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

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        let err = linked.start().unwrap_err();
        assert!(err
            .to_string()
            .contains("linked module metadata has no start function"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_start_rejects_non_function_start_export() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x0b],
                ..Code::default()
            }],
            global_section: vec![Global {
                ty: GlobalType {
                    val_type: ValueType::I32,
                    mutable: false,
                },
                init: ConstExpr::from_i32(0),
            }],
            export_section: vec![Export {
                ty: ExternType::GLOBAL,
                name: "_start".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let linked = LinkedModule::from_metadata_and_first_local_function(compiled.aot.clone(), 0)
            .unwrap();

        let err = linked.start().unwrap_err();
        assert!(err
            .to_string()
            .contains("linked module metadata has no start function"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_start_rejects_start_export_without_local_function_metadata() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "_start".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        let start_export = metadata
            .exports
            .iter_mut()
            .find(|export| export.ty == ExternType::FUNC && export.name == "_start")
            .unwrap();
        start_export.index = 7;
        let linked =
            LinkedModule::from_metadata_and_first_local_function(metadata, compiled.function_ptr(0).unwrap())
                .unwrap();

        let err = linked.start().unwrap_err();
        assert!(err
            .to_string()
            .contains("linked module metadata has no local function for wasm index 7"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_start_rejects_parameterized_start_export() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x1a, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "_start".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        let err = linked.start().unwrap_err();
        assert!(err
            .to_string()
            .contains("function 0 expects 1 parameter slots, got 0"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_start_rejects_start_export_with_missing_type_metadata() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "_start".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.functions[0].type_index = 7;
        let linked = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        let err = linked.start().unwrap_err();
        assert!(err
            .to_string()
            .contains("linked module metadata is missing type 7"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_start_rejects_missing_entry_preamble_for_start_export() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "_start".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();
        linked.entry_preamble_offsets.clear();

        let err = linked.start().unwrap_err();
        assert!(err
            .to_string()
            .contains("linked module metadata is missing an entry preamble for type 0"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_start_rejects_value_returning_start_export() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "_start".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        let err = linked.start().unwrap_err();
        assert!(err.to_string().contains("start functions must not return values"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_initializes_runtime_state_from_metadata() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![
                function_type(&[], &[]),
                function_type(&[], &[ValueType::I32]),
            ],
            function_section: vec![1, 0, 1],
            table_section: vec![Table {
                min: 1,
                max: Some(1),
                ty: RefType::FUNCREF,
            }],
            memory_section: Some(razero_wasm::module::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                ..razero_wasm::module::Memory::default()
            }),
            global_section: vec![Global {
                ty: GlobalType {
                    val_type: ValueType::I32,
                    mutable: false,
                },
                init: ConstExpr::from_i32(0),
            }],
            code_section: vec![
                Code {
                    body: vec![0x41, 0x07, 0x0b],
                    ..Code::default()
                },
                Code {
                    body: vec![0x0b],
                    ..Code::default()
                },
                Code {
                    body: vec![0x41, 0x05, 0x0b],
                    ..Code::default()
                },
            ],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 2,
            }],
            start_section: Some(1),
            data_section: vec![DataSegment {
                offset_expression: ConstExpr::from_opcode(0x23, &[0]),
                init: vec![5],
                passive: false,
            }],
            element_section: vec![ElementSegment {
                offset_expr: ConstExpr::from_opcode(0x23, &[0]),
                table_index: 0,
                init: vec![ConstExpr::from_opcode(0xd2, &[0])],
                ty: RefType::FUNCREF,
                mode: ElementMode::Active,
            }],
            enabled_features: CoreFeatures::V2,
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let linked = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap();

        assert_eq!(linked.call_export("run", &[]).unwrap(), vec![5]);
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_imported_runtime_shapes() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            import_section: vec![razero_wasm::module::Import::function("env", "host", 0)],
            import_function_count: 1,
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x10, 0x00, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 1,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let err = LinkedModule::from_metadata_and_first_local_function(
            compiled.aot.clone(),
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked runtime packaging currently requires modules without imports"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_shared_memory_runtime_shapes() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(razero_wasm::module::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                ..razero_wasm::module::Memory::default()
            }),
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.memory.as_mut().unwrap().is_shared = true;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked runtime packaging does not support shared memories or atomics integration"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_host_module_runtime_shapes() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.module_shape.is_host_module = true;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("host modules are not supported by linked runtime packaging"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_runtime_injected_termination_helpers() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.ensure_termination = true;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked runtime packaging does not support runtime-injected termination helpers"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_passive_data_segments() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(razero_wasm::module::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                ..razero_wasm::module::Memory::default()
            }),
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            data_section: vec![DataSegment {
                offset_expression: ConstExpr::from_i32(0),
                init: vec![1],
                passive: false,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.data_segments[0].passive = true;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("data[0] is passive; linked runtime packaging only supports active data segments"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_active_data_without_local_memory() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(razero_wasm::module::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                ..razero_wasm::module::Memory::default()
            }),
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            data_section: vec![DataSegment {
                offset_expression: ConstExpr::from_i32(0),
                init: vec![1],
                passive: false,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.memory = None;
        metadata.module_shape.has_local_memory = false;
        metadata.module_shape.has_any_memory = false;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("active data segments require a defined local memory in linked runtime packaging"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_passive_element_segments() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![
                function_type(&[], &[ValueType::I32]),
                function_type(&[], &[]),
            ],
            function_section: vec![0, 1],
            table_section: vec![Table {
                min: 1,
                max: Some(1),
                ty: RefType::FUNCREF,
            }],
            code_section: vec![
                Code {
                    body: vec![0x41, 0x07, 0x0b],
                    ..Code::default()
                },
                Code {
                    body: vec![0x0b],
                    ..Code::default()
                },
            ],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            element_section: vec![ElementSegment {
                offset_expr: ConstExpr::from_i32(0),
                table_index: 0,
                init: vec![ConstExpr::from_opcode(0xd2, &[1])],
                ty: RefType::FUNCREF,
                mode: ElementMode::Active,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.element_segments[0].mode = ElementMode::Passive;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("element[0] uses Passive; linked runtime packaging only supports active element segments"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_element_segments_with_unknown_table() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![
                function_type(&[], &[ValueType::I32]),
                function_type(&[], &[]),
            ],
            function_section: vec![0, 1],
            table_section: vec![Table {
                min: 1,
                max: Some(1),
                ty: RefType::FUNCREF,
            }],
            code_section: vec![
                Code {
                    body: vec![0x41, 0x07, 0x0b],
                    ..Code::default()
                },
                Code {
                    body: vec![0x0b],
                    ..Code::default()
                },
            ],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            element_section: vec![ElementSegment {
                offset_expr: ConstExpr::from_i32(0),
                table_index: 0,
                init: vec![ConstExpr::from_opcode(0xd2, &[1])],
                ty: RefType::FUNCREF,
                mode: ElementMode::Active,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.element_segments[0].table_index = 1;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("element[0] references unknown table 1"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_non_funcref_tables() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            table_section: vec![Table {
                min: 1,
                max: Some(1),
                ty: RefType::FUNCREF,
            }],
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.tables[0].ty = RefType::EXTERNREF;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("table[0] uses externref, only funcref tables are supported by linked runtime packaging"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_non_funcref_element_segments() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![
                function_type(&[], &[ValueType::I32]),
                function_type(&[], &[]),
            ],
            function_section: vec![0, 1],
            table_section: vec![Table {
                min: 1,
                max: Some(1),
                ty: RefType::FUNCREF,
            }],
            code_section: vec![
                Code {
                    body: vec![0x41, 0x07, 0x0b],
                    ..Code::default()
                },
                Code {
                    body: vec![0x0b],
                    ..Code::default()
                },
            ],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            element_section: vec![ElementSegment {
                offset_expr: ConstExpr::from_i32(0),
                table_index: 0,
                init: vec![ConstExpr::from_opcode(0xd2, &[1])],
                ty: RefType::FUNCREF,
                mode: ElementMode::Active,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.element_segments[0].ty = RefType::EXTERNREF;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("element[0] uses externref; linked runtime packaging only supports funcref element segments"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_inconsistent_table_counts() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            table_section: vec![Table {
                min: 1,
                max: Some(1),
                ty: RefType::FUNCREF,
            }],
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.module_shape.local_table_count = 0;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked runtime metadata has inconsistent table counts"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_inconsistent_element_segment_counts() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![
                function_type(&[], &[ValueType::I32]),
                function_type(&[], &[]),
            ],
            function_section: vec![0, 1],
            table_section: vec![Table {
                min: 1,
                max: Some(1),
                ty: RefType::FUNCREF,
            }],
            code_section: vec![
                Code {
                    body: vec![0x41, 0x07, 0x0b],
                    ..Code::default()
                },
                Code {
                    body: vec![0x0b],
                    ..Code::default()
                },
            ],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            element_section: vec![ElementSegment {
                offset_expr: ConstExpr::from_i32(0),
                table_index: 0,
                init: vec![ConstExpr::from_opcode(0xd2, &[1])],
                ty: RefType::FUNCREF,
                mode: ElementMode::Active,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.module_shape.element_segment_count = 0;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked runtime metadata has inconsistent element segment counts"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_inconsistent_global_counts() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            global_section: vec![Global {
                ty: GlobalType {
                    val_type: ValueType::I32,
                    mutable: false,
                },
                init: ConstExpr::from_i32(0),
            }],
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.module_shape.local_global_count = 0;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked runtime metadata has inconsistent global counts"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_inconsistent_data_segment_counts() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(razero_wasm::module::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                ..razero_wasm::module::Memory::default()
            }),
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            data_section: vec![DataSegment {
                offset_expression: ConstExpr::from_i32(0),
                init: vec![1],
                passive: false,
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.module_shape.data_segment_count = 0;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked runtime metadata has inconsistent data segment counts"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_mismatched_global_initializer_type() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(razero_wasm::module::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                ..razero_wasm::module::Memory::default()
            }),
            global_section: vec![Global {
                ty: GlobalType {
                    val_type: ValueType::I32,
                    mutable: false,
                },
                init: ConstExpr::from_i32(0),
            }],
            table_section: vec![Table {
                min: 1,
                max: Some(1),
                ty: RefType::FUNCREF,
            }],
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            data_section: vec![DataSegment {
                offset_expression: ConstExpr::from_i32(0),
                init: vec![0xaa],
                passive: false,
            }],
            element_section: vec![ElementSegment {
                offset_expr: ConstExpr::from_i32(0),
                table_index: 0,
                init: vec![ConstExpr::from_opcode(0xd2, &[0])],
                ty: RefType::FUNCREF,
                mode: ElementMode::Active,
            }],
            enabled_features: CoreFeatures::V2,
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.global_initializers[0].init_expression = ConstExpr::from_i64(0).data;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("global[0] initializer type i64 does not match declared type i32"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_invalid_data_offset_opcode() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(razero_wasm::module::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                ..razero_wasm::module::Memory::default()
            }),
            global_section: vec![Global {
                ty: GlobalType {
                    val_type: ValueType::I32,
                    mutable: false,
                },
                init: ConstExpr::from_i32(0),
            }],
            table_section: vec![Table {
                min: 1,
                max: Some(1),
                ty: RefType::FUNCREF,
            }],
            code_section: vec![Code {
                body: vec![0x41, 0x07, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            data_section: vec![DataSegment {
                offset_expression: ConstExpr::from_i32(0),
                init: vec![0xaa],
                passive: false,
            }],
            element_section: vec![ElementSegment {
                offset_expr: ConstExpr::from_i32(0),
                table_index: 0,
                init: vec![ConstExpr::from_opcode(0xd2, &[0])],
                ty: RefType::FUNCREF,
                mode: ElementMode::Active,
            }],
            enabled_features: CoreFeatures::V2,
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.data_segments[0].offset_expression = vec![0xff, 0x0b];
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("invalid opcode for const expression: 0xff"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_missing_local_function_descriptors() {
        let mut engine = CompilerEngine::new();
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

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.functions.clear();
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked module metadata does not describe any local functions"));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn linked_module_rejects_first_function_pointer_below_text_offset() {
        let mut engine = CompilerEngine::new();
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

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.functions[0].executable_offset = compiled.function_ptr(0).unwrap() + 1;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked function pointer is below the recorded text offset"));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linked_module_rejects_non_linux_x86_64_targets() {
        let mut engine = CompilerEngine::new();
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

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.target.operating_system = AotTargetOperatingSystem::Windows;
        metadata.target.architecture = AotTargetArchitecture::Aarch64;
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked module metadata does not target Linux/x86_64"));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linked_module_rejects_execution_context_abi_mismatch() {
        let mut engine = CompilerEngine::new();
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

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let mut metadata = compiled.aot.clone();
        metadata.execution_context = AotExecutionContextMetadata {
            abi_version: AotExecutionContextMetadata::current().abi_version + 1,
            ..metadata.execution_context.clone()
        };
        let err = LinkedModule::from_metadata_and_first_local_function(
            metadata,
            compiled.function_ptr(0).unwrap(),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("linked module execution-context ABI does not match this runtime"));
    }
}
