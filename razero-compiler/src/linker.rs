#![doc = "Native Linux/ELF executable packaging and link helpers.\n\nThe versioned Rust AOT packaging contract is documented in `../AOT_PACKAGING_ABI.md`."]

use std::{
    ffi::OsString,
    fs,
    io::{Cursor, Read},
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

#[cfg(target_arch = "x86_64")]
use crate::backend::isa::amd64::abi_entry::{
    entry_asm_source as native_entry_asm_source,
    AFTER_HOST_CALL_ENTRYPOINT_SYMBOL as NATIVE_AFTER_HOST_CALL_ENTRYPOINT_SYMBOL,
    ENTRYPOINT_SYMBOL as NATIVE_ENTRYPOINT_SYMBOL,
};
#[cfg(target_arch = "x86_64")]
use crate::backend::isa::amd64::machine::Amd64Machine as NativeMachine;
#[cfg(target_arch = "aarch64")]
use crate::backend::isa::arm64::abi_entry::{
    entry_asm_source as native_entry_asm_source,
    AFTER_HOST_CALL_ENTRYPOINT_SYMBOL as NATIVE_AFTER_HOST_CALL_ENTRYPOINT_SYMBOL,
    ENTRYPOINT_SYMBOL as NATIVE_ENTRYPOINT_SYMBOL,
};
#[cfg(target_arch = "aarch64")]
use crate::backend::isa::arm64::machine::Arm64Machine as NativeMachine;
use crate::{
    aot::{
        deserialize_aot_metadata, AotCompiledMetadata, AotDataSegmentMetadata,
        AotFunctionTypeMetadata, AotImportDescMetadata, AotImportMetadata, AotTargetArchitecture,
        AotTargetOperatingSystem,
    },
    backend::machine::Machine,
    engine::{linked_wasm_function_symbol_name, RelocatableObjectArtifact},
    frontend::signature_for_wasm_function_type,
    runtime_state::{build_linked_runtime_plan, LinkedRuntimePlan},
    wazevoapi::{offsetdata::FUNCTION_INSTANCE_SIZE, ExitCode, EXIT_CODE_MASK},
};

/// Magic header for `.razero-package` metadata bundles emitted by [`link_native_executable`].
pub const NATIVE_PACKAGE_MAGIC: &[u8; 8] = b"RZPKG001";

/// Stable metadata entry stored in a `.razero-package` bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativePackageMetadataEntry {
    pub module_name: String,
    pub metadata_sidecar_bytes: Vec<u8>,
}

/// Stable packaged host-function descriptor stored in a `.razero-package` bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackagedHostImportDescriptor {
    pub guest_module_name: String,
    pub import_module: String,
    pub import_name: String,
    pub function_import_index: Index,
    pub type_index: Index,
    pub host_symbol_name: String,
}

impl PackagedHostImportDescriptor {
    pub fn from_import_metadata(
        guest_module_name: impl Into<String>,
        import: &AotImportMetadata,
        host_symbol_name: impl Into<String>,
    ) -> Result<Self, NativeLinkError> {
        let AotImportDescMetadata::Func(type_index) = &import.desc else {
            return Err(NativeLinkError::new(format!(
                "import '{}.{}' is not a function import",
                import.module, import.name
            )));
        };
        Ok(Self {
            guest_module_name: guest_module_name.into(),
            import_module: import.module.clone(),
            import_name: import.name.clone(),
            function_import_index: import.index_per_type,
            type_index: *type_index,
            host_symbol_name: host_symbol_name.into(),
        })
    }
}

/// Stable `.razero-package` bundle schema for linked native executables.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativePackageMetadataBundle {
    pub modules: Vec<NativePackageMetadataEntry>,
    pub host_imports: Vec<PackagedHostImportDescriptor>,
}

/// Error returned when decoding or validating `.razero-package` metadata bundles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativePackageMetadataError {
    message: String,
}

impl NativePackageMetadataError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for NativePackageMetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NativePackageMetadataError {}

/// Object file plus metadata sidecar bytes that participate in native packaging.
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

/// C ABI symbol exported by [`link_native_executable`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeCAbiExport {
    pub module_name: String,
    pub wasm_function_index: Index,
    pub symbol_name: String,
    pub params: Vec<ValueType>,
    pub results: Vec<ValueType>,
}

/// Result of packaging one or more linked native modules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeExecutablePackage {
    pub executable_path: PathBuf,
    pub metadata_bundle_path: PathBuf,
    pub cabi_exports: Vec<NativeCAbiExport>,
}

/// Error returned when object emission, metadata validation, or external linking fails.
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativePackagingTarget {
    architecture: AotTargetArchitecture,
    object_architecture: ObjectArchitecture,
    entrypoint_symbol: &'static str,
    after_host_call_entrypoint_symbol: &'static str,
    entry_asm_source: &'static str,
    trim_void_preamble_prologue: bool,
}

fn current_native_packaging_target() -> Result<NativePackagingTarget, NativeLinkError> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Ok(NativePackagingTarget {
            architecture: AotTargetArchitecture::X86_64,
            object_architecture: ObjectArchitecture::X86_64,
            entrypoint_symbol: NATIVE_ENTRYPOINT_SYMBOL,
            after_host_call_entrypoint_symbol: NATIVE_AFTER_HOST_CALL_ENTRYPOINT_SYMBOL,
            entry_asm_source: native_entry_asm_source(),
            trim_void_preamble_prologue: true,
        });
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return Ok(NativePackagingTarget {
            architecture: AotTargetArchitecture::Aarch64,
            object_architecture: ObjectArchitecture::Aarch64,
            entrypoint_symbol: NATIVE_ENTRYPOINT_SYMBOL,
            after_host_call_entrypoint_symbol: NATIVE_AFTER_HOST_CALL_ENTRYPOINT_SYMBOL,
            entry_asm_source: native_entry_asm_source(),
            trim_void_preamble_prologue: false,
        });
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    )))]
    {
        Err(NativeLinkError::new(
            "native executable packaging currently requires a Linux/x86_64 or Linux/aarch64 host",
        ))
    }
}

fn native_compile_entry_preamble(ty: &AotFunctionTypeMetadata) -> Result<Vec<u8>, NativeLinkError> {
    let mut function_type = FunctionType::default();
    function_type.params = ty.params.clone();
    function_type.results = ty.results.clone();
    function_type.param_num_in_u64 = ty.param_num_in_u64;
    function_type.result_num_in_u64 = ty.result_num_in_u64;
    let signature = signature_for_wasm_function_type(&function_type);
    let mut machine = NativeMachine::new();
    Ok(machine.compile_entry_preamble(&signature, false))
}

fn native_compile_host_trampoline(
    ty: &AotFunctionTypeMetadata,
    exit_code: ExitCode,
) -> Result<Vec<u8>, NativeLinkError> {
    let mut function_type = FunctionType::default();
    function_type.params = ty.params.clone();
    function_type.results = ty.results.clone();
    function_type.param_num_in_u64 = ty.param_num_in_u64;
    function_type.result_num_in_u64 = ty.result_num_in_u64;
    let signature = signature_for_wasm_function_type(&function_type);
    let mut machine = NativeMachine::new();
    Ok(machine.compile_host_function_trampoline(exit_code, &signature, true))
}

/// Serializes the stable `.razero-package` metadata bundle written beside native executables.
pub fn serialize_native_package_metadata_bundle(bundle: &NativePackageMetadataBundle) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(NATIVE_PACKAGE_MAGIC);
    bytes.extend_from_slice(&(bundle.modules.len() as u32).to_le_bytes());
    for module in &bundle.modules {
        let name_bytes = module.module_name.as_bytes();
        bytes.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(name_bytes);
        bytes.extend_from_slice(&(module.metadata_sidecar_bytes.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&module.metadata_sidecar_bytes);
    }
    bytes.extend_from_slice(&(bundle.host_imports.len() as u32).to_le_bytes());
    for host_import in &bundle.host_imports {
        write_string(&mut bytes, &host_import.guest_module_name);
        write_string(&mut bytes, &host_import.import_module);
        write_string(&mut bytes, &host_import.import_name);
        bytes.extend_from_slice(&host_import.function_import_index.to_le_bytes());
        bytes.extend_from_slice(&host_import.type_index.to_le_bytes());
        write_string(&mut bytes, &host_import.host_symbol_name);
    }
    bytes
}

/// Decodes the stable `.razero-package` metadata bundle.
pub fn deserialize_native_package_metadata_bundle(
    bytes: &[u8],
) -> Result<NativePackageMetadataBundle, NativePackageMetadataError> {
    let mut cursor = Cursor::new(bytes);
    let mut magic = [0u8; 8];
    read_exact(
        &mut cursor,
        &mut magic,
        "native package metadata: invalid header length",
    )?;
    if &magic != NATIVE_PACKAGE_MAGIC {
        return Err(NativePackageMetadataError::new(
            "native package metadata: invalid magic number",
        ));
    }
    let module_count = read_u32(&mut cursor)? as usize;
    let mut modules = Vec::with_capacity(module_count);
    for _ in 0..module_count {
        let module_name = read_string(&mut cursor, "native package metadata: invalid module name")?;
        let metadata_len = read_u64(&mut cursor)? as usize;
        let mut metadata_sidecar_bytes = vec![0; metadata_len];
        read_exact(
            &mut cursor,
            &mut metadata_sidecar_bytes,
            "native package metadata: invalid module sidecar",
        )?;
        modules.push(NativePackageMetadataEntry {
            module_name,
            metadata_sidecar_bytes,
        });
    }
    let host_imports = if cursor_remaining(&cursor) == 0 {
        Vec::new()
    } else {
        let host_import_count = read_u32(&mut cursor)? as usize;
        let mut host_imports = Vec::with_capacity(host_import_count);
        for _ in 0..host_import_count {
            host_imports.push(PackagedHostImportDescriptor {
                guest_module_name: read_string(
                    &mut cursor,
                    "native package metadata: invalid guest module name",
                )?,
                import_module: read_string(
                    &mut cursor,
                    "native package metadata: invalid import module",
                )?,
                import_name: read_string(
                    &mut cursor,
                    "native package metadata: invalid import name",
                )?,
                function_import_index: read_u32(&mut cursor)?,
                type_index: read_u32(&mut cursor)?,
                host_symbol_name: read_string(
                    &mut cursor,
                    "native package metadata: invalid host symbol name",
                )?,
            });
        }
        host_imports
    };
    if cursor_remaining(&cursor) != 0 {
        return Err(NativePackageMetadataError::new(
            "native package metadata: unexpected trailing bytes",
        ));
    }
    Ok(NativePackageMetadataBundle {
        modules,
        host_imports,
    })
}

/// Links one or more native Linux/ELF relocatable Wasm modules into a native executable.
///
/// This stable packaging surface currently targets scalar C ABI-compatible exported functions.
/// The linked executable is accompanied by a `.razero-package` file containing only module names
/// and serialized AOT metadata sidecars; object bytes and static libraries remain external inputs.
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
    let target = current_native_packaging_target()?;

    let output_path = output_path.as_ref();
    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(io_err)?;
    let work_dir = append_path_suffix(output_path, ".razero-link");
    if work_dir.exists() {
        fs::remove_dir_all(&work_dir).map_err(io_err)?;
    }
    fs::create_dir_all(&work_dir).map_err(io_err)?;

    let metadata_bundle_path = append_path_suffix(output_path, ".razero-package");
    let mut metadata_bundle = NativePackageMetadataBundle::default();

    let mut object_paths = Vec::with_capacity(modules.len());
    let mut module_specs = Vec::with_capacity(modules.len());
    let mut cabi_exports = Vec::new();
    let mut execution_context_size = 0usize;

    for (index, module) in modules.iter().enumerate() {
        let metadata = deserialize_aot_metadata(&module.metadata_sidecar_bytes)
            .map_err(|err| NativeLinkError::new(err.to_string()))?;
        validate_metadata_support(&module.name, &metadata, target)?;
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

        metadata_bundle.modules.push(NativePackageMetadataEntry {
            module_name: module.name.clone(),
            metadata_sidecar_bytes: module.metadata_sidecar_bytes.clone(),
        });
    }

    fs::write(
        &metadata_bundle_path,
        serialize_native_package_metadata_bundle(&metadata_bundle),
    )
    .map_err(io_err)?;

    let preamble_object_bytes = build_preamble_object(&module_specs, target)?;
    let preamble_object_path = work_dir.join("razero-cabi-preambles.o");
    fs::write(&preamble_object_path, preamble_object_bytes).map_err(io_err)?;

    let wrappers_source = build_wrapper_source(&module_specs, execution_context_size, target)?;
    let wrappers_source_path = work_dir.join("razero-cabi-wrappers.c");
    let wrappers_object_path = work_dir.join("razero-cabi-wrappers.o");
    fs::write(&wrappers_source_path, wrappers_source).map_err(io_err)?;
    compile_c_object(&wrappers_source_path, &wrappers_object_path)?;

    let entry_stem = match target.architecture {
        AotTargetArchitecture::X86_64 => "razero-amd64-entry",
        AotTargetArchitecture::Aarch64 => "razero-arm64-entry",
        AotTargetArchitecture::Unknown => "razero-entry",
    };
    let entry_source_path = work_dir.join(format!("{entry_stem}.S"));
    let entry_object_path = work_dir.join(format!("{entry_stem}.o"));
    fs::write(&entry_source_path, target.entry_asm_source).map_err(io_err)?;
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

/// Specialized native-link flow for the `examples/hello-host` guest.
///
/// The example-specific `(i32, i32) -> ()` print-style behavior stays explicit, but it now rides
/// on top of reusable packaged host-import descriptors and generic host trampolines.
pub fn link_hello_host_executable(
    output_path: impl AsRef<Path>,
    guest: &NativeLinkModule,
    static_libraries: &[PathBuf],
) -> Result<PathBuf, NativeLinkError> {
    let target = current_native_packaging_target()?;
    let metadata = deserialize_aot_metadata(&guest.metadata_sidecar_bytes)
        .map_err(|err| NativeLinkError::new(err.to_string()))?;
    let hello_host = HelloHostSpec::from_metadata(&guest.name, &metadata)?;

    let output_path = output_path.as_ref();
    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(io_err)?;
    let work_dir = append_path_suffix(output_path, ".razero-link");
    if work_dir.exists() {
        fs::remove_dir_all(&work_dir).map_err(io_err)?;
    }
    fs::create_dir_all(&work_dir).map_err(io_err)?;

    let metadata_bundle_path = append_path_suffix(output_path, ".razero-package");
    fs::write(
        &metadata_bundle_path,
        serialize_native_package_metadata_bundle(&NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: guest.name.clone(),
                metadata_sidecar_bytes: guest.metadata_sidecar_bytes.clone(),
            }],
            host_imports: hello_host
                .host_imports
                .iter()
                .map(|import| import.descriptor.clone())
                .collect(),
        }),
    )
    .map_err(io_err)?;

    let guest_object_path = work_dir.join("hello-host-guest.o");
    fs::write(&guest_object_path, &guest.object_bytes).map_err(io_err)?;

    let preamble_symbol = "razero_hello_host_run_preamble";
    let preamble_object_path = work_dir.join("hello-host-preamble.o");
    fs::write(
        &preamble_object_path,
        build_named_preamble_object(preamble_symbol, &hello_host.run_type, target)?,
    )
    .map_err(io_err)?;

    let entry_stem = match target.architecture {
        AotTargetArchitecture::X86_64 => "razero-amd64-entry",
        AotTargetArchitecture::Aarch64 => "razero-arm64-entry",
        AotTargetArchitecture::Unknown => "razero-entry",
    };
    let entry_source_path = work_dir.join(format!("{entry_stem}.S"));
    let entry_object_path = work_dir.join(format!("{entry_stem}.o"));
    fs::write(&entry_source_path, target.entry_asm_source).map_err(io_err)?;
    compile_assembly_object(&entry_source_path, &entry_object_path)?;

    let wrapper_source_path = work_dir.join("hello-host-main.c");
    let wrapper_object_path = work_dir.join("hello-host-main.o");
    fs::write(
        &wrapper_source_path,
        build_hello_host_source(&metadata, &hello_host, preamble_symbol, target),
    )
    .map_err(io_err)?;
    compile_c_object(&wrapper_source_path, &wrapper_object_path)?;

    let host_import_object_path = work_dir.join("hello-host-import.o");
    fs::write(
        &host_import_object_path,
        build_host_import_object(&hello_host.host_imports, target)?,
    )
    .map_err(io_err)?;

    let hello_host_handler_source_path = work_dir.join("hello-host-handler.c");
    let hello_host_handler_object_path = work_dir.join("hello-host-handler.o");
    fs::write(
        &hello_host_handler_source_path,
        build_hello_host_handler_source(&hello_host.host_imports[0].descriptor.host_symbol_name),
    )
    .map_err(io_err)?;
    compile_c_object(
        &hello_host_handler_source_path,
        &hello_host_handler_object_path,
    )?;

    let cc = cc_path();
    let mut link = Command::new(&cc);
    link.arg("-no-pie")
        .arg("-o")
        .arg(output_path)
        .arg(&wrapper_object_path)
        .arg(&entry_object_path)
        .arg(&preamble_object_path)
        .arg(&host_import_object_path)
        .arg(&hello_host_handler_object_path)
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
    run_symbol: String,
    run_type: AotFunctionTypeMetadata,
    memory_len_bytes: usize,
    data_segments: Vec<HelloHostDataSegment>,
    host_imports: Vec<PackagedHostImportSpec>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct HelloHostDataSegment {
    offset: usize,
    init: Vec<u8>,
}

#[derive(Clone, Debug)]
struct PackagedHostImportSpec {
    descriptor: PackagedHostImportDescriptor,
    import_type: AotFunctionTypeMetadata,
    trampoline_symbol: String,
}

impl HelloHostSpec {
    fn from_metadata(
        guest_module_name: &str,
        metadata: &AotCompiledMetadata,
    ) -> Result<Self, NativeLinkError> {
        validate_hello_host_metadata(metadata)?;
        let host_import = metadata
            .imports
            .iter()
            .find(|import| matches!(&import.desc, AotImportDescMetadata::Func(_)))
            .ok_or_else(|| {
                NativeLinkError::new("hello-host guest must import one host function")
            })?;
        let AotImportDescMetadata::Func(import_type_index) = &host_import.desc else {
            unreachable!()
        };
        let import_type = metadata
            .types
            .get(*import_type_index as usize)
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
            run_symbol: linked_wasm_function_symbol_name(&metadata.module_id, run_export.index),
            run_type,
            memory_len_bytes,
            data_segments,
            host_imports: vec![PackagedHostImportSpec {
                descriptor: PackagedHostImportDescriptor::from_import_metadata(
                    guest_module_name,
                    host_import,
                    &hello_host_handler_symbol_name(&host_import.module, &host_import.name),
                )?,
                import_type,
                trampoline_symbol: packaged_host_import_symbol_name(
                    host_import.index_per_type,
                    &host_import.module,
                    &host_import.name,
                ),
            }],
        })
    }
}

fn validate_metadata_support(
    module_name: &str,
    metadata: &AotCompiledMetadata,
    target: NativePackagingTarget,
) -> Result<(), NativeLinkError> {
    if metadata.target.operating_system != AotTargetOperatingSystem::Linux {
        return Err(NativeLinkError::new(format!(
            "module '{module_name}' targets {:?}, only linux is supported",
            metadata.target.operating_system
        )));
    }
    if metadata.target.architecture != target.architecture {
        return Err(NativeLinkError::new(format!(
            "module '{module_name}' targets {}, but this linker instance packages {} artifacts",
            metadata.target.architecture.name(),
            target.architecture.name()
        )));
    }
    build_linked_runtime_plan(metadata).map_err(|err| {
        NativeLinkError::new(format!(
            "module '{module_name}' is outside the packaged linked-runtime slice: {err}"
        ))
    })?;
    Ok(())
}

fn validate_host_import_metadata(
    metadata: &AotCompiledMetadata,
    host_import_count: usize,
    target: NativePackagingTarget,
) -> Result<(), NativeLinkError> {
    if metadata.target.operating_system != AotTargetOperatingSystem::Linux
        || metadata.target.architecture != target.architecture
    {
        return Err(NativeLinkError::new(
            "packaged host-import linker only supports Linux artifacts matching the native linker architecture",
        ));
    }
    if metadata.module_shape.import_function_count != host_import_count as u32
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
            "packaged host-import linker currently expects explicit function imports, one local memory, no globals/tables/start, and no element segments",
        ));
    }
    if metadata
        .imports
        .iter()
        .filter(|import| matches!(&import.desc, AotImportDescMetadata::Func(_)))
        .count()
        != host_import_count
    {
        return Err(NativeLinkError::new(
            "packaged host-import linker expected a descriptor for every imported function",
        ));
    }
    Ok(())
}

fn validate_hello_host_metadata(metadata: &AotCompiledMetadata) -> Result<(), NativeLinkError> {
    validate_host_import_metadata(metadata, 1, current_native_packaging_target()?)?;
    let host_import = metadata
        .imports
        .iter()
        .find(|import| matches!(&import.desc, AotImportDescMetadata::Func(_)))
        .ok_or_else(|| NativeLinkError::new("hello-host linker could not find the host import"))?;
    let AotImportDescMetadata::Func(type_index) = &host_import.desc else {
        unreachable!()
    };
    let host_import_type = metadata
        .types
        .get(*type_index as usize)
        .ok_or_else(|| NativeLinkError::new("hello-host import type metadata is missing"))?;
    if host_import_type.params != [ValueType::I32, ValueType::I32]
        || !host_import_type.results.is_empty()
    {
        return Err(NativeLinkError::new(
            "hello-host linker expects its host import to use the (i32, i32) -> () signature",
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
        |_index| unreachable!("packaged host-import data offsets must not read globals"),
        |_index| unreachable!("packaged host-import data offsets must not use ref.func"),
    )
    .map_err(|err| NativeLinkError::new(err.to_string()))?;
    match ty {
        ValueType::I32 | ValueType::I64 => Ok(values.first().copied().unwrap_or_default() as usize),
        _ => Err(NativeLinkError::new(
            "packaged host-import data offsets must evaluate to i32/i64",
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

fn build_preamble_object(
    module_specs: &[ModuleSpec],
    target: NativePackagingTarget,
) -> Result<Vec<u8>, NativeLinkError> {
    let mut text = Vec::new();
    let mut symbols = Vec::new();
    for module in module_specs {
        for (type_index, ty) in module.metadata.types.iter().enumerate() {
            if !function_type_supported(ty) {
                continue;
            }
            align_to_16(&mut text);
            let offset = text.len() as u64;
            let bytes = native_compile_entry_preamble(ty)?;
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
        target.object_architecture,
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
    target: NativePackagingTarget,
) -> Result<Vec<u8>, NativeLinkError> {
    let mut bytes = native_compile_entry_preamble(ty)?;
    if target.trim_void_preamble_prologue
        && ty.params.is_empty()
        && ty.results.is_empty()
        && bytes.starts_with(&[0x55, 0x48, 0x89, 0xe5])
    {
        bytes.drain(..4);
    }

    let mut object = ElfObject::new(
        BinaryFormat::Elf,
        target.object_architecture,
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
    host_imports: &[PackagedHostImportSpec],
    target: NativePackagingTarget,
) -> Result<Vec<u8>, NativeLinkError> {
    let mut text = Vec::new();
    let mut symbols = Vec::with_capacity(host_imports.len());
    for host_import in host_imports {
        align_to_16(&mut text);
        let offset = text.len() as u64;
        let bytes = native_compile_host_trampoline(
            &host_import.import_type,
            ExitCode::call_go_function_with_index(
                host_import.descriptor.function_import_index as usize,
                false,
            ),
        )?;
        text.extend_from_slice(&bytes);
        symbols.push((
            host_import.trampoline_symbol.clone(),
            offset,
            bytes.len() as u64,
        ));
    }

    let mut object = ElfObject::new(
        BinaryFormat::Elf,
        target.object_architecture,
        Endianness::Little,
    );
    object.add_file_symbol(b"razero-packaged-host-imports".to_vec());
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
fn build_hello_host_source(
    metadata: &AotCompiledMetadata,
    hello_host: &HelloHostSpec,
    preamble_symbol: &str,
    target: NativePackagingTarget,
) -> String {
    let mut source = String::new();
    source.push_str("#include <stdint.h>\n#include <stdio.h>\n#include <string.h>\n\n");
    source.push_str(&format!(
        "extern void {symbol}(const unsigned char*, const unsigned char*, uintptr_t, const unsigned char*, uint64_t*, uintptr_t);\n",
        symbol = target.entrypoint_symbol,
    ));
    source.push_str(&format!(
        "extern void {symbol}(const unsigned char*, uintptr_t, uintptr_t, uintptr_t);\n",
        symbol = target.after_host_call_entrypoint_symbol,
    ));
    source.push_str(&format!(
        "extern void {}(void);\nextern const unsigned char {}[];\n",
        hello_host.run_symbol, preamble_symbol
    ));
    for host_import in &hello_host.host_imports {
        source.push_str(&format!(
            "extern const unsigned char {}[];\nextern int {}(unsigned char*, unsigned char*, size_t, uint64_t*, size_t);\n",
            host_import.trampoline_symbol, host_import.descriptor.host_symbol_name
        ));
    }
    source.push('\n');
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
    for host_import in &hello_host.host_imports {
        let import_base = metadata.module_context.imported_functions_begin
            + FUNCTION_INSTANCE_SIZE.raw() * host_import.descriptor.function_import_index as i32;
        source.push_str(&format!(
            "    store_u64(guest_module_ctx.bytes, {import_base}, (uint64_t)(uintptr_t){trampoline});\n    store_u64(guest_module_ctx.bytes, {import_ctx}, 0);\n    store_u64(guest_module_ctx.bytes, {import_type}, 0);\n",
            trampoline = host_import.trampoline_symbol,
            import_ctx = import_base + 8,
            import_type = import_base + 16
        ));
    }
    source.push_str(&format!(
        "    {entrypoint_symbol}({preamble_symbol}, (const unsigned char*)&{run_symbol}, (uintptr_t)&exec_ctx, guest_module_ctx.bytes, param_result, stack_top);\n    for (;;) {{\n        uint32_t exit_code = read_u32(exec_ctx.bytes, {exit_code_offset});\n        switch (exit_code & RAZERO_EXIT_MASK) {{\n            case RAZERO_EXIT_OK:\n                return 0;\n            case RAZERO_EXIT_CALL_GO_FUNCTION: {{\n                uintptr_t go_stack_ptr = (uintptr_t)read_u64(exec_ctx.bytes, {go_stack_offset});\n                uintptr_t return_address = (uintptr_t)read_u64(exec_ctx.bytes, {return_offset});\n                uintptr_t frame_pointer = (uintptr_t)read_u64(exec_ctx.bytes, {frame_offset});\n                uint64_t* words = (uint64_t*)go_stack_ptr;\n                size_t stack_word_count = (size_t)(words[0] / 8u);\n                uint64_t* stack_words = words + 1;\n                switch (exit_code >> 8) {{\n{host_dispatch_cases}                    default:\n                        fprintf(stderr, \"unexpected host function index %u\\n\", exit_code >> 8);\n                        return 3;\n                }}\n                store_u32(exec_ctx.bytes, {exit_code_offset}, RAZERO_EXIT_OK);\n                {after_host_call_symbol}((const unsigned char*)return_address, (uintptr_t)&exec_ctx, go_stack_ptr, frame_pointer);\n                break;\n            }}\n            default:\n                fprintf(stderr, \"unsupported exit code %u while running packaged host-import guest\\n\", exit_code);\n                return 6;\n        }}\n    }}\n}}\n",
        run_symbol = hello_host.run_symbol,
        entrypoint_symbol = target.entrypoint_symbol,
        after_host_call_symbol = target.after_host_call_entrypoint_symbol,
        exit_code_offset = metadata.execution_context.exit_code_offset,
        go_stack_offset = metadata.execution_context.stack_pointer_before_go_call_offset,
        return_offset = metadata.execution_context.go_call_return_address_offset,
        frame_offset = metadata.execution_context.frame_pointer_before_go_call_offset,
        host_dispatch_cases = hello_host
            .host_imports
            .iter()
            .map(|host_import| host_dispatch_case_source(host_import, hello_host.memory_len_bytes.max(1)))
            .collect::<Vec<_>>()
            .join("")
    ));
    source
}

fn host_dispatch_case_source(host_import: &PackagedHostImportSpec, memory_len: usize) -> String {
    let slot_count = host_import
        .import_type
        .param_num_in_u64
        .max(host_import.import_type.result_num_in_u64)
        .max(1);
    format!(
        "                    case {index}u:\n                        if (stack_word_count < {slot_count}u) {{ fprintf(stderr, \"host import {module}.{name} stack is too small\\n\"); return 4; }}\n                        if ({host_symbol}(guest_module_ctx.bytes, guest_memory, (size_t){memory_len}, stack_words, stack_word_count) != 0) {{ fprintf(stderr, \"host import {module}.{name} failed\\n\"); return 5; }}\n                        break;\n",
        index = host_import.descriptor.function_import_index,
        slot_count = slot_count,
        host_symbol = host_import.descriptor.host_symbol_name,
        module = host_import.descriptor.import_module,
        name = host_import.descriptor.import_name,
        memory_len = memory_len,
    )
}

fn build_hello_host_handler_source(symbol_name: &str) -> String {
    format!(
        "#include <stdint.h>\n#include <stddef.h>\n#include <stdio.h>\n\nint {symbol_name}(unsigned char* module_context, unsigned char* guest_memory, size_t guest_memory_len, uint64_t* stack_words, size_t stack_word_count) {{\n    (void)module_context;\n    if (stack_word_count < 2u) {{ return 1; }}\n    uint32_t ptr = (uint32_t)stack_words[0];\n    uint32_t len = (uint32_t)stack_words[1];\n    if ((uint64_t)ptr + (uint64_t)len > (uint64_t)guest_memory_len) {{ return 2; }}\n    fwrite(guest_memory + ptr, 1u, len, stdout);\n    fflush(stdout);\n    return 0;\n}}\n"
    )
}

fn hello_host_handler_symbol_name(import_module: &str, import_name: &str) -> String {
    format!(
        "razero_hello_host_{}_{}_handler",
        sanitize_identifier(import_module),
        sanitize_identifier(import_name)
    )
}

fn packaged_host_import_symbol_name(
    function_import_index: Index,
    import_module: &str,
    import_name: &str,
) -> String {
    format!(
        "razero_import_function_{}_{}_{}",
        function_import_index,
        sanitize_identifier(import_module),
        sanitize_identifier(import_name)
    )
}

fn build_wrapper_source(
    module_specs: &[ModuleSpec],
    execution_context_size: usize,
    target: NativePackagingTarget,
) -> Result<String, NativeLinkError> {
    let mut source = String::new();
    source.push_str("#include <stdint.h>\n#include <stddef.h>\n#include <string.h>\n\n");
    source.push_str(&format!(
        "extern void {symbol}(const unsigned char*, const unsigned char*, uintptr_t, const unsigned char*, uint64_t*, uintptr_t);\n\n",
        symbol = target.entrypoint_symbol,
    ));
    source.push_str(&format!(
        "typedef struct __attribute__((aligned(16))) {{ unsigned char bytes[{execution_context_size}]; }} razero_link_exec_ctx_t;\n\n"
    ));
    source.push_str(
        "typedef struct { uintptr_t executable_ptr; uintptr_t module_context_ptr; uint32_t type_id; uint32_t reserved; } razero_link_function_instance_t;\n",
    );
    source.push_str(
        "typedef struct { const uintptr_t* base_address; uint32_t len; uint32_t reserved; } razero_link_table_t;\n\n",
    );
    source.push_str(
        "static void store_u64(unsigned char* base, uintptr_t offset, uint64_t value) {\n    memcpy(base + offset, &value, sizeof(value));\n}\n\n",
    );

    for module in module_specs {
        let runtime_plan = build_linked_runtime_plan(&module.metadata)
            .map_err(|err| NativeLinkError::new(err.to_string()))?;
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
        }
        source.push_str(&build_module_runtime_source(module, &runtime_plan, target)?);
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
            source.push_str(&wrapper_function_source(
                &module.sanitized_name,
                function.wasm_function_index,
                ty,
                &raw_symbol,
                &preamble_symbol,
                &module_init_function_name(&module.sanitized_name),
                &module_context_name(&module.sanitized_name),
                target,
            ));
            source.push('\n');
        }
    }
    Ok(source)
}

fn wrapper_function_source(
    module_name: &str,
    wasm_function_index: Index,
    ty: &AotFunctionTypeMetadata,
    raw_symbol: &str,
    preamble_symbol: &str,
    init_function_name: &str,
    module_context_name: &str,
    target: NativePackagingTarget,
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
    source.push_str(&format!("    {init_function_name}();\n"));
    for (index, value) in ty.params.iter().enumerate() {
        source.push_str(&param_pack_source(*value, index));
    }
    source.push_str(&format!(
        "    {entrypoint_symbol}({preamble_symbol}, (const unsigned char*)&{raw_symbol}, (uintptr_t)&execution_context, {module_context_name}.bytes, param_result, (uintptr_t)go_stack);\n",
        entrypoint_symbol = target.entrypoint_symbol,
    ));
    if let Some(result) = ty.results.first().copied() {
        source.push_str(&result_unpack_source(result));
    }
    source.push_str("}\n");
    source
}

fn build_module_runtime_source(
    module: &ModuleSpec,
    runtime_plan: &LinkedRuntimePlan,
    target: NativePackagingTarget,
) -> Result<String, NativeLinkError> {
    let mut source = String::new();
    let module_context_name = module_context_name(&module.sanitized_name);
    let init_function_name = module_init_function_name(&module.sanitized_name);
    let function_instances_name = format!("{}_function_instances", module.sanitized_name);
    let tables_name = format!("{}_tables", module.sanitized_name);
    let type_ids_name = format!("{}_type_ids", module.sanitized_name);
    let initialized_name = format!("{}_initialized", module.sanitized_name);
    source.push_str(&format!(
        "static struct __attribute__((aligned(16))) {{ unsigned char bytes[{ctx_size}]; }} {module_context_name} = {{0}};\n",
        ctx_size = module.metadata.module_context.total_size.max(1)
    ));
    if let Some(memory) = runtime_plan.memory_bytes.as_ref() {
        source.push_str(&format!(
            "static unsigned char {name}[{len}] = {{{bytes}}};\n",
            name = module_memory_name(&module.sanitized_name),
            len = memory.len().max(1),
            bytes = comma_hex_bytes(memory)
        ));
    }
    if !runtime_plan.type_ids.is_empty() {
        source.push_str(&format!(
            "static const uint32_t {type_ids_name}[{len}] = {{{values}}};\n",
            len = runtime_plan.type_ids.len(),
            values = runtime_plan
                .type_ids
                .iter()
                .map(|value| format!("{value}u"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    source.push_str(&format!(
        "static razero_link_function_instance_t {function_instances_name}[{len}] = {{0}};\n",
        len = module.metadata.functions.len().max(1)
    ));
    for (table_index, table) in runtime_plan.tables.iter().enumerate() {
        source.push_str(&format!(
            "static uintptr_t {table_elements_name}[{len}] = {{{values}}};\n",
            table_elements_name = table_elements_name(&module.sanitized_name, table_index),
            len = table.elements.len().max(1),
            values = if table.elements.is_empty() {
                "0".to_string()
            } else {
                table
                    .elements
                    .iter()
                    .map(|entry| {
                        entry
                            .and_then(|wasm_function_index| {
                                module.metadata.functions.iter().position(|function| {
                                    function.wasm_function_index == wasm_function_index
                                })
                            })
                            .map(|slot| format!("(uintptr_t)&{function_instances_name}[{slot}]"))
                            .unwrap_or_else(|| "0".to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ));
    }
    source.push_str(&format!(
        "static razero_link_table_t {tables_name}[{len}] = {{0}};\n",
        len = runtime_plan.tables.len().max(1)
    ));
    source.push_str(&format!("static int {initialized_name} = 0;\n"));
    source.push_str(&format!("static void {init_function_name}(void) {{\n"));
    source.push_str(&format!("    if ({initialized_name}) return;\n"));
    source.push_str(&format!("    {initialized_name} = 1;\n"));
    for (slot, function) in module.metadata.functions.iter().enumerate() {
        let raw_symbol = linked_wasm_function_symbol_name(
            &module.metadata.module_id,
            function.wasm_function_index,
        );
        source.push_str(&format!(
            "    {function_instances_name}[{slot}].executable_ptr = (uintptr_t)&{raw_symbol};\n    {function_instances_name}[{slot}].module_context_ptr = (uintptr_t){module_context_name}.bytes;\n    {function_instances_name}[{slot}].type_id = {type_id}u;\n",
            type_id = function.type_index
        ));
    }
    for (table_index, table) in runtime_plan.tables.iter().enumerate() {
        let table_elements_name = table_elements_name(&module.sanitized_name, table_index);
        let base_expr = if table.elements.is_empty() {
            "NULL".to_string()
        } else {
            table_elements_name.clone()
        };
        source.push_str(&format!(
            "    {tables_name}[{table_index}].base_address = {base_expr};\n    {tables_name}[{table_index}].len = {len}u;\n",
            len = table.elements.len()
        ));
    }
    if module.metadata.module_context.local_memory_begin >= 0 {
        let base_expr = runtime_plan
            .memory_bytes
            .as_ref()
            .map(|_| {
                format!(
                    "(uint64_t)(uintptr_t){}",
                    module_memory_name(&module.sanitized_name)
                )
            })
            .unwrap_or_else(|| "0".to_string());
        let len = runtime_plan
            .memory_bytes
            .as_ref()
            .map(|memory| memory.len())
            .unwrap_or(0);
        source.push_str(&format!(
            "    store_u64({module_context_name}.bytes, {offset}, {base_expr});\n    store_u64({module_context_name}.bytes, {len_offset}, (uint64_t){len});\n",
            offset = module.metadata.module_context.local_memory_begin,
            len_offset = module.metadata.module_context.local_memory_begin + 8
        ));
    }
    if module.metadata.module_context.globals_begin >= 0 {
        for (index, global) in runtime_plan.globals.iter().enumerate() {
            let offset = module.metadata.module_context.globals_begin + (index as i32 * 16);
            source.push_str(&format!(
                "    store_u64({module_context_name}.bytes, {offset}, UINT64_C({value_lo}));\n    store_u64({module_context_name}.bytes, {offset_hi}, UINT64_C({value_hi}));\n",
                offset_hi = offset + 8,
                value_lo = global.value_lo,
                value_hi = global.value_hi
            ));
        }
    }
    if module.metadata.module_context.type_ids_1st_element >= 0 && !runtime_plan.type_ids.is_empty()
    {
        source.push_str(&format!(
            "    store_u64({module_context_name}.bytes, {offset}, (uint64_t)(uintptr_t){type_ids_name});\n",
            offset = module.metadata.module_context.type_ids_1st_element
        ));
    }
    if module.metadata.module_context.tables_begin >= 0 {
        for index in 0..runtime_plan.tables.len() {
            source.push_str(&format!(
                "    store_u64({module_context_name}.bytes, {offset}, (uint64_t)(uintptr_t)&{tables_name}[{index}]);\n",
                offset = module.metadata.module_context.tables_begin + (index as i32 * 8)
            ));
        }
    }
    if let Some(start_index) = module.metadata.start_function_index {
        let start_function = module
            .metadata
            .functions
            .iter()
            .find(|function| function.wasm_function_index == start_index)
            .ok_or_else(|| NativeLinkError::new("start function metadata is missing"))?;
        let start_preamble =
            preamble_symbol_name(&module.sanitized_name, start_function.type_index);
        let start_raw = linked_wasm_function_symbol_name(&module.metadata.module_id, start_index);
        source.push_str(
            "    {\n        uint64_t param_result[1] = {0};\n        razero_link_exec_ctx_t execution_context = {{0}};\n        uint64_t go_stack[4096] = {0};\n        ",
        );
        source.push_str(&format!(
            "{entrypoint_symbol}({start_preamble}, (const unsigned char*)&{start_raw}, (uintptr_t)&execution_context, {module_context_name}.bytes, param_result, (uintptr_t)go_stack);\n    }}\n",
            entrypoint_symbol = target.entrypoint_symbol,
        ));
    }
    source.push_str("}\n\n");
    Ok(source)
}

fn module_init_function_name(module_name: &str) -> String {
    format!(
        "razero_cabi_{}_initialize",
        sanitize_identifier(module_name)
    )
}

fn module_context_name(module_name: &str) -> String {
    format!(
        "razero_cabi_{}_module_ctx",
        sanitize_identifier(module_name)
    )
}

fn module_memory_name(module_name: &str) -> String {
    format!("razero_cabi_{}_memory", sanitize_identifier(module_name))
}

fn table_elements_name(module_name: &str, table_index: usize) -> String {
    format!(
        "razero_cabi_{}_table_{}_elements",
        sanitize_identifier(module_name),
        table_index
    )
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
    run_command(&mut command, "compile native entrypoint")
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

fn read_exact(
    cursor: &mut Cursor<&[u8]>,
    buf: &mut [u8],
    message: &str,
) -> Result<(), NativePackageMetadataError> {
    cursor
        .read_exact(buf)
        .map_err(|_| NativePackageMetadataError::new(message))
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> Result<u32, NativePackageMetadataError> {
    let mut bytes = [0u8; 4];
    read_exact(
        cursor,
        &mut bytes,
        "native package metadata: invalid u32 field",
    )?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> Result<u64, NativePackageMetadataError> {
    let mut bytes = [0u8; 8];
    read_exact(
        cursor,
        &mut bytes,
        "native package metadata: invalid u64 field",
    )?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_string(
    cursor: &mut Cursor<&[u8]>,
    message: &str,
) -> Result<String, NativePackageMetadataError> {
    let len = read_u32(cursor)? as usize;
    let mut bytes = vec![0; len];
    read_exact(cursor, &mut bytes, message)?;
    String::from_utf8(bytes)
        .map_err(|err| NativePackageMetadataError::new(format!("{message}: invalid UTF-8: {err}")))
}

fn write_string(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

fn cursor_remaining(cursor: &Cursor<&[u8]>) -> usize {
    cursor
        .get_ref()
        .len()
        .saturating_sub(cursor.position() as usize)
}

#[cfg(test)]
mod tests {
    use super::{
        append_path_suffix, build_wrapper_source, deserialize_native_package_metadata_bundle,
        link_native_executable, serialize_native_package_metadata_bundle,
        validate_hello_host_metadata, HelloHostSpec, ModuleSpec, NativeLinkModule,
        NativePackageMetadataBundle, NativePackageMetadataEntry, NativePackagingTarget,
        PackagedHostImportDescriptor, NATIVE_PACKAGE_MAGIC,
    };
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
        module::{
            Code, ConstExpr, DataSegment, ElementMode, ElementSegment, Export, ExternType, Global,
            GlobalType, Import, ImportDesc, Module, RefType, Table, ValueType,
        },
    };

    use crate::aot::{
        serialize_aot_metadata, AotCompiledMetadata, AotFunctionTypeMetadata, AotTarget,
        AotTargetArchitecture, AotTargetOperatingSystem,
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

    fn compile_module_metadata(module: &Module) -> AotCompiledMetadata {
        let mut engine = CompilerEngine::new();
        engine.compile_module(module).unwrap();
        let artifact = engine
            .compiled_module(module)
            .unwrap()
            .emit_relocatable_object()
            .unwrap();
        crate::aot::deserialize_aot_metadata(&artifact.metadata_sidecar_bytes).unwrap()
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

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
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
        let metadata_bundle = fs::read(append_path_suffix(&output, ".razero-package")).unwrap();
        let decoded_bundle = deserialize_native_package_metadata_bundle(&metadata_bundle).unwrap();
        assert_eq!(
            decoded_bundle.host_imports,
            vec![PackagedHostImportDescriptor {
                guest_module_name: "hello-host".to_string(),
                import_module: "env".to_string(),
                import_name: "print".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "razero_hello_host_env_print_handler".to_string(),
            }]
        );
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

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn link_hello_host_executable_accepts_any_single_function_import_name() {
        if !command_exists("cc") {
            return;
        }

        let workspace = test_workspace("package-link-driver-hello-host-generic-import");
        fs::create_dir_all(&workspace).unwrap();

        let mut module = decode_module(HELLO_HOST_WASM, CoreFeatures::V2).unwrap();
        module.build_memory_definitions();
        let import = module
            .import_section
            .iter_mut()
            .find(|import| matches!(import.ty, ExternType::FUNC))
            .expect("hello-host should have one function import");
        import.module = "host".to_string();
        import.name = "emit".to_string();

        let mut engine = CompilerEngine::new();
        engine.compile_module(&module).unwrap();
        let artifact = engine
            .compiled_module(&module)
            .unwrap()
            .emit_relocatable_object()
            .unwrap();

        let output = super::link_hello_host_executable(
            workspace.join("hello-host-generic-native"),
            &NativeLinkModule::from_artifact("hello-host", artifact),
            &[],
        )
        .unwrap();
        let metadata_bundle = fs::read(append_path_suffix(&output, ".razero-package")).unwrap();
        let decoded_bundle = deserialize_native_package_metadata_bundle(&metadata_bundle).unwrap();
        assert_eq!(
            decoded_bundle.host_imports,
            vec![PackagedHostImportDescriptor {
                guest_module_name: "hello-host".to_string(),
                import_module: "host".to_string(),
                import_name: "emit".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "razero_hello_host_host_emit_handler".to_string(),
            }]
        );
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

    #[test]
    fn validate_hello_host_metadata_rejects_multiple_function_imports() {
        let mut module = decode_module(HELLO_HOST_WASM, CoreFeatures::V2).unwrap();
        module
            .import_section
            .push(Import::function("env", "log", 0));
        let metadata = compile_module_metadata(&module);

        let err = validate_hello_host_metadata(&metadata).unwrap_err();
        assert_eq!(
            err.to_string(),
            "packaged host-import linker expected a descriptor for every imported function"
        );
    }

    #[test]
    fn validate_hello_host_metadata_rejects_wrong_host_import_signature() {
        let mut module = decode_module(HELLO_HOST_WASM, CoreFeatures::V2).unwrap();
        module
            .type_section
            .push(function_type(&[ValueType::I32], &[]));
        let wrong_type_index = (module.type_section.len() - 1) as u32;
        let host_import = module
            .import_section
            .iter_mut()
            .find(|import| matches!(import.ty, ExternType::FUNC))
            .expect("hello-host should have one function import");
        host_import.desc = ImportDesc::Func(wrong_type_index);
        let metadata = compile_module_metadata(&module);

        let err = validate_hello_host_metadata(&metadata).unwrap_err();
        assert_eq!(
            err.to_string(),
            "hello-host linker expects its host import to use the (i32, i32) -> () signature"
        );
    }

    #[test]
    fn validate_hello_host_metadata_rejects_run_export_with_non_void_signature() {
        let module = decode_module(HELLO_HOST_WASM, CoreFeatures::V2).unwrap();
        let mut metadata = compile_module_metadata(&module);
        let wrong_type_index = metadata.types.len() as u32;
        metadata.types.push(AotFunctionTypeMetadata {
            params: vec![ValueType::I32],
            results: vec![],
            param_num_in_u64: 1,
            result_num_in_u64: 0,
        });
        let run_export = metadata
            .exports
            .iter()
            .find(|export| export.ty.0 == 0 && export.name == "run")
            .expect("hello-host should export run");
        let run_function = metadata
            .functions
            .iter_mut()
            .find(|function| function.wasm_function_index == run_export.index)
            .expect("run export should resolve to a local function");
        run_function.type_index = wrong_type_index;

        let err = HelloHostSpec::from_metadata("hello-host", &metadata).unwrap_err();
        assert_eq!(
            err.to_string(),
            "hello-host run export must use the () -> () signature"
        );
    }

    #[test]
    fn validate_hello_host_metadata_rejects_missing_run_export() {
        let mut module = decode_module(HELLO_HOST_WASM, CoreFeatures::V2).unwrap();
        module
            .export_section
            .retain(|export| !(export.ty == ExternType::FUNC && export.name == "run"));
        let metadata = compile_module_metadata(&module);

        let err = validate_hello_host_metadata(&metadata).unwrap_err();
        assert_eq!(err.to_string(), "hello-host guest must export func run");
    }

    #[test]
    fn validate_hello_host_metadata_rejects_missing_local_memory() {
        let module = decode_module(HELLO_HOST_WASM, CoreFeatures::V2).unwrap();
        let mut metadata = compile_module_metadata(&module);
        metadata.memory = None;

        let err = HelloHostSpec::from_metadata("hello-host", &metadata).unwrap_err();
        assert_eq!(
            err.to_string(),
            "hello-host guest must define one local memory"
        );
    }

    #[test]
    fn validate_hello_host_metadata_rejects_passive_data_segments() {
        let module = decode_module(HELLO_HOST_WASM, CoreFeatures::V2).unwrap();
        let mut metadata = compile_module_metadata(&module);
        let segment = metadata
            .data_segments
            .first_mut()
            .expect("hello-host should include one data segment");
        segment.passive = true;

        let err = HelloHostSpec::from_metadata("hello-host", &metadata).unwrap_err();
        assert_eq!(
            err.to_string(),
            "hello-host linker does not support passive data segments"
        );
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
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
        let expected_metadata_sidecar_bytes = artifact.metadata_sidecar_bytes.clone();

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
        let decoded_bundle = deserialize_native_package_metadata_bundle(&metadata_bundle).unwrap();
        assert_eq!(
            decoded_bundle,
            NativePackageMetadataBundle {
                modules: vec![NativePackageMetadataEntry {
                    module_name: "guest".to_string(),
                    metadata_sidecar_bytes: expected_metadata_sidecar_bytes,
                }],
                host_imports: Vec::new(),
            }
        );

        let status = Command::new(&package.executable_path).status().unwrap();
        assert!(status.success());

        fs::remove_dir_all(workspace).unwrap();
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn link_native_executable_initializes_packaged_runtime_state() {
        if !command_exists("cc") || !command_exists("ar") {
            return;
        }

        let workspace = test_workspace("package-link-runtime-state");
        fs::create_dir_all(&workspace).unwrap();

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

extern int32_t razero_cabi_guest_function_2(void);

int main(void) {
    return razero_cabi_guest_function_2() == 5 ? 0 : 1;
}
"#,
        )
        .unwrap();
        let main_o = workspace.join("main.o");
        let libmain = workspace.join("libmain.a");
        assert!(Command::new("cc")
            .arg("-std=c11")
            .arg("-O2")
            .arg("-fno-pie")
            .arg("-c")
            .arg(&main_c)
            .arg("-o")
            .arg(&main_o)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("ar")
            .arg("rcs")
            .arg(&libmain)
            .arg(&main_o)
            .status()
            .unwrap()
            .success());

        let package = link_native_executable(
            workspace.join("guest-runtime-bin"),
            &[NativeLinkModule::from_artifact("guest", artifact)],
            &[libmain.clone()],
        )
        .unwrap();
        assert!(Command::new(&package.executable_path)
            .status()
            .unwrap()
            .success());

        fs::remove_dir_all(workspace).unwrap();
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn link_native_executable_packages_multiple_modules_with_richer_runtime_shapes() {
        if !command_exists("cc") || !command_exists("ar") {
            return;
        }

        let workspace = test_workspace("package-link-multi-module");
        fs::create_dir_all(&workspace).unwrap();

        let mut add_one = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b],
                ..Code::default()
            }],
            ..Module::default()
        };
        add_one.assign_module_id(b"package-link-multi-module-math", &[], false);
        let mut runtime_shape = Module {
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
        runtime_shape.assign_module_id(b"package-link-multi-module-state", &[], false);

        let mut engine = CompilerEngine::new();
        engine.compile_module(&add_one).unwrap();
        let add_one_artifact = engine
            .compiled_module(&add_one)
            .unwrap()
            .emit_relocatable_object()
            .unwrap();
        engine.compile_module(&runtime_shape).unwrap();
        let runtime_shape_artifact = engine
            .compiled_module(&runtime_shape)
            .unwrap()
            .emit_relocatable_object()
            .unwrap();

        let main_c = workspace.join("main.c");
        fs::write(
            &main_c,
            r#"#include <stdint.h>

extern int32_t razero_cabi_math_function_0(int32_t);
extern int32_t razero_cabi_state_function_2(void);

int main(void) {
    return razero_cabi_math_function_0(41) == 42 && razero_cabi_state_function_2() == 5 ? 0 : 1;
}
"#,
        )
        .unwrap();
        let main_o = workspace.join("main.o");
        let libmain = workspace.join("libmain.a");
        assert!(Command::new("cc")
            .arg("-std=c11")
            .arg("-O2")
            .arg("-fno-pie")
            .arg("-c")
            .arg(&main_c)
            .arg("-o")
            .arg(&main_o)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("ar")
            .arg("rcs")
            .arg(&libmain)
            .arg(&main_o)
            .status()
            .unwrap()
            .success());

        let package = link_native_executable(
            workspace.join("guest-multi-bin"),
            &[
                NativeLinkModule::from_artifact("math", add_one_artifact),
                NativeLinkModule::from_artifact("state", runtime_shape_artifact),
            ],
            &[libmain.clone()],
        )
        .unwrap();

        let export_symbols = package
            .cabi_exports
            .iter()
            .map(|export| export.symbol_name.as_str())
            .collect::<Vec<_>>();
        assert!(export_symbols.contains(&"razero_cabi_math_function_0"));
        assert!(export_symbols.contains(&"razero_cabi_state_function_2"));

        let metadata_bundle = fs::read(&package.metadata_bundle_path).unwrap();
        let decoded_bundle = deserialize_native_package_metadata_bundle(&metadata_bundle).unwrap();
        assert_eq!(
            decoded_bundle
                .modules
                .iter()
                .map(|module| module.module_name.as_str())
                .collect::<Vec<_>>(),
            vec!["math", "state"]
        );
        assert!(Command::new(&package.executable_path)
            .status()
            .unwrap()
            .success());

        fs::remove_dir_all(workspace).unwrap();
    }

    fn sample_package_metadata_bundle() -> NativePackageMetadataBundle {
        NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: Vec::new(),
        }
    }

    #[test]
    fn package_metadata_bundle_round_trips() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![
                NativePackageMetadataEntry {
                    module_name: "guest-a".to_string(),
                    metadata_sidecar_bytes: vec![1, 2, 3, 4],
                },
                NativePackageMetadataEntry {
                    module_name: "guest-b".to_string(),
                    metadata_sidecar_bytes: vec![5, 6, 7],
                },
            ],
            host_imports: vec![
                PackagedHostImportDescriptor {
                    guest_module_name: "guest-a".to_string(),
                    import_module: "env".to_string(),
                    import_name: "inc".to_string(),
                    function_import_index: 0,
                    type_index: 1,
                    host_symbol_name: "env_inc_handler".to_string(),
                },
                PackagedHostImportDescriptor {
                    guest_module_name: "guest-b".to_string(),
                    import_module: "math".to_string(),
                    import_name: "double".to_string(),
                    function_import_index: 1,
                    type_index: 2,
                    host_symbol_name: "math_double_handler".to_string(),
                },
            ],
        };
        let encoded = serialize_native_package_metadata_bundle(&bundle);
        assert!(encoded.starts_with(NATIVE_PACKAGE_MAGIC));
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_empty_metadata_sidecar() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: Vec::new(),
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 1,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_empty_module_name() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: String::new(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: Vec::new(),
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_empty_guest_module_name() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: String::new(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 1,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_empty_import_module() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: String::new(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 1,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_empty_import_name() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: String::new(),
                function_import_index: 0,
                type_index: 1,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_empty_host_symbol_name() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 1,
                host_symbol_name: String::new(),
            }],
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_max_function_import_index() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: u32::MAX,
                type_index: 1,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_max_type_index() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: u32::MAX,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_multiple_modules_and_boundary_indices() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![
                NativePackageMetadataEntry {
                    module_name: "guest-a".to_string(),
                    metadata_sidecar_bytes: vec![1, 2, 3],
                },
                NativePackageMetadataEntry {
                    module_name: "guest-b".to_string(),
                    metadata_sidecar_bytes: vec![4, 5, 6],
                },
            ],
            host_imports: vec![
                PackagedHostImportDescriptor {
                    guest_module_name: "guest-a".to_string(),
                    import_module: "env".to_string(),
                    import_name: "low".to_string(),
                    function_import_index: 0,
                    type_index: u32::MAX,
                    host_symbol_name: "env_low_handler".to_string(),
                },
                PackagedHostImportDescriptor {
                    guest_module_name: "guest-b".to_string(),
                    import_module: "math".to_string(),
                    import_name: "high".to_string(),
                    function_import_index: u32::MAX,
                    type_index: 0,
                    host_symbol_name: "math_high_handler".to_string(),
                },
            ],
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_three_modules_varying_sidecars() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![
                NativePackageMetadataEntry {
                    module_name: "guest-a".to_string(),
                    metadata_sidecar_bytes: Vec::new(),
                },
                NativePackageMetadataEntry {
                    module_name: "guest-b".to_string(),
                    metadata_sidecar_bytes: (0u8..100).collect(),
                },
                NativePackageMetadataEntry {
                    module_name: "guest-c".to_string(),
                    metadata_sidecar_bytes: vec![0xff],
                },
            ],
            host_imports: Vec::new(),
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_four_imports_and_boundary_mix() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![
                NativePackageMetadataEntry {
                    module_name: "guest-a".to_string(),
                    metadata_sidecar_bytes: vec![1, 2, 3],
                },
                NativePackageMetadataEntry {
                    module_name: "guest-b".to_string(),
                    metadata_sidecar_bytes: vec![4, 5, 6, 7],
                },
            ],
            host_imports: vec![
                PackagedHostImportDescriptor {
                    guest_module_name: "guest-a".to_string(),
                    import_module: "env".to_string(),
                    import_name: "zero".to_string(),
                    function_import_index: 0,
                    type_index: 0,
                    host_symbol_name: "env_zero_handler".to_string(),
                },
                PackagedHostImportDescriptor {
                    guest_module_name: "guest-a".to_string(),
                    import_module: "env".to_string(),
                    import_name: "max".to_string(),
                    function_import_index: u32::MAX,
                    type_index: u32::MAX,
                    host_symbol_name: "env_max_handler".to_string(),
                },
                PackagedHostImportDescriptor {
                    guest_module_name: "guest-b".to_string(),
                    import_module: "math".to_string(),
                    import_name: "mid".to_string(),
                    function_import_index: 1,
                    type_index: 100,
                    host_symbol_name: "math_mid_handler".to_string(),
                },
                PackagedHostImportDescriptor {
                    guest_module_name: "guest-b".to_string(),
                    import_module: "math".to_string(),
                    import_name: "near-max".to_string(),
                    function_import_index: u32::MAX - 1,
                    type_index: 1,
                    host_symbol_name: "math_near_max_handler".to_string(),
                },
            ],
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_runtime_state_aot_sidecar() {
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
        let metadata = compile_module_metadata(&module);
        let sidecar = serialize_aot_metadata(&metadata);
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: sidecar,
            }],
            host_imports: Vec::new(),
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
        let decoded_metadata =
            crate::aot::deserialize_aot_metadata(&decoded.modules[0].metadata_sidecar_bytes)
                .unwrap();
        assert_eq!(decoded_metadata, metadata);
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_multiple_tables_varying_max_flags() {
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            table_section: vec![
                Table {
                    min: 0,
                    max: None,
                    ty: RefType::FUNCREF,
                },
                Table {
                    min: 1,
                    max: Some(1),
                    ty: RefType::FUNCREF,
                },
                Table {
                    min: 2,
                    max: Some(u32::MAX),
                    ty: RefType::FUNCREF,
                },
            ],
            code_section: vec![Code {
                body: vec![0x41, 0x05, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            enabled_features: CoreFeatures::V2,
            ..Module::default()
        };
        let metadata = compile_module_metadata(&module);
        let sidecar = serialize_aot_metadata(&metadata);
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: sidecar,
            }],
            host_imports: Vec::new(),
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
        let decoded_metadata =
            crate::aot::deserialize_aot_metadata(&decoded.modules[0].metadata_sidecar_bytes)
                .unwrap();
        assert_eq!(decoded_metadata, metadata);
        assert_eq!(
            decoded_metadata
                .tables
                .iter()
                .map(|table| (table.min, table.max))
                .collect::<Vec<_>>(),
            vec![(0, None), (1, Some(1)), (2, Some(u32::MAX))]
        );
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_multiple_data_segments_passive_boundary() {
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
                body: vec![0x41, 0x05, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            data_section: vec![
                DataSegment {
                    offset_expression: ConstExpr::from_opcode(0x41, &[0x00]),
                    init: vec![1, 2, 3],
                    passive: false,
                },
                DataSegment {
                    offset_expression: ConstExpr::from_opcode(0x41, &[0x00]),
                    init: Vec::new(),
                    passive: true,
                },
                DataSegment {
                    offset_expression: ConstExpr::from_opcode(0x41, &[0x10]),
                    init: vec![0xff; 32],
                    passive: true,
                },
            ],
            enabled_features: CoreFeatures::V2,
            ..Module::default()
        };
        let metadata = compile_module_metadata(&module);
        let sidecar = serialize_aot_metadata(&metadata);
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: sidecar,
            }],
            host_imports: Vec::new(),
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
        let decoded_metadata =
            crate::aot::deserialize_aot_metadata(&decoded.modules[0].metadata_sidecar_bytes)
                .unwrap();
        assert_eq!(decoded_metadata, metadata);
        assert_eq!(
            decoded_metadata
                .data_segments
                .iter()
                .map(|segment| (segment.passive, segment.init.len()))
                .collect::<Vec<_>>(),
            vec![(false, 3), (true, 0), (true, 32)]
        );
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_mutable_and_boundary_globals() {
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            global_section: vec![
                Global {
                    ty: GlobalType {
                        val_type: ValueType::I32,
                        mutable: false,
                    },
                    init: ConstExpr::from_i32(0),
                },
                Global {
                    ty: GlobalType {
                        val_type: ValueType::I64,
                        mutable: true,
                    },
                    init: ConstExpr::from_i64(9),
                },
                Global {
                    ty: GlobalType {
                        val_type: ValueType::I32,
                        mutable: true,
                    },
                    init: ConstExpr::from_i32(i32::MAX),
                },
            ],
            code_section: vec![Code {
                body: vec![0x41, 0x05, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            enabled_features: CoreFeatures::V2,
            ..Module::default()
        };
        let metadata = compile_module_metadata(&module);
        let sidecar = serialize_aot_metadata(&metadata);
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: sidecar,
            }],
            host_imports: Vec::new(),
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
        let decoded_metadata =
            crate::aot::deserialize_aot_metadata(&decoded.modules[0].metadata_sidecar_bytes)
                .unwrap();
        assert_eq!(decoded_metadata, metadata);
        assert_eq!(
            decoded_metadata
                .globals
                .iter()
                .map(|global| (global.val_type, global.mutable))
                .collect::<Vec<_>>(),
            vec![
                (ValueType::I32, false),
                (ValueType::I64, true),
                (ValueType::I32, true),
            ]
        );
        assert_eq!(3, decoded_metadata.global_initializers.len());
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_multiple_element_segments_mixed_modes() {
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0, 0],
            table_section: vec![
                Table {
                    min: 1,
                    max: Some(1),
                    ty: RefType::FUNCREF,
                },
                Table {
                    min: 2,
                    max: Some(2),
                    ty: RefType::FUNCREF,
                },
            ],
            code_section: vec![
                Code {
                    body: vec![0x41, 0x05, 0x0b],
                    ..Code::default()
                },
                Code {
                    body: vec![0x41, 0x07, 0x0b],
                    ..Code::default()
                },
            ],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            element_section: vec![
                ElementSegment {
                    offset_expr: ConstExpr::from_i32(0),
                    table_index: 0,
                    init: vec![ConstExpr::from_opcode(0xd2, &[0])],
                    ty: RefType::FUNCREF,
                    mode: ElementMode::Active,
                },
                ElementSegment {
                    offset_expr: ConstExpr::from_i32(0),
                    table_index: 1,
                    init: vec![
                        ConstExpr::from_opcode(0xd2, &[0]),
                        ConstExpr::from_opcode(0xd2, &[1]),
                    ],
                    ty: RefType::FUNCREF,
                    mode: ElementMode::Passive,
                },
                ElementSegment {
                    offset_expr: ConstExpr::from_i32(0),
                    table_index: 0,
                    init: vec![ConstExpr::from_opcode(0xd2, &[1])],
                    ty: RefType::FUNCREF,
                    mode: ElementMode::Declarative,
                },
            ],
            enabled_features: CoreFeatures::V2,
            ..Module::default()
        };
        let metadata = compile_module_metadata(&module);
        let sidecar = serialize_aot_metadata(&metadata);
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: sidecar,
            }],
            host_imports: Vec::new(),
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
        let decoded_metadata =
            crate::aot::deserialize_aot_metadata(&decoded.modules[0].metadata_sidecar_bytes)
                .unwrap();
        assert_eq!(decoded_metadata, metadata);
        assert_eq!(
            decoded_metadata
                .element_segments
                .iter()
                .map(|segment| (
                    segment.table_index,
                    segment.mode,
                    segment.init_expressions.len()
                ))
                .collect::<Vec<_>>(),
            vec![
                (0, ElementMode::Active, 1),
                (1, ElementMode::Passive, 2),
                (0, ElementMode::Declarative, 1),
            ]
        );
    }

    #[test]
    fn package_metadata_bundle_round_trips_with_memory_config_variants() {
        let modules = [
            (
                "guest-unbounded",
                Module {
                    type_section: vec![function_type(&[], &[ValueType::I32])],
                    function_section: vec![0],
                    memory_section: Some(razero_wasm::module::Memory {
                        min: 0,
                        cap: 0,
                        max: 0,
                        is_max_encoded: false,
                        ..razero_wasm::module::Memory::default()
                    }),
                    code_section: vec![Code {
                        body: vec![0x41, 0x05, 0x0b],
                        ..Code::default()
                    }],
                    export_section: vec![Export {
                        ty: ExternType::FUNC,
                        name: "run".to_string(),
                        index: 0,
                    }],
                    enabled_features: CoreFeatures::V2,
                    ..Module::default()
                },
            ),
            (
                "guest-tight",
                Module {
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
                        body: vec![0x41, 0x05, 0x0b],
                        ..Code::default()
                    }],
                    export_section: vec![Export {
                        ty: ExternType::FUNC,
                        name: "run".to_string(),
                        index: 0,
                    }],
                    enabled_features: CoreFeatures::V2,
                    ..Module::default()
                },
            ),
            (
                "guest-wide",
                Module {
                    type_section: vec![function_type(&[], &[ValueType::I32])],
                    function_section: vec![0],
                    memory_section: Some(razero_wasm::module::Memory {
                        min: 2,
                        cap: 256,
                        max: 256,
                        is_max_encoded: true,
                        ..razero_wasm::module::Memory::default()
                    }),
                    code_section: vec![Code {
                        body: vec![0x41, 0x05, 0x0b],
                        ..Code::default()
                    }],
                    export_section: vec![Export {
                        ty: ExternType::FUNC,
                        name: "run".to_string(),
                        index: 0,
                    }],
                    enabled_features: CoreFeatures::V2,
                    ..Module::default()
                },
            ),
        ];
        let bundle = NativePackageMetadataBundle {
            modules: modules
                .iter()
                .map(|(name, module)| NativePackageMetadataEntry {
                    module_name: (*name).to_string(),
                    metadata_sidecar_bytes: serialize_aot_metadata(&compile_module_metadata(
                        module,
                    )),
                })
                .collect(),
            host_imports: Vec::new(),
        };

        let encoded = serialize_native_package_metadata_bundle(&bundle);
        let decoded = deserialize_native_package_metadata_bundle(&encoded).unwrap();
        assert_eq!(decoded, bundle);
        let memories = decoded
            .modules
            .iter()
            .map(|module| {
                crate::aot::deserialize_aot_metadata(&module.metadata_sidecar_bytes)
                    .unwrap()
                    .memory
                    .expect("memory metadata should be present")
            })
            .map(|memory| (memory.min, memory.cap, memory.max, memory.is_max_encoded))
            .collect::<Vec<_>>();
        assert_eq!(
            memories,
            vec![(0, 0, 0, false), (1, 1, 1, true), (2, 256, 256, true)]
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_invalid_magic_number() {
        let bundle = sample_package_metadata_bundle();
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        encoded[..NATIVE_PACKAGE_MAGIC.len()].copy_from_slice(b"BADMAGIC");

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid magic number"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_magic_header() {
        let bundle = sample_package_metadata_bundle();
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        encoded.truncate(4);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid header length"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_module_count() {
        let bundle = sample_package_metadata_bundle();
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        encoded.truncate(NATIVE_PACKAGE_MAGIC.len() + 2);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid u32 field"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_module_name_length() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: Vec::new(),
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len() + 4 + 2;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid u32 field"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_module_name_bytes() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: Vec::new(),
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at =
            NATIVE_PACKAGE_MAGIC.len() + 4 + 4 + bundle.modules[0].module_name.len() - 2;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid module name"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_invalid_utf8_in_module_name() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: Vec::new(),
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let module_name_offset = NATIVE_PACKAGE_MAGIC.len() + 4 + 4;
        encoded[module_name_offset] = 0xff;

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid module name: invalid UTF-8: invalid utf-8 sequence of 1 bytes from index 0"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_module_sidecar() {
        let bundle = sample_package_metadata_bundle();
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            - 1;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid module sidecar"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_module_sidecar_length_partial() {
        let bundle = sample_package_metadata_bundle();
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at =
            NATIVE_PACKAGE_MAGIC.len() + 4 + 4 + bundle.modules[0].module_name.len() + 1;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid u64 field"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_count() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 2;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid u32 field"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_guest_module_name_length() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 2;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid u32 field"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_guest_module_name_bytes() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            - 2;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid guest module name"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_invalid_utf8_in_guest_module_name() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let guest_module_name_offset = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4;
        encoded[guest_module_name_offset] = 0xff;

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid guest module name: invalid UTF-8: invalid utf-8 sequence of 1 bytes from index 0"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_module_name_length() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 2;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid u32 field"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_import_name_length() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 4
            + bundle.host_imports[0].import_module.len()
            + 2;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid u32 field"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_import_name_bytes() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 4
            + bundle.host_imports[0].import_module.len()
            + 4
            + bundle.host_imports[0].import_name.len()
            - 2;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid import name"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_invalid_utf8_in_import_name() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let import_name_offset = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 4
            + bundle.host_imports[0].import_module.len()
            + 4;
        encoded[import_name_offset] = 0xff;

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid import name: invalid UTF-8: invalid utf-8 sequence of 1 bytes from index 0"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_module_name_bytes() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 4
            + bundle.host_imports[0].import_module.len()
            - 2;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid import module"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_invalid_utf8_in_import_module() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let import_module_offset = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 4;
        encoded[import_module_offset] = 0xff;

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid import module: invalid UTF-8: invalid utf-8 sequence of 1 bytes from index 0"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_function_index() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 4
            + bundle.host_imports[0].import_module.len()
            + 4
            + bundle.host_imports[0].import_name.len()
            + 2;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid u32 field"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_function_index_partial() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 4
            + bundle.host_imports[0].import_module.len()
            + 4
            + bundle.host_imports[0].import_name.len()
            + 1;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid u32 field"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_type_index() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 4
            + bundle.host_imports[0].import_module.len()
            + 4
            + bundle.host_imports[0].import_name.len()
            + 4
            + 2;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid u32 field"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_type_index_partial() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 4
            + bundle.host_imports[0].import_module.len()
            + 4
            + bundle.host_imports[0].import_name.len()
            + 4
            + 1;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid u32 field"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_symbol_name_length() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 4
            + bundle.host_imports[0].import_module.len()
            + 4
            + bundle.host_imports[0].import_name.len()
            + 4
            + 4;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid u32 field"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_symbol_name_bytes() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let truncate_at = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 4
            + bundle.host_imports[0].import_module.len()
            + 4
            + bundle.host_imports[0].import_name.len()
            + 4
            + 4
            + 4
            + bundle.host_imports[0].host_symbol_name.len()
            - 2;
        encoded.truncate(truncate_at);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid host symbol name"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_invalid_utf8_in_host_symbol_name() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        let host_symbol_name_offset = NATIVE_PACKAGE_MAGIC.len()
            + 4
            + 4
            + bundle.modules[0].module_name.len()
            + 8
            + bundle.modules[0].metadata_sidecar_bytes.len()
            + 4
            + 4
            + bundle.host_imports[0].guest_module_name.len()
            + 4
            + bundle.host_imports[0].import_module.len()
            + 4
            + bundle.host_imports[0].import_name.len()
            + 4
            + 4
            + 4;
        encoded[host_symbol_name_offset] = 0xff;

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid host symbol name: invalid UTF-8: invalid utf-8 sequence of 1 bytes from index 0"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_truncated_host_import_metadata() {
        let bundle = NativePackageMetadataBundle {
            modules: vec![NativePackageMetadataEntry {
                module_name: "guest".to_string(),
                metadata_sidecar_bytes: vec![1, 2, 3],
            }],
            host_imports: vec![PackagedHostImportDescriptor {
                guest_module_name: "guest".to_string(),
                import_module: "env".to_string(),
                import_name: "inc".to_string(),
                function_import_index: 0,
                type_index: 0,
                host_symbol_name: "env_inc_handler".to_string(),
            }],
        };
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        encoded.pop();

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: invalid host symbol name"
        );
    }

    #[test]
    fn package_metadata_bundle_rejects_unexpected_trailing_bytes() {
        let bundle = sample_package_metadata_bundle();
        let mut encoded = serialize_native_package_metadata_bundle(&bundle);
        encoded.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);

        let err = deserialize_native_package_metadata_bundle(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "native package metadata: unexpected trailing bytes"
        );
    }

    #[test]
    fn link_native_executable_rejects_unsupported_targets_before_linking() {
        let metadata = AotCompiledMetadata {
            target: AotTarget {
                architecture: AotTargetArchitecture::Aarch64,
                operating_system: AotTargetOperatingSystem::Windows,
            },
            functions: vec![crate::aot::AotFunctionMetadata {
                local_function_index: 0,
                wasm_function_index: 0,
                type_index: 0,
                executable_offset: 0,
                executable_len: 4,
            }],
            types: vec![crate::aot::AotFunctionTypeMetadata {
                params: vec![ValueType::I32],
                results: vec![ValueType::I32],
                param_num_in_u64: 1,
                result_num_in_u64: 1,
            }],
            ..AotCompiledMetadata::default()
        };

        let err = link_native_executable(
            PathBuf::from("target/unsupported-native-bin"),
            &[NativeLinkModule::new(
                "guest",
                Vec::new(),
                serialize_aot_metadata(&metadata),
            )],
            &[],
        )
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "module 'guest' targets Windows, only linux is supported"
        );
    }

    #[test]
    fn link_native_executable_rejects_empty_module_list() {
        let err =
            link_native_executable(PathBuf::from("target/empty-native-bin"), &[], &[]).unwrap_err();

        assert_eq!(
            err.to_string(),
            "native executable packaging requires at least one relocatable object"
        );
    }

    #[test]
    fn link_native_executable_rejects_module_without_cabi_exports() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            ..Module::default()
        };
        let mut metadata = compile_module_metadata(&module);
        metadata.types[0].results = vec![ValueType::V128];
        metadata.types[0].result_num_in_u64 = 2;

        let err = link_native_executable(
            PathBuf::from("target/no-cabi-native-bin"),
            &[NativeLinkModule::new(
                "guest",
                Vec::new(),
                serialize_aot_metadata(&metadata),
            )],
            &[],
        )
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "module 'guest' does not expose any C ABI compatible functions"
        );
    }

    #[test]
    fn build_wrapper_source_switches_to_aarch64_entrypoint_symbols() {
        let source = build_wrapper_source(
            &[ModuleSpec {
                sanitized_name: "guest".to_string(),
                metadata: crate::aot::AotCompiledMetadata {
                    types: vec![crate::aot::AotFunctionTypeMetadata {
                        params: vec![ValueType::I32],
                        results: vec![ValueType::I32],
                        param_num_in_u64: 1,
                        result_num_in_u64: 1,
                    }],
                    functions: vec![crate::aot::AotFunctionMetadata {
                        local_function_index: 0,
                        wasm_function_index: 0,
                        type_index: 0,
                        executable_offset: 0,
                        executable_len: 4,
                    }],
                    ..crate::aot::AotCompiledMetadata::default()
                },
            }],
            64,
            NativePackagingTarget {
                architecture: crate::aot::AotTargetArchitecture::Aarch64,
                object_architecture: object::Architecture::Aarch64,
                entrypoint_symbol: "razero_arm64_entrypoint",
                after_host_call_entrypoint_symbol: "razero_arm64_after_go_function_call_entrypoint",
                entry_asm_source: "",
                trim_void_preamble_prologue: false,
            },
        )
        .unwrap();

        assert!(source.contains("razero_arm64_entrypoint"));
        assert!(!source.contains("razero_amd64_entrypoint"));
    }
}
