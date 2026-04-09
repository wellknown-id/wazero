#![doc = "Native Linux/x86_64 executable packaging and link helpers."]

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use object::write::{Object as ElfObject, StandardSection, Symbol as ElfSymbol, SymbolSection};
use object::{
    Architecture as ObjectArchitecture, BinaryFormat, Endianness, SymbolFlags, SymbolKind,
    SymbolScope,
};
use razero_wasm::const_expr::{evaluate_const_expr, ConstExpr};
use razero_wasm::module::{FunctionType, Index, ValueType};

use crate::{
    aot::{
        deserialize_aot_metadata, AotCompiledMetadata, AotDataSegmentMetadata,
        AotFunctionTypeMetadata, AotTargetArchitecture, AotTargetOperatingSystem,
    },
    backend::isa::amd64::abi_entry_preamble::compile_entry_preamble,
    backend::isa::amd64::abi_host_call::compile_host_function_trampoline,
    engine::{linked_wasm_function_symbol_name, RelocatableObjectArtifact},
    entrypoint_amd64::ENTRY_ASM_SOURCE,
    frontend::signature_for_wasm_function_type,
    wazevoapi::{ExitCode, EXIT_CODE_MASK},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeLinkModule {
    pub name: String,
    pub object_bytes: Vec<u8>,
    pub metadata_sidecar_bytes: Vec<u8>,
}

impl NativeLinkModule {
    pub fn new(
        name: impl Into<String>,
        object_bytes: Vec<u8>,
        metadata_sidecar_bytes: Vec<u8>,
    ) -> Self {
        Self {
            name: name.into(),
            object_bytes,
            metadata_sidecar_bytes,
        }
    }

    pub fn from_artifact(name: impl Into<String>, artifact: RelocatableObjectArtifact) -> Self {
        Self::new(name, artifact.object_bytes, artifact.metadata_sidecar_bytes)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeCAbiExport {
    pub module_name: String,
    pub wasm_function_index: Index,
    pub symbol_name: String,
    pub params: Vec<ValueType>,
    pub results: Vec<ValueType>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeExecutablePackage {
    pub executable_path: PathBuf,
    pub metadata_bundle_path: PathBuf,
    pub cabi_exports: Vec<NativeCAbiExport>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeLinkError {
    message: String,
}

impl NativeLinkError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for NativeLinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NativeLinkError {}

pub fn link_native_executable(
    output_path: impl AsRef<Path>,
    modules: &[NativeLinkModule],
    static_libraries: &[PathBuf],
) -> Result<NativeExecutablePackage, NativeLinkError> {
    if modules.is_empty() {
        return Err(NativeLinkError::new(
            "native executable packaging requires at least one relocatable object",
        ));
    }

    let output_path = output_path.as_ref();
    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(io_err)?;
    let work_dir = append_path_suffix(output_path, ".razero-link");
    if work_dir.exists() {
        fs::remove_dir_all(&work_dir).map_err(io_err)?;
    }
    fs::create_dir_all(&work_dir).map_err(io_err)?;

    let metadata_bundle_path = append_path_suffix(output_path, ".razero-package");
    let mut metadata_bundle = Vec::new();
    metadata_bundle.extend_from_slice(b"RZPKG001");
    metadata_bundle.extend_from_slice(&(modules.len() as u32).to_le_bytes());

    let mut object_paths = Vec::with_capacity(modules.len());
    let mut module_specs = Vec::with_capacity(modules.len());
    let mut cabi_exports = Vec::new();
    let mut execution_context_size = 0usize;

    for (index, module) in modules.iter().enumerate() {
        let metadata = deserialize_aot_metadata(&module.metadata_sidecar_bytes)
            .map_err(|err| NativeLinkError::new(err.to_string()))?;
        validate_metadata_support(&module.name, &metadata)?;
        execution_context_size =
            execution_context_size.max(metadata.execution_context.size.max(16));

        let object_path = work_dir.join(format!("{index}-{}.o", sanitize_identifier(&module.name)));
        fs::write(&object_path, &module.object_bytes).map_err(io_err)?;
        object_paths.push(object_path);

        let sanitized_name = sanitize_identifier(&module.name);
        let exports = module_exports(&sanitized_name, &metadata);
        if exports.is_empty() {
            return Err(NativeLinkError::new(format!(
                "module '{}' does not expose any C ABI compatible functions",
                module.name
            )));
        }
        cabi_exports.extend(exports.iter().cloned());
        module_specs.push(ModuleSpec {
            sanitized_name,
            metadata,
        });

        let name_bytes = module.name.as_bytes();
        metadata_bundle.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
        metadata_bundle.extend_from_slice(name_bytes);
        metadata_bundle
            .extend_from_slice(&(module.metadata_sidecar_bytes.len() as u64).to_le_bytes());
        metadata_bundle.extend_from_slice(&module.metadata_sidecar_bytes);
    }

    fs::write(&metadata_bundle_path, metadata_bundle).map_err(io_err)?;

    let preamble_object_bytes = build_preamble_object(&module_specs)?;
    let preamble_object_path = work_dir.join("razero-cabi-preambles.o");
    fs::write(&preamble_object_path, preamble_object_bytes).map_err(io_err)?;

    let wrappers_source = build_wrapper_source(&module_specs, execution_context_size);
    let wrappers_source_path = work_dir.join("razero-cabi-wrappers.c");
    let wrappers_object_path = work_dir.join("razero-cabi-wrappers.o");
    fs::write(&wrappers_source_path, wrappers_source).map_err(io_err)?;
    compile_c_object(&wrappers_source_path, &wrappers_object_path)?;

    let entry_source_path = work_dir.join("razero-amd64-entry.S");
    let entry_object_path = work_dir.join("razero-amd64-entry.o");
    fs::write(&entry_source_path, ENTRY_ASM_SOURCE).map_err(io_err)?;
    compile_assembly_object(&entry_source_path, &entry_object_path)?;

    let cc = cc_path();
    let mut link = Command::new(&cc);
    link.arg("-no-pie")
        .arg("-o")
        .arg(output_path)
        .arg(&wrappers_object_path)
        .arg(&entry_object_path)
        .arg(&preamble_object_path);
    for object_path in &object_paths {
        link.arg(object_path);
    }
    for library in static_libraries {
        link.arg(library);
    }
    run_command(&mut link, "link native executable")?;

    fs::remove_dir_all(&work_dir).map_err(io_err)?;

    Ok(NativeExecutablePackage {
        executable_path: output_path.to_path_buf(),
        metadata_bundle_path,
        cabi_exports,
    })
}

pub fn link_hello_host_executable(
    output_path: impl AsRef<Path>,
    guest: &NativeLinkModule,
    static_libraries: &[PathBuf],
) -> Result<PathBuf, NativeLinkError> {
    let metadata = deserialize_aot_metadata(&guest.metadata_sidecar_bytes)
        .map_err(|err| NativeLinkError::new(err.to_string()))?;
    let hello_host = HelloHostSpec::from_metadata(&metadata)?;

    let output_path = output_path.as_ref();
    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(io_err)?;
    let work_dir = append_path_suffix(output_path, ".razero-link");
    if work_dir.exists() {
        fs::remove_dir_all(&work_dir).map_err(io_err)?;
    }
    fs::create_dir_all(&work_dir).map_err(io_err)?;

    let guest_object_path = work_dir.join("hello-host-guest.o");
    fs::write(&guest_object_path, &guest.object_bytes).map_err(io_err)?;

    let preamble_symbol = "razero_hello_host_run_preamble";
    let preamble_object_path = work_dir.join("hello-host-preamble.o");
    fs::write(
        &preamble_object_path,
        build_named_preamble_object(preamble_symbol, &hello_host.run_type)?,
    )
    .map_err(io_err)?;

    let entry_source_path = work_dir.join("razero-amd64-entry.S");
    let entry_object_path = work_dir.join("razero-amd64-entry.o");
    fs::write(&entry_source_path, ENTRY_ASM_SOURCE).map_err(io_err)?;
    compile_assembly_object(&entry_source_path, &entry_object_path)?;

    let wrapper_source_path = work_dir.join("hello-host-main.c");
    let wrapper_object_path = work_dir.join("hello-host-main.o");
    fs::write(
        &wrapper_source_path,
        build_hello_host_source(&metadata, &hello_host, preamble_symbol),
    )
    .map_err(io_err)?;
    compile_c_object(&wrapper_source_path, &wrapper_object_path)?;

    let host_import_object_path = work_dir.join("hello-host-import.o");
    fs::write(
        &host_import_object_path,
        build_host_import_object(&hello_host.import_symbol, &hello_host.import_type)?,
    )
    .map_err(io_err)?;

    let cc = cc_path();
    let mut link = Command::new(&cc);
    link.arg("-no-pie")
        .arg("-o")
        .arg(output_path)
        .arg(&wrapper_object_path)
        .arg(&entry_object_path)
        .arg(&preamble_object_path)
        .arg(&host_import_object_path)
        .arg(&guest_object_path);
    for library in static_libraries {
        link.arg(library);
    }
    run_command(&mut link, "link hello-host executable")?;
    fs::remove_dir_all(&work_dir).map_err(io_err)?;
    Ok(output_path.to_path_buf())
}

#[derive(Clone, Debug)]
struct ModuleSpec {
    sanitized_name: String,
    metadata: AotCompiledMetadata,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct HelloHostSpec {
    import_symbol: String,
    import_type: AotFunctionTypeMetadata,
    run_symbol: String,
    run_type: AotFunctionTypeMetadata,
    memory_len_bytes: usize,
    data_segments: Vec<HelloHostDataSegment>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct HelloHostDataSegment {
    offset: usize,
    init: Vec<u8>,
}

impl HelloHostSpec {
    fn from_metadata(metadata: &AotCompiledMetadata) -> Result<Self, NativeLinkError> {
        validate_hello_host_metadata(metadata)?;
        let print_import = metadata
            .imports
            .iter()
            .find(|import| matches!(import.desc, crate::aot::AotImportDescMetadata::Func(_)))
            .ok_or_else(|| NativeLinkError::new("hello-host guest must import env.print"))?;
        let crate::aot::AotImportDescMetadata::Func(import_type_index) = print_import.desc else {
            unreachable!()
        };
        let import_type = metadata
            .types
            .get(import_type_index as usize)
            .cloned()
            .ok_or_else(|| NativeLinkError::new("hello-host import type metadata is missing"))?;
        let run_export = metadata
            .exports
            .iter()
            .find(|export| export.ty.0 == 0 && export.name == "run")
            .ok_or_else(|| NativeLinkError::new("hello-host guest must export func run"))?;
        let run_function = metadata
            .functions
            .iter()
            .find(|function| function.wasm_function_index == run_export.index)
            .ok_or_else(|| {
                NativeLinkError::new("hello-host run export must resolve to a local function")
            })?;
        let run_type = metadata
            .types
            .get(run_function.type_index as usize)
            .cloned()
            .ok_or_else(|| {
                NativeLinkError::new("hello-host run export is missing type metadata")
            })?;
        if !run_type.params.is_empty() || !run_type.results.is_empty() {
            return Err(NativeLinkError::new(
                "hello-host run export must use the () -> () signature",
            ));
        }
        let memory = metadata
            .memory
            .as_ref()
            .ok_or_else(|| NativeLinkError::new("hello-host guest must define one local memory"))?;
        let memory_len_bytes = (memory.cap.max(memory.min) as usize)
            .saturating_mul(65_536)
            .max(memory.min as usize * 65_536);
        let mut data_segments = Vec::with_capacity(metadata.data_segments.len());
        for segment in &metadata.data_segments {
            if segment.passive {
                return Err(NativeLinkError::new(
                    "hello-host linker does not support passive data segments",
                ));
            }
            data_segments.push(HelloHostDataSegment {
                offset: active_data_offset(segment)?,
                init: segment.init.clone(),
            });
        }
        Ok(Self {
            import_symbol: "razero_import_function_0_env_print".to_string(),
            import_type,
            run_symbol: linked_wasm_function_symbol_name(&metadata.module_id, run_export.index),
            run_type,
            memory_len_bytes,
            data_segments,
        })
    }
}

fn validate_metadata_support(
    module_name: &str,
    metadata: &AotCompiledMetadata,
) -> Result<(), NativeLinkError> {
    if metadata.target.operating_system != AotTargetOperatingSystem::Linux {
        return Err(NativeLinkError::new(format!(
            "module '{module_name}' targets {:?}, only linux is supported",
            metadata.target.operating_system
        )));
    }
    if metadata.target.architecture != AotTargetArchitecture::X86_64 {
        return Err(NativeLinkError::new(format!(
            "module '{module_name}' targets {:?}, only x86_64 is supported",
            metadata.target.architecture
        )));
    }
    if metadata.module_shape.has_any_memory
        || metadata.module_shape.import_global_count > 0
        || metadata.module_shape.import_table_count > 0
        || metadata.module_shape.local_global_count > 0
        || metadata.module_shape.local_table_count > 0
        || metadata.module_shape.data_segment_count > 0
        || metadata.module_shape.element_segment_count > 0
        || metadata.module_shape.has_start_section
        || metadata.ensure_termination
    {
        return Err(NativeLinkError::new(format!(
            "module '{module_name}' uses runtime state that the Linux/x86_64 C ABI linker path does not package yet",
        )));
    }
    Ok(())
}

fn validate_hello_host_metadata(metadata: &AotCompiledMetadata) -> Result<(), NativeLinkError> {
    if metadata.target.operating_system != AotTargetOperatingSystem::Linux
        || metadata.target.architecture != AotTargetArchitecture::X86_64
    {
        return Err(NativeLinkError::new(
            "hello-host linker only supports Linux/x86_64 artifacts",
        ));
    }
    if metadata.module_shape.import_function_count != 1
        || metadata.module_shape.import_global_count != 0
        || metadata.module_shape.import_memory_count != 0
        || metadata.module_shape.import_table_count != 0
        || metadata.module_shape.local_global_count != 0
        || metadata.module_shape.local_table_count != 0
        || !metadata.module_shape.has_local_memory
        || metadata.module_shape.has_start_section
        || metadata.module_shape.element_segment_count != 0
        || metadata.ensure_termination
    {
        return Err(NativeLinkError::new(
            "hello-host linker expects exactly one env.print import, one local memory, no globals/tables/start, and no element segments",
        ));
    }
    if metadata
        .imports
        .iter()
        .filter(|import| matches!(import.desc, crate::aot::AotImportDescMetadata::Func(_)))
        .count()
        != 1
    {
        return Err(NativeLinkError::new(
            "hello-host linker expects exactly one imported function",
        ));
    }
    let print_import = metadata
        .imports
        .iter()
        .find(|import| matches!(import.desc, crate::aot::AotImportDescMetadata::Func(_)))
        .ok_or_else(|| NativeLinkError::new("hello-host linker could not find env.print"))?;
    if print_import.module != "env" || print_import.name != "print" {
        return Err(NativeLinkError::new(
            "hello-host linker expects the imported function to be env.print",
        ));
    }
    let crate::aot::AotImportDescMetadata::Func(type_index) = print_import.desc else {
        unreachable!()
    };
    let print_type = metadata
        .types
        .get(type_index as usize)
        .ok_or_else(|| NativeLinkError::new("hello-host import type metadata is missing"))?;
    if print_type.params != [ValueType::I32, ValueType::I32] || !print_type.results.is_empty() {
        return Err(NativeLinkError::new(
            "hello-host linker expects env.print to use the (i32, i32) -> () signature",
        ));
    }
    if metadata
        .exports
        .iter()
        .all(|export| !(export.ty.0 == 0 && export.name == "run"))
    {
        return Err(NativeLinkError::new(
            "hello-host guest must export func run",
        ));
    }
    Ok(())
}

fn active_data_offset(segment: &AotDataSegmentMetadata) -> Result<usize, NativeLinkError> {
    let (values, ty) = evaluate_const_expr(
        &ConstExpr::new(segment.offset_expression.clone()),
        |_index| unreachable!("hello-host data offsets must not read globals"),
        |_index| unreachable!("hello-host data offsets must not use ref.func"),
    )
    .map_err(|err| NativeLinkError::new(err.to_string()))?;
    match ty {
        ValueType::I32 | ValueType::I64 => Ok(values.first().copied().unwrap_or_default() as usize),
        _ => Err(NativeLinkError::new(
            "hello-host data offsets must evaluate to i32/i64",
        )),
    }
}

fn module_exports(module_name: &str, metadata: &AotCompiledMetadata) -> Vec<NativeCAbiExport> {
    metadata
        .functions
        .iter()
        .filter_map(|function| {
            let ty = metadata.types.get(function.type_index as usize)?;
            function_type_supported(ty).then(|| NativeCAbiExport {
                module_name: module_name.to_string(),
                wasm_function_index: function.wasm_function_index,
                symbol_name: cabi_wrapper_symbol_name(module_name, function.wasm_function_index),
                params: ty.params.clone(),
                results: ty.results.clone(),
            })
        })
        .collect()
}

fn function_type_supported(ty: &AotFunctionTypeMetadata) -> bool {
    ty.results.len() <= 1
        && ty.params.iter().all(|value| scalar_c_abi_value(*value))
        && ty.results.iter().all(|value| scalar_c_abi_value(*value))
}

fn scalar_c_abi_value(value: ValueType) -> bool {
    matches!(
        value,
        ValueType::I32 | ValueType::I64 | ValueType::F32 | ValueType::F64
    )
}

fn build_preamble_object(module_specs: &[ModuleSpec]) -> Result<Vec<u8>, NativeLinkError> {
    let mut text = Vec::new();
    let mut symbols = Vec::new();
    for module in module_specs {
        for (type_index, ty) in module.metadata.types.iter().enumerate() {
            if !function_type_supported(ty) {
                continue;
            }
            align_to_16(&mut text);
            let offset = text.len() as u64;
            let mut function_type = FunctionType::default();
            function_type.params = ty.params.clone();
            function_type.results = ty.results.clone();
            function_type.param_num_in_u64 = ty.param_num_in_u64;
            function_type.result_num_in_u64 = ty.result_num_in_u64;
            let bytes =
                compile_entry_preamble(&signature_for_wasm_function_type(&function_type), false);
            let size = bytes.len() as u64;
            text.extend_from_slice(&bytes);
            symbols.push((
                preamble_symbol_name(&module.sanitized_name, type_index as u32),
                offset,
                size,
            ));
        }
    }

    let mut object = ElfObject::new(
        BinaryFormat::Elf,
        ObjectArchitecture::X86_64,
        Endianness::Little,
    );
    object.add_file_symbol(b"razero-cabi-preambles".to_vec());
    let text_section = object.section_id(StandardSection::Text);
    object.set_section_data(text_section, text, 16);
    for (name, value, size) in symbols {
        object.add_symbol(ElfSymbol {
            name: name.into_bytes(),
            value,
            size,
            kind: SymbolKind::Text,
            scope: SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Section(text_section),
            flags: SymbolFlags::None,
        });
    }
    object
        .write()
        .map_err(|err| NativeLinkError::new(err.to_string()))
}

#[allow(dead_code)]
fn build_named_preamble_object(
    symbol_name: &str,
    ty: &AotFunctionTypeMetadata,
) -> Result<Vec<u8>, NativeLinkError> {
    let mut function_type = FunctionType::default();
    function_type.params = ty.params.clone();
    function_type.results = ty.results.clone();
    function_type.param_num_in_u64 = ty.param_num_in_u64;
    function_type.result_num_in_u64 = ty.result_num_in_u64;
    let mut bytes =
        compile_entry_preamble(&signature_for_wasm_function_type(&function_type), false);
    if ty.params.is_empty() && ty.results.is_empty() && bytes.starts_with(&[0x55, 0x48, 0x89, 0xe5])
    {
        bytes.drain(..4);
    }

    let mut object = ElfObject::new(
        BinaryFormat::Elf,
        ObjectArchitecture::X86_64,
        Endianness::Little,
    );
    object.add_file_symbol(b"razero-hello-host-preamble".to_vec());
    let text_section = object.section_id(StandardSection::Text);
    object.set_section_data(text_section, bytes.clone(), 16);
    object.add_symbol(ElfSymbol {
        name: symbol_name.as_bytes().to_vec(),
        value: 0,
        size: bytes.len() as u64,
        kind: SymbolKind::Text,
        scope: SymbolScope::Linkage,
        weak: false,
        section: SymbolSection::Section(text_section),
        flags: SymbolFlags::None,
    });
    object
        .write()
        .map_err(|err| NativeLinkError::new(err.to_string()))
}

#[allow(dead_code)]
fn build_host_import_object(
    symbol_name: &str,
    ty: &AotFunctionTypeMetadata,
) -> Result<Vec<u8>, NativeLinkError> {
    let mut function_type = FunctionType::default();
    function_type.params = ty.params.clone();
    function_type.results = ty.results.clone();
    function_type.param_num_in_u64 = ty.param_num_in_u64;
    function_type.result_num_in_u64 = ty.result_num_in_u64;
    let bytes = compile_host_function_trampoline(
        ExitCode::call_go_function_with_index(0, false),
        &signature_for_wasm_function_type(&function_type),
        true,
    );

    let mut object = ElfObject::new(
        BinaryFormat::Elf,
        ObjectArchitecture::X86_64,
        Endianness::Little,
    );
    object.add_file_symbol(b"razero-hello-host-imports".to_vec());
    let text_section = object.section_id(StandardSection::Text);
    object.set_section_data(text_section, bytes.clone(), 16);
    object.add_symbol(ElfSymbol {
        name: symbol_name.as_bytes().to_vec(),
        value: 0,
        size: bytes.len() as u64,
        kind: SymbolKind::Text,
        scope: SymbolScope::Linkage,
        weak: false,
        section: SymbolSection::Section(text_section),
        flags: SymbolFlags::None,
    });
    object
        .write()
        .map_err(|err| NativeLinkError::new(err.to_string()))
}

#[allow(dead_code)]
fn build_hello_host_source(
    metadata: &AotCompiledMetadata,
    hello_host: &HelloHostSpec,
    preamble_symbol: &str,
) -> String {
    let mut source = String::new();
    source.push_str("#include <stdint.h>\n#include <stdio.h>\n#include <string.h>\n\n");
    source.push_str(
        "extern void razero_amd64_entrypoint(const unsigned char*, const unsigned char*, uintptr_t, const unsigned char*, uint64_t*, uintptr_t);\n",
    );
    source.push_str(
        "extern void razero_amd64_after_go_function_call_entrypoint(const unsigned char*, uintptr_t, uintptr_t, uintptr_t);\n",
    );
    source.push_str(&format!(
        "extern void {}(void);\nextern const unsigned char {}[];\nextern const unsigned char {}[];\n\n",
        hello_host.run_symbol, preamble_symbol, hello_host.import_symbol
    ));
    source.push_str(&format!(
        "enum {{ RAZERO_EXIT_OK = 0u, RAZERO_EXIT_CALL_GO_FUNCTION = {}u, RAZERO_EXIT_MASK = {}u }};\n\n",
        ExitCode::CALL_GO_FUNCTION.raw(),
        EXIT_CODE_MASK
    ));
    source.push_str(&format!(
        "typedef struct __attribute__((aligned(16))) {{ unsigned char bytes[{exec_ctx_size}]; }} razero_link_exec_ctx_t;\n",
        exec_ctx_size = metadata.execution_context.size.max(16)
    ));
    source.push_str(&format!(
        "static unsigned char guest_memory[{memory_len}] = {{0}};\n",
        memory_len = hello_host.memory_len_bytes.max(1)
    ));
    source.push_str(&format!(
        "static struct __attribute__((aligned(16))) {{ unsigned char bytes[{ctx_size}]; }} guest_module_ctx = {{0}};\n\n",
        ctx_size = metadata.module_context.total_size.max(1)
    ));
    for (index, data) in hello_host.data_segments.iter().enumerate() {
        source.push_str(&format!(
            "static const unsigned char guest_data_{index}[{len}] = {{{bytes}}};\n",
            len = data.init.len().max(1),
            bytes = comma_hex_bytes(&data.init)
        ));
    }
    source.push_str(
        "\nstatic uint32_t read_u32(const unsigned char* base, uintptr_t offset) {\n    uint32_t value;\n    memcpy(&value, base + offset, sizeof(value));\n    return value;\n}\n\n",
    );
    source.push_str(
        "static uint64_t read_u64(const unsigned char* base, uintptr_t offset) {\n    uint64_t value;\n    memcpy(&value, base + offset, sizeof(value));\n    return value;\n}\n\n",
    );
    source.push_str(
        "static void store_u32(unsigned char* base, uintptr_t offset, uint32_t value) {\n    memcpy(base + offset, &value, sizeof(value));\n}\n\n",
    );
    source.push_str(
        "static void store_u64(unsigned char* base, uintptr_t offset, uint64_t value) {\n    memcpy(base + offset, &value, sizeof(value));\n}\n\n",
    );
    source.push_str("int main(void) {\n");
    source.push_str("    razero_link_exec_ctx_t exec_ctx = {{0}};\n");
    source.push_str("    uint64_t param_result[1] = {0};\n");
    source.push_str("    uint64_t go_stack[4096] = {0};\n");
    source.push_str(
        "    uintptr_t stack_top = (((uintptr_t)(go_stack + 4096)) - 1u) & ~(uintptr_t)15u;\n",
    );
    for (index, data) in hello_host.data_segments.iter().enumerate() {
        source.push_str(&format!(
            "    memcpy(guest_memory + {offset}, guest_data_{index}, {len});\n",
            offset = data.offset,
            len = data.init.len()
        ));
    }
    source.push_str(&format!(
        "    store_u64(guest_module_ctx.bytes, {mem_base}, (uint64_t)(uintptr_t)guest_memory);\n    store_u64(guest_module_ctx.bytes, {mem_len}, (uint64_t){memory_len});\n",
        mem_base = metadata.module_context.local_memory_begin,
        mem_len = metadata.module_context.local_memory_begin + 8,
        memory_len = hello_host.memory_len_bytes.max(1)
    ));
    source.push_str(&format!(
        "    store_u64(guest_module_ctx.bytes, {import_base}, (uint64_t)(uintptr_t){});\n    store_u64(guest_module_ctx.bytes, {import_ctx}, 0);\n    store_u64(guest_module_ctx.bytes, {import_type}, 0);\n",
        hello_host.import_symbol,
        import_base = metadata.module_context.imported_functions_begin,
        import_ctx = metadata.module_context.imported_functions_begin + 8,
        import_type = metadata.module_context.imported_functions_begin + 16
    ));
    source.push_str(&format!(
        "    razero_amd64_entrypoint({preamble_symbol}, (const unsigned char*)&{run_symbol}, (uintptr_t)&exec_ctx, guest_module_ctx.bytes, param_result, stack_top);\n    for (;;) {{\n        uint32_t exit_code = read_u32(exec_ctx.bytes, {exit_code_offset});\n        switch (exit_code & RAZERO_EXIT_MASK) {{\n            case RAZERO_EXIT_OK:\n                return 0;\n            case RAZERO_EXIT_CALL_GO_FUNCTION: {{\n                uintptr_t go_stack_ptr = (uintptr_t)read_u64(exec_ctx.bytes, {go_stack_offset});\n                uintptr_t return_address = (uintptr_t)read_u64(exec_ctx.bytes, {return_offset});\n                uintptr_t frame_pointer = (uintptr_t)read_u64(exec_ctx.bytes, {frame_offset});\n                uint64_t* words = (uint64_t*)go_stack_ptr;\n                if ((exit_code >> 8) != 0u) {{ fprintf(stderr, \"unexpected host function index %u\\n\", exit_code >> 8); return 3; }}\n                if ((words[0] / 8u) < 2u) {{ fprintf(stderr, \"host stack is too small\\n\"); return 4; }}\n                uint32_t ptr = (uint32_t)words[1];\n                uint32_t len = (uint32_t)words[2];\n                if ((uint64_t)ptr + (uint64_t)len > (uint64_t){memory_len}) {{ fprintf(stderr, \"print slice is out of bounds\\n\"); return 5; }}\n                fwrite(guest_memory + ptr, 1u, len, stdout);\n                fflush(stdout);\n                store_u32(exec_ctx.bytes, {exit_code_offset}, RAZERO_EXIT_OK);\n                razero_amd64_after_go_function_call_entrypoint((const unsigned char*)return_address, (uintptr_t)&exec_ctx, go_stack_ptr, frame_pointer);\n                break;\n            }}\n            default:\n                fprintf(stderr, \"unsupported exit code %u while running hello-host guest\\n\", exit_code);\n                return 6;\n        }}\n    }}\n}}\n",
        run_symbol = hello_host.run_symbol,
        exit_code_offset = metadata.execution_context.exit_code_offset,
        go_stack_offset = metadata.execution_context.stack_pointer_before_go_call_offset,
        return_offset = metadata.execution_context.go_call_return_address_offset,
        frame_offset = metadata.execution_context.frame_pointer_before_go_call_offset,
        memory_len = hello_host.memory_len_bytes.max(1)
    ));
    source
}

fn build_wrapper_source(module_specs: &[ModuleSpec], execution_context_size: usize) -> String {
    let mut source = String::new();
    source.push_str("#include <stdint.h>\n#include <stddef.h>\n\n");
    source.push_str(
        "extern void razero_amd64_entrypoint(const unsigned char*, const unsigned char*, uintptr_t, const unsigned char*, uint64_t*, uintptr_t);\n\n",
    );
    source.push_str(&format!(
        "typedef struct __attribute__((aligned(16))) {{ unsigned char bytes[{execution_context_size}]; }} razero_link_exec_ctx_t;\n\n"
    ));

    for module in module_specs {
        for function in &module.metadata.functions {
            let Some(ty) = module.metadata.types.get(function.type_index as usize) else {
                continue;
            };
            if !function_type_supported(ty) {
                continue;
            }
            let preamble_symbol = preamble_symbol_name(&module.sanitized_name, function.type_index);
            let raw_symbol = linked_wasm_function_symbol_name(
                &module.metadata.module_id,
                function.wasm_function_index,
            );
            source.push_str(&format!(
                "extern void {raw_symbol}(void);\nextern const unsigned char {preamble_symbol}[];\n"
            ));
            source.push_str(&wrapper_function_source(
                &module.sanitized_name,
                function.wasm_function_index,
                ty,
                &raw_symbol,
                &preamble_symbol,
            ));
            source.push('\n');
        }
    }
    source
}

fn wrapper_function_source(
    module_name: &str,
    wasm_function_index: Index,
    ty: &AotFunctionTypeMetadata,
    raw_symbol: &str,
    preamble_symbol: &str,
) -> String {
    let mut source = String::new();
    let function_name = cabi_wrapper_symbol_name(module_name, wasm_function_index);
    let return_type = ty
        .results
        .first()
        .copied()
        .map(c_type_name)
        .unwrap_or("void");
    let params = ty
        .params
        .iter()
        .enumerate()
        .map(|(index, value)| format!("{} arg{index}", c_type_name(*value)))
        .collect::<Vec<_>>()
        .join(", ");
    let params = if params.is_empty() {
        "void"
    } else {
        params.as_str()
    };
    let slot_count = ty.param_num_in_u64.max(ty.result_num_in_u64).max(1);
    source.push_str(&format!("{return_type} {function_name}({params}) {{\n"));
    source.push_str(&format!(
        "    uint64_t param_result[{slot_count}] = {{0}};\n"
    ));
    source.push_str("    razero_link_exec_ctx_t execution_context = {{0}};\n");
    source.push_str("    uint64_t go_stack[4096] = {0};\n");
    for (index, value) in ty.params.iter().enumerate() {
        source.push_str(&param_pack_source(*value, index));
    }
    source.push_str(&format!(
        "    razero_amd64_entrypoint({preamble_symbol}, (const unsigned char*)&{raw_symbol}, (uintptr_t)&execution_context, NULL, param_result, (uintptr_t)go_stack);\n"
    ));
    if let Some(result) = ty.results.first().copied() {
        source.push_str(&result_unpack_source(result));
    }
    source.push_str("}\n");
    source
}

fn param_pack_source(value: ValueType, index: usize) -> String {
    match value {
        ValueType::I32 => {
            format!("    param_result[{index}] = (uint32_t)arg{index};\n")
        }
        ValueType::I64 => {
            format!("    param_result[{index}] = (uint64_t)arg{index};\n")
        }
        ValueType::F32 => format!(
            "    union {{ float value; uint32_t bits; }} arg{index}_bits = {{ .value = arg{index} }};\n    param_result[{index}] = arg{index}_bits.bits;\n"
        ),
        ValueType::F64 => format!(
            "    union {{ double value; uint64_t bits; }} arg{index}_bits = {{ .value = arg{index} }};\n    param_result[{index}] = arg{index}_bits.bits;\n"
        ),
        other => unreachable!("unsupported C ABI parameter type {}", other.name()),
    }
}

fn result_unpack_source(value: ValueType) -> String {
    match value {
        ValueType::I32 => "    return (int32_t)(uint32_t)param_result[0];\n".to_string(),
        ValueType::I64 => "    return (int64_t)param_result[0];\n".to_string(),
        ValueType::F32 => "    union { float value; uint32_t bits; } result = { .bits = (uint32_t)param_result[0] };\n    return result.value;\n".to_string(),
        ValueType::F64 => "    union { double value; uint64_t bits; } result = { .bits = param_result[0] };\n    return result.value;\n".to_string(),
        other => unreachable!("unsupported C ABI result type {}", other.name()),
    }
}

fn cabi_wrapper_symbol_name(module_name: &str, wasm_function_index: Index) -> String {
    format!(
        "razero_cabi_{}_function_{}",
        sanitize_identifier(module_name),
        wasm_function_index
    )
}

fn preamble_symbol_name(module_name: &str, type_index: Index) -> String {
    format!(
        "razero_cabi_{}_type_{}_preamble",
        sanitize_identifier(module_name),
        type_index
    )
}

fn c_type_name(value: ValueType) -> &'static str {
    match value {
        ValueType::I32 => "int32_t",
        ValueType::I64 => "int64_t",
        ValueType::F32 => "float",
        ValueType::F64 => "double",
        other => panic!("unsupported C ABI value type {}", other.name()),
    }
}

fn sanitize_identifier(value: &str) -> String {
    let mut ret = String::with_capacity(value.len().max(1));
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            ret.push(ch);
        } else {
            ret.push('_');
        }
    }
    if ret.is_empty() {
        ret.push_str("module");
    }
    if ret.as_bytes()[0].is_ascii_digit() {
        ret.insert(0, '_');
    }
    ret
}

#[allow(dead_code)]
fn comma_hex_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "0x00".to_string();
    }
    bytes
        .iter()
        .map(|byte| format!("0x{byte:02x}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn append_path_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = OsString::from(path.as_os_str());
    os.push(suffix);
    PathBuf::from(os)
}

fn align_to_16(bytes: &mut Vec<u8>) {
    let padding = (16 - (bytes.len() & 15)) & 15;
    bytes.resize(bytes.len() + padding, 0);
}

fn cc_path() -> String {
    std::env::var("CC").unwrap_or_else(|_| "cc".to_string())
}

fn compile_c_object(source: &Path, output: &Path) -> Result<(), NativeLinkError> {
    let cc = cc_path();
    let mut command = Command::new(cc);
    command
        .arg("-std=c11")
        .arg("-O2")
        .arg("-fno-pie")
        .arg("-c")
        .arg(source)
        .arg("-o")
        .arg(output);
    run_command(&mut command, "compile C ABI wrappers")
}

fn compile_assembly_object(source: &Path, output: &Path) -> Result<(), NativeLinkError> {
    let cc = cc_path();
    let mut command = Command::new(cc);
    command
        .arg("-c")
        .arg("-x")
        .arg("assembler")
        .arg(source)
        .arg("-o")
        .arg(output);
    run_command(&mut command, "compile amd64 entrypoint")
}

fn run_command(command: &mut Command, description: &str) -> Result<(), NativeLinkError> {
    let output = command.output().map_err(io_err)?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(NativeLinkError::new(format!(
        "{description} failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status, stdout, stderr
    )))
}

fn io_err(err: std::io::Error) -> NativeLinkError {
    NativeLinkError::new(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::{link_native_executable, NativeLinkModule};
    use std::{
        fs,
        path::PathBuf,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use razero_decoder::decoder::decode_module;
    use razero_features::CoreFeatures;
    use razero_wasm::{
        engine::Engine as WasmEngine,
        module::{Code, Module, ValueType},
    };

    use crate::engine::CompilerEngine;

    const HELLO_HOST_WASM: &[u8] =
        include_bytes!("../../examples/hello-host/testdata/hello_world.wasm");

    fn function_type(
        params: &[ValueType],
        results: &[ValueType],
    ) -> razero_wasm::module::FunctionType {
        let mut ty = razero_wasm::module::FunctionType::default();
        ty.params = params.to_vec();
        ty.results = results.to_vec();
        ty.cache_num_in_u64();
        ty
    }

    fn command_exists(name: &str) -> bool {
        Command::new(name).arg("--version").output().is_ok()
    }

    fn test_workspace(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("{name}-{unique}"))
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn link_hello_host_executable_runs_example_guest() {
        if !command_exists("cc") {
            return;
        }

        let workspace = test_workspace("package-link-driver-hello-host");
        fs::create_dir_all(&workspace).unwrap();

        let mut module = decode_module(HELLO_HOST_WASM, CoreFeatures::V2).unwrap();
        module.build_memory_definitions();

        let mut engine = CompilerEngine::new();
        engine.compile_module(&module).unwrap();
        let artifact = engine
            .compiled_module(&module)
            .unwrap()
            .emit_relocatable_object()
            .unwrap();

        let output = super::link_hello_host_executable(
            workspace.join("hello-host-native"),
            &NativeLinkModule::from_artifact("hello-host", artifact),
            &[],
        )
        .unwrap();
        let run = Command::new(&output).output().unwrap();

        assert!(
            run.status.success(),
            "status: {:?}\nstdout:\n{}\nstderr:\n{}",
            run.status,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(
            String::from_utf8(run.stdout).unwrap(),
            "hello world from guest"
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn link_native_executable_packages_metadata_and_runs_cabi_wrapper() {
        if !command_exists("cc") || !command_exists("ar") {
            return;
        }

        let workspace = test_workspace("package-link-driver");
        fs::create_dir_all(&workspace).unwrap();

        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut engine = CompilerEngine::new();
        engine.compile_module(&module).unwrap();
        let artifact = engine
            .compiled_module(&module)
            .unwrap()
            .emit_relocatable_object()
            .unwrap();

        let main_c = workspace.join("main.c");
        fs::write(
            &main_c,
            r#"#include <stdint.h>

extern int32_t razero_cabi_guest_function_0(int32_t);

int main(void) {
    return razero_cabi_guest_function_0(41) == 42 ? 0 : 1;
}
"#,
        )
        .unwrap();
        let main_o = workspace.join("main.o");
        let libmain = workspace.join("libmain.a");
        let status = Command::new("cc")
            .arg("-std=c11")
            .arg("-O2")
            .arg("-fno-pie")
            .arg("-c")
            .arg(&main_c)
            .arg("-o")
            .arg(&main_o)
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("ar")
            .arg("rcs")
            .arg(&libmain)
            .arg(&main_o)
            .status()
            .unwrap();
        assert!(status.success());

        let package = link_native_executable(
            workspace.join("guest-bin"),
            &[NativeLinkModule::from_artifact("guest", artifact)],
            &[libmain.clone()],
        )
        .unwrap();

        assert_eq!(1, package.cabi_exports.len());
        assert_eq!(
            "razero_cabi_guest_function_0",
            package.cabi_exports[0].symbol_name
        );

        let metadata_bundle = fs::read(&package.metadata_bundle_path).unwrap();
        assert!(metadata_bundle.starts_with(b"RZPKG001"));

        let status = Command::new(&package.executable_path).status().unwrap();
        assert!(status.success());

        fs::remove_dir_all(workspace).unwrap();
    }
}
