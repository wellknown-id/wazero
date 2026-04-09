#![doc = "Minimal Linux/x86_64 startup support for linked AOT objects."]

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
use razero_wasm::engine::EngineError;

#[derive(Debug)]
pub struct LinkedModule {
    metadata: AotCompiledMetadata,
    text_base: usize,
    entry_preambles: AlignedBytes,
    entry_preamble_offsets: Vec<usize>,
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
        Ok(Self {
            metadata,
            text_base,
            entry_preambles,
            entry_preamble_offsets,
        })
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
        let target = self
            .metadata
            .start_function_index
            .or_else(|| {
                self.metadata
                    .exports
                    .iter()
                    .find(|export| export.ty == ExternType::FUNC && export.name == "_start")
                    .map(|export| export.index)
            })
            .ok_or_else(|| {
                LinkedModuleError::InvalidMetadata(
                    "linked module metadata has no start function".to_string(),
                )
            })?;
        let results = self.call_function_index(target, &[])?;
        if !results.is_empty() {
            return Err(LinkedModuleError::Unsupported(
                "start functions must not return values".to_string(),
            ));
        }
        Ok(())
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
            0,
            slots,
            ty.param_num_in_u64,
            ty.result_num_in_u64,
            None,
            None,
            None,
        );
        Ok(call_engine.call(&mut stack)?.to_vec())
    }
}

fn validate_metadata(metadata: &AotCompiledMetadata) -> Result<(), LinkedModuleError> {
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = metadata;
        return Err(LinkedModuleError::Unsupported(
            "linked AOT startup support currently targets Linux/x86_64 only".to_string(),
        ));
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        if metadata.target.operating_system != crate::aot::AotTargetOperatingSystem::Linux
            || metadata.target.architecture != crate::aot::AotTargetArchitecture::X86_64
        {
            return Err(LinkedModuleError::Unsupported(
                "linked module metadata does not target Linux/x86_64".to_string(),
            ));
        }
        if metadata.execution_context != AotExecutionContextMetadata::current() {
            return Err(LinkedModuleError::Unsupported(
                "linked module execution-context ABI does not match this runtime".to_string(),
            ));
        }
        if metadata.module_shape.is_host_module {
            return Err(LinkedModuleError::Unsupported(
                "host modules are not supported by linked startup support".to_string(),
            ));
        }
        if metadata.module_shape.import_function_count != 0
            || metadata.module_shape.import_global_count != 0
            || metadata.module_shape.import_memory_count != 0
            || metadata.module_shape.import_table_count != 0
            || metadata.module_shape.local_global_count != 0
            || metadata.module_shape.local_table_count != 0
            || metadata.module_shape.has_any_memory
            || metadata.module_shape.data_segment_count != 0
            || metadata.module_shape.element_segment_count != 0
            || metadata.ensure_termination
        {
            return Err(LinkedModuleError::Unsupported(
                "linked startup support currently requires modules without imports, memory, tables, globals, data, or runtime-injected helpers".to_string(),
            ));
        }
        if metadata.functions.is_empty() {
            return Err(LinkedModuleError::InvalidMetadata(
                "linked module metadata does not contain any compiled local functions".to_string(),
            ));
        }
        Ok(())
    }
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
    use razero_wasm::engine::Engine;
    use razero_wasm::module::{Code, Export, ExternType, FunctionType, Module, ValueType};

    use super::LinkedModule;
    use crate::engine::CompilerEngine;

    fn function_type(params: &[ValueType], results: &[ValueType]) -> FunctionType {
        let mut ty = FunctionType::default();
        ty.params = params.to_vec();
        ty.results = results.to_vec();
        ty.cache_num_in_u64();
        ty
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
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

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
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

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linked_module_rejects_runtime_dependent_shapes() {
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
            .contains("requires modules without imports, memory, tables, globals, data, or runtime-injected helpers"));
    }
}
