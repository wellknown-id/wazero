#![doc = "Compiler engine glue."]

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
#[cfg(test)]
use std::{
    sync::OnceLock,
    thread::{self, ThreadId},
};

use object::write::{
    Object as ElfObject, Relocation as ElfRelocation, StandardSection, Symbol as ElfSymbol,
    SymbolSection,
};
use object::{
    Architecture as ObjectArchitecture, BinaryFormat, Endianness, RelocationEncoding,
    RelocationFlags, RelocationKind, SymbolFlags, SymbolKind, SymbolScope,
};
use razero_platform::{
    map_code_segment, protect_code_segment, unmap_code_segment, CodeSegment, MmapError,
};
use razero_wasm::engine::{
    CompileOptions, Engine as WasmEngine, EngineError, ModuleEngine as WasmModuleEngine,
};
use razero_wasm::module::{Index, Module, ModuleId};
use razero_wasm::module_instance::ModuleInstance;

use crate::aot::{
    relocations_for_function, serialize_aot_metadata, AotCompiledMetadata, AotFunctionMetadata,
    AotRelocationMetadata, AotSourceMapEntry, AotTargetArchitecture, AotTargetOperatingSystem,
};
use crate::backend::{Compiler as BackendCompiler, Machine, RelocationInfo, SourceOffsetInfo};
use crate::engine_cache::{
    deserialize_compiled_module, file_cache_key, serialize_compiled_module, CachedCompiledModule,
    CompiledModuleCache, PrecompiledModuleArtifact,
};
use crate::frontend::{
    signature_for_wasm_function_type, wasm_type_to_ssa_type, Compiler as FrontendCompiler,
};
use crate::module_engine::CompilerModuleEngine;
use crate::ssa::{Builder, Signature, SignatureId, Type};
use crate::wazevoapi::{ExitCode, ModuleContextOffsetData, ModuleContextOffsetSource};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use crate::sighandler_linux_amd64 as signal_handler;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
use crate::sighandler_linux_arm64 as signal_handler;
#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64")
)))]
use crate::sighandler_stub as signal_handler;

#[cfg(target_arch = "x86_64")]
use crate::backend::isa::amd64::machine::Amd64Machine as NativeMachine;
#[cfg(target_arch = "aarch64")]
use crate::backend::isa::arm64::machine::Arm64Machine as NativeMachine;

pub struct AlignedBytes {
    storage: Vec<u128>,
    len: usize,
    executable: Option<CodeSegment>,
}

impl AlignedBytes {
    pub fn zeroed(len: usize) -> Self {
        if len == 0 {
            return Self::default();
        }
        let words = len.div_ceil(16);
        Self {
            storage: vec![0; words],
            len,
            executable: None,
        }
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let mut ret = Self::zeroed(bytes.len());
        ret.as_mut_slice().copy_from_slice(&bytes);
        ret
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_ptr(&self) -> *const u8 {
        if self.len == 0 {
            std::ptr::null()
        } else if let Some(executable) = &self.executable {
            executable.as_ptr()
        } else {
            self.storage.as_ptr().cast()
        }
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.clear_executable();
        if self.len == 0 {
            std::ptr::null_mut()
        } else {
            self.storage.as_mut_ptr().cast()
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        if self.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.storage.as_ptr().cast(), self.len) }
        }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.clear_executable();
        if self.len == 0 {
            &mut []
        } else {
            unsafe { std::slice::from_raw_parts_mut(self.as_mut_ptr(), self.len) }
        }
    }

    pub fn ptr_at(&self, offset: usize) -> Option<usize> {
        (offset < self.len).then_some(self.as_ptr() as usize + offset)
    }

    pub fn make_executable(&mut self) -> Result<(), MmapError> {
        if self.len == 0 || self.executable.is_some() {
            return Ok(());
        }
        let mut segment = map_code_segment(self.len)?;
        segment.as_mut_slice()?.copy_from_slice(self.as_slice());
        if let Err(err) = protect_code_segment(&mut segment) {
            let _ = unmap_code_segment(&mut segment);
            return Err(err);
        }
        self.executable = Some(segment);
        Ok(())
    }

    fn clear_executable(&mut self) {
        if let Some(mut segment) = self.executable.take() {
            let _ = unmap_code_segment(&mut segment);
        }
    }
}

impl Default for AlignedBytes {
    fn default() -> Self {
        Self {
            storage: Vec::new(),
            len: 0,
            executable: None,
        }
    }
}

impl Clone for AlignedBytes {
    fn clone(&self) -> Self {
        let mut cloned = Self::from_bytes(self.as_slice().to_vec());
        if self.executable.is_some() {
            cloned
                .make_executable()
                .expect("cloned executable bytes must remap");
        }
        cloned
    }
}

impl std::fmt::Debug for AlignedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlignedBytes")
            .field("len", &self.len)
            .field(
                "executable",
                &self.executable.as_ref().map(CodeSegment::is_executable),
            )
            .finish()
    }
}

impl PartialEq for AlignedBytes {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for AlignedBytes {}

impl Drop for AlignedBytes {
    fn drop(&mut self) {
        self.clear_executable();
    }
}

unsafe impl Send for AlignedBytes {}
unsafe impl Sync for AlignedBytes {}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Executables {
    pub executable: AlignedBytes,
    pub entry_preambles: AlignedBytes,
    pub(crate) entry_preamble_offsets: Vec<usize>,
}

impl Executables {
    pub fn from_executable_bytes(bytes: Vec<u8>) -> Self {
        Self {
            executable: AlignedBytes::from_bytes(bytes),
            ..Self::default()
        }
    }

    pub fn entry_preamble_ptr(&self, index: usize) -> Option<usize> {
        self.entry_preamble_offsets
            .get(index)
            .and_then(|offset| self.entry_preambles.ptr_at(*offset))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListenerTrampolines {
    executable: AlignedBytes,
    before_offset: usize,
    after_offset: usize,
}

impl ListenerTrampolines {
    pub fn before_ptr(&self) -> Option<usize> {
        self.executable.ptr_at(self.before_offset)
    }

    pub fn after_ptr(&self) -> Option<usize> {
        self.executable.ptr_at(self.after_offset)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SharedFunctions {
    executable: AlignedBytes,
    memory_grow_offset: Option<usize>,
    check_module_exit_code_offset: Option<usize>,
    stack_grow_offset: Option<usize>,
    table_grow_offset: Option<usize>,
    ref_func_offset: Option<usize>,
    memory_wait32_offset: Option<usize>,
    memory_wait64_offset: Option<usize>,
    memory_notify_offset: Option<usize>,
    listener_trampolines: HashMap<String, ListenerTrampolines>,
}

impl SharedFunctions {
    pub fn memory_grow_ptr(&self) -> Option<usize> {
        self.memory_grow_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn check_module_exit_code_ptr(&self) -> Option<usize> {
        self.check_module_exit_code_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn stack_grow_ptr(&self) -> Option<usize> {
        self.stack_grow_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn table_grow_ptr(&self) -> Option<usize> {
        self.table_grow_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn ref_func_ptr(&self) -> Option<usize> {
        self.ref_func_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn memory_wait32_ptr(&self) -> Option<usize> {
        self.memory_wait32_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn memory_wait64_ptr(&self) -> Option<usize> {
        self.memory_wait64_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn memory_notify_ptr(&self) -> Option<usize> {
        self.memory_notify_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceMap {
    pub executable_offsets: Vec<usize>,
    pub wasm_binary_offsets: Vec<u64>,
}

#[derive(Clone, Debug)]
pub struct CompiledModule {
    pub executables: Executables,
    pub function_offsets: Vec<usize>,
    pub module: Module,
    pub offsets: ModuleContextOffsetData,
    pub aot: AotCompiledMetadata,
    pub shared_functions: Arc<SharedFunctions>,
    pub ensure_termination: bool,
    pub fuel_enabled: bool,
    pub fuel: i64,
    pub memory_isolation_enabled: bool,
    pub source_map: SourceMap,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelocatableObjectArtifact {
    pub object_bytes: Vec<u8>,
    pub metadata_sidecar_bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectEmissionError {
    message: String,
}

impl ObjectEmissionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ObjectEmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ObjectEmissionError {}

impl CompiledModule {
    pub fn function_ptr(&self, local_index: usize) -> Option<usize> {
        self.function_offsets
            .get(local_index)
            .and_then(|offset| self.executables.executable.ptr_at(*offset))
    }

    pub fn entry_preamble_ptr(&self, type_index: usize) -> Option<usize> {
        self.executables.entry_preamble_ptr(type_index)
    }

    pub fn aot_metadata(&self) -> &AotCompiledMetadata {
        &self.aot
    }

    pub fn emit_relocatable_object(
        &self,
    ) -> Result<RelocatableObjectArtifact, ObjectEmissionError> {
        if self.aot.target.operating_system != AotTargetOperatingSystem::Linux {
            return Err(ObjectEmissionError::new(format!(
                "relocatable object emission currently supports Linux only (got {:?})",
                self.aot.target.operating_system
            )));
        }
        let object_architecture = relocatable_object_architecture(self.aot.target.architecture)?;

        let mut text_bytes = self.executables.executable.as_slice().to_vec();
        normalize_relocation_sites(
            self.aot.target.architecture,
            &mut text_bytes,
            &self.aot.relocations,
        )?;

        let mut object = ElfObject::new(BinaryFormat::Elf, object_architecture, Endianness::Little);
        object.add_file_symbol(b"razero-module".to_vec());
        let text_section = object.section_id(StandardSection::Text);
        object.set_section_data(text_section, text_bytes, 16);

        let mut symbols = HashMap::new();
        for (wasm_function_index, symbol) in import_function_symbols(&mut object, self)? {
            symbols.insert(wasm_function_index, symbol);
        }
        for (wasm_function_index, symbol) in
            local_function_symbols(&mut object, text_section, self)?
        {
            symbols.insert(wasm_function_index, symbol);
        }
        for relocation in &self.aot.relocations {
            let symbol = symbols
                .get(&relocation.target_function_index)
                .copied()
                .ok_or_else(|| {
                    ObjectEmissionError::new(format!(
                        "missing relocation target symbol for function index {}",
                        relocation.target_function_index
                    ))
                })?;
            object
                .add_relocation(
                    text_section,
                    object_relocation(self.aot.target.architecture, relocation, symbol)?,
                )
                .map_err(|err| ObjectEmissionError::new(err.to_string()))?;
        }

        Ok(RelocatableObjectArtifact {
            object_bytes: object
                .write()
                .map_err(|err| ObjectEmissionError::new(err.to_string()))?,
            metadata_sidecar_bytes: serialize_aot_metadata(&self.aot),
        })
    }

    pub fn function_index_of(&self, addr: usize) -> Option<Index> {
        let base = self.executables.executable.as_ptr() as usize;
        if base == 0 || addr < base || addr >= base + self.executables.executable.len() {
            return None;
        }
        let rel = addr - base;
        let index = self
            .function_offsets
            .partition_point(|offset| *offset <= rel);
        (index > 0).then_some((index - 1) as Index)
    }

    pub fn source_offset_for_pc(&self, addr: usize) -> u64 {
        let base = self.executables.executable.as_ptr() as usize;
        let rel = addr.saturating_sub(base);
        let index = self
            .source_map
            .executable_offsets
            .partition_point(|offset| *offset <= rel);
        if index == 0 {
            0
        } else {
            self.source_map.wasm_binary_offsets[index - 1]
        }
    }
}

fn relocatable_object_architecture(
    architecture: AotTargetArchitecture,
) -> Result<ObjectArchitecture, ObjectEmissionError> {
    match architecture {
        AotTargetArchitecture::X86_64 => Ok(ObjectArchitecture::X86_64),
        AotTargetArchitecture::Aarch64 => Ok(ObjectArchitecture::Aarch64),
        other => Err(ObjectEmissionError::new(format!(
            "relocatable object emission currently supports x86_64 and aarch64 only (got {other:?})"
        ))),
    }
}

fn object_relocation(
    architecture: AotTargetArchitecture,
    relocation: &AotRelocationMetadata,
    symbol: object::write::SymbolId,
) -> Result<ElfRelocation, ObjectEmissionError> {
    match architecture {
        AotTargetArchitecture::X86_64 => Ok(ElfRelocation {
            offset: x86_64_relocation_offset(relocation)?,
            symbol,
            addend: -4,
            flags: RelocationFlags::Generic {
                kind: RelocationKind::Relative,
                encoding: RelocationEncoding::X86Branch,
                size: 32,
            },
        }),
        AotTargetArchitecture::Aarch64 => Ok(ElfRelocation {
            offset: aarch64_relocation_offset(relocation)?,
            symbol,
            addend: 0,
            flags: RelocationFlags::Generic {
                kind: RelocationKind::Relative,
                encoding: RelocationEncoding::AArch64Call,
                size: 26,
            },
        }),
        other => Err(ObjectEmissionError::new(format!(
            "unsupported relocation architecture {other:?}"
        ))),
    }
}

fn normalize_relocation_sites(
    architecture: AotTargetArchitecture,
    text_bytes: &mut [u8],
    relocations: &[AotRelocationMetadata],
) -> Result<(), ObjectEmissionError> {
    match architecture {
        AotTargetArchitecture::X86_64 => normalize_x86_64_relocation_sites(text_bytes, relocations),
        AotTargetArchitecture::Aarch64 => {
            normalize_aarch64_relocation_sites(text_bytes, relocations)
        }
        other => Err(ObjectEmissionError::new(format!(
            "unsupported relocation architecture {other:?}"
        ))),
    }
}

fn normalize_aarch64_relocation_sites(
    text_bytes: &mut [u8],
    relocations: &[AotRelocationMetadata],
) -> Result<(), ObjectEmissionError> {
    for relocation in relocations {
        let offset = usize::try_from(relocation.executable_offset).map_err(|_| {
            ObjectEmissionError::new(format!(
                "invalid negative relocation offset {}",
                relocation.executable_offset
            ))
        })?;
        let text_len = text_bytes.len();
        let word = if relocation.is_tail_call {
            0x1400_0000u32
        } else {
            0x9400_0000u32
        };
        let range = text_bytes.get_mut(offset..offset + 4).ok_or_else(|| {
            ObjectEmissionError::new(format!(
                "relocation site at {offset} is out of bounds for {} bytes",
                text_len
            ))
        })?;
        range.copy_from_slice(&word.to_le_bytes());
    }
    Ok(())
}

fn aarch64_relocation_offset(
    relocation: &AotRelocationMetadata,
) -> Result<u64, ObjectEmissionError> {
    u64::try_from(relocation.executable_offset).map_err(|_| {
        ObjectEmissionError::new(format!(
            "invalid negative relocation offset {}",
            relocation.executable_offset
        ))
    })
}

fn import_function_symbols(
    object: &mut ElfObject<'_>,
    compiled: &CompiledModule,
) -> Result<Vec<(Index, object::write::SymbolId)>, ObjectEmissionError> {
    let mut ret = Vec::new();
    let mut next_function_import_index = 0u32;
    for import in &compiled.aot.imports {
        if !matches!(import.desc, crate::aot::AotImportDescMetadata::Func(_)) {
            continue;
        }
        if next_function_import_index >= compiled.aot.import_function_count {
            break;
        }
        let name = format!(
            "razero_import_function_{}_{}_{}",
            next_function_import_index,
            sanitize_symbol_component(&import.module),
            sanitize_symbol_component(&import.name)
        );
        let symbol = object.add_symbol(ElfSymbol {
            name: name.into_bytes(),
            value: 0,
            size: 0,
            kind: SymbolKind::Text,
            scope: SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Undefined,
            flags: SymbolFlags::None,
        });
        ret.push((next_function_import_index, symbol));
        next_function_import_index += 1;
    }
    Ok(ret)
}

fn local_function_symbols(
    object: &mut ElfObject<'_>,
    text_section: object::write::SectionId,
    compiled: &CompiledModule,
) -> Result<Vec<(Index, object::write::SymbolId)>, ObjectEmissionError> {
    let mut ret = Vec::with_capacity(compiled.aot.functions.len());
    for function in &compiled.aot.functions {
        let symbol = object.add_symbol(ElfSymbol {
            name: format!("razero_wasm_function_{}", function.wasm_function_index).into_bytes(),
            value: function.executable_offset as u64,
            size: function.executable_len as u64,
            kind: SymbolKind::Text,
            scope: SymbolScope::Compilation,
            weak: false,
            section: SymbolSection::Section(text_section),
            flags: SymbolFlags::None,
        });
        object.add_symbol(ElfSymbol {
            name: linked_wasm_function_symbol_name(
                &compiled.aot.module_id,
                function.wasm_function_index,
            )
            .into_bytes(),
            value: function.executable_offset as u64,
            size: function.executable_len as u64,
            kind: SymbolKind::Text,
            scope: SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Section(text_section),
            flags: SymbolFlags::None,
        });
        ret.push((function.wasm_function_index, symbol));
    }
    Ok(ret)
}

pub(crate) fn linked_wasm_function_symbol_name(
    module_id: &ModuleId,
    wasm_function_index: Index,
) -> String {
    let mut module_id_hex = String::with_capacity(module_id.len() * 2);
    for byte in module_id {
        use std::fmt::Write;

        let _ = write!(&mut module_id_hex, "{byte:02x}");
    }
    format!("razero_wasm_function_{module_id_hex}_{wasm_function_index}")
}

fn normalize_x86_64_relocation_sites(
    text_bytes: &mut [u8],
    relocations: &[AotRelocationMetadata],
) -> Result<(), ObjectEmissionError> {
    for relocation in relocations {
        let offset = usize::try_from(relocation.executable_offset).map_err(|_| {
            ObjectEmissionError::new(format!(
                "invalid negative relocation offset {}",
                relocation.executable_offset
            ))
        })?;
        let text_len = text_bytes.len();
        let range = text_bytes.get_mut(offset + 1..offset + 5).ok_or_else(|| {
            ObjectEmissionError::new(format!(
                "relocation site at {offset} is out of bounds for {} bytes",
                text_len
            ))
        })?;
        range.fill(0);
    }
    Ok(())
}

fn x86_64_relocation_offset(
    relocation: &AotRelocationMetadata,
) -> Result<u64, ObjectEmissionError> {
    let base = u64::try_from(relocation.executable_offset).map_err(|_| {
        ObjectEmissionError::new(format!(
            "invalid negative relocation offset {}",
            relocation.executable_offset
        ))
    })?;
    Ok(base + 1)
}

fn sanitize_symbol_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Clone)]
struct CompiledModuleWithCount {
    compiled_module: Arc<CompiledModule>,
    ref_count: usize,
}

pub struct CompilerEngine {
    version: String,
    compiled_modules: HashMap<ModuleId, CompiledModuleWithCount>,
    sorted_compiled_modules: Vec<Arc<CompiledModule>>,
    shared_functions: Arc<SharedFunctions>,
    cache: Option<Arc<dyn CompiledModuleCache>>,
    secure_mode: bool,
    fuel: i64,
}

impl Default for CompilerEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl CompilerEngine {
    pub fn new() -> Self {
        Self::with_cache_secure_mode_and_fuel(None, false, 0)
    }

    pub fn with_secure_mode(secure_mode: bool) -> Self {
        Self::with_cache_secure_mode_and_fuel(None, secure_mode, 0)
    }

    pub fn with_fuel(fuel: i64) -> Self {
        Self::with_cache_secure_mode_and_fuel(None, false, fuel)
    }

    pub fn with_secure_mode_and_fuel(secure_mode: bool, fuel: i64) -> Self {
        Self::with_cache_secure_mode_and_fuel(None, secure_mode, fuel)
    }

    pub fn with_cache(cache: Option<Arc<dyn CompiledModuleCache>>) -> Self {
        Self::with_cache_secure_mode_and_fuel(cache, false, 0)
    }

    pub fn with_cache_and_secure_mode(
        cache: Option<Arc<dyn CompiledModuleCache>>,
        secure_mode: bool,
    ) -> Self {
        Self::with_cache_secure_mode_and_fuel(cache, secure_mode, 0)
    }

    fn with_cache_secure_mode_and_fuel(
        cache: Option<Arc<dyn CompiledModuleCache>>,
        secure_mode: bool,
        fuel: i64,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            compiled_modules: HashMap::new(),
            sorted_compiled_modules: Vec::new(),
            shared_functions: Arc::new(compile_shared_functions()),
            cache,
            secure_mode,
            fuel: fuel.max(0),
        }
    }

    pub fn compiled_module(&self, module: &Module) -> Option<Arc<CompiledModule>> {
        self.compiled_modules
            .get(&module.id)
            .map(|entry| entry.compiled_module.clone())
    }

    pub fn precompiled_module_artifact(
        &self,
        module: &Module,
    ) -> Option<PrecompiledModuleArtifact> {
        if self.fuel_enabled() {
            return None;
        }
        let compiled = self.compiled_module(module)?;
        Some(PrecompiledModuleArtifact {
            executable: compiled.executables.executable.as_slice().to_vec(),
            function_offsets: compiled.function_offsets.clone(),
            source_map: compiled.source_map.clone(),
            aot: compiled.aot.clone(),
        })
    }

    pub fn compiled_module_of_addr(&self, addr: usize) -> Option<Arc<CompiledModule>> {
        let index = self.sorted_compiled_modules.partition_point(|candidate| {
            candidate.executables.executable.as_ptr() as usize <= addr
        });
        let candidate = self.sorted_compiled_modules.get(index.checked_sub(1)?)?;
        let base = candidate.executables.executable.as_ptr() as usize;
        let end = base + candidate.executables.executable.len();
        (base <= addr && addr < end).then_some(candidate.clone())
    }

    fn add_compiled_module(
        &mut self,
        module: &Module,
        compiled: Arc<CompiledModule>,
    ) -> Arc<CompiledModule> {
        if let Some(existing) = self.compiled_modules.get_mut(&module.id) {
            existing.ref_count += 1;
            return existing.compiled_module.clone();
        }
        if !compiled.executables.executable.is_empty() {
            self.add_compiled_module_to_sorted_list(compiled.clone());
        }
        self.compiled_modules.insert(
            module.id,
            CompiledModuleWithCount {
                compiled_module: compiled.clone(),
                ref_count: 1,
            },
        );
        compiled
    }

    fn add_compiled_module_to_sorted_list(&mut self, compiled: Arc<CompiledModule>) {
        let ptr = compiled.executables.executable.as_ptr() as usize;
        let index = self
            .sorted_compiled_modules
            .partition_point(|existing| (existing.executables.executable.as_ptr() as usize) < ptr);
        self.sorted_compiled_modules.insert(index, compiled);
    }

    fn delete_compiled_module_from_sorted_list(&mut self, compiled: &Arc<CompiledModule>) {
        let ptr = compiled.executables.executable.as_ptr() as usize;
        if let Some(index) = self
            .sorted_compiled_modules
            .iter()
            .position(|candidate| candidate.executables.executable.as_ptr() as usize == ptr)
        {
            self.sorted_compiled_modules.remove(index);
        }
    }

    fn load_from_cache(&self, module: &Module) -> Result<Option<Arc<CompiledModule>>, EngineError> {
        let Some(cache) = &self.cache else {
            return Ok(None);
        };
        let cache_key =
            file_cache_key(module, self.memory_isolation_enabled(), self.fuel_enabled());
        let Some(bytes) = cache.get(&cache_key) else {
            return Ok(None);
        };
        let Some(cached) = deserialize_compiled_module(&self.version, &bytes)
            .map_err(|err| EngineError::new(err.to_string()))?
        else {
            cache.delete(&cache_key);
            return Ok(None);
        };
        Ok(Some(Arc::new(
            self.cached_module_into_compiled(module, cached)?,
        )))
    }

    fn cached_module_into_compiled(
        &self,
        module: &Module,
        cached: CachedCompiledModule,
    ) -> Result<CompiledModule, EngineError> {
        let memory_isolation_enabled = self.memory_isolation_enabled();
        let mut executables = cached.executables;
        executables
            .executable
            .make_executable()
            .map_err(|err| EngineError::new(err.to_string()))?;
        let (entry_preambles, entry_preamble_offsets) = compile_entry_preambles(module)?;
        executables.entry_preambles = entry_preambles;
        executables.entry_preamble_offsets = entry_preamble_offsets;
        if memory_isolation_enabled && !executables.executable.is_empty() {
            let start = executables.executable.as_ptr() as usize;
            let end = start + executables.executable.len();
            signal_handler::register_jit_code_range(start, end);
        }
        Ok(CompiledModule {
            executables,
            function_offsets: cached.function_offsets,
            module: module.clone(),
            offsets: ModuleContextOffsetData::new(module, false),
            aot: cached.aot,
            shared_functions: self.shared_functions.clone(),
            ensure_termination: module.ensure_termination,
            fuel_enabled: self.fuel_enabled(),
            fuel: self.fuel,
            memory_isolation_enabled,
            source_map: cached.source_map,
        })
    }

    fn precompiled_module_into_compiled(
        &self,
        module: &Module,
        artifact: PrecompiledModuleArtifact,
    ) -> Result<CompiledModule, EngineError> {
        if artifact.aot.module_id != module.id {
            return Err(EngineError::new(
                "precompiled artifact does not match module identity",
            ));
        }
        self.cached_module_into_compiled(
            module,
            CachedCompiledModule {
                executables: Executables {
                    executable: AlignedBytes::from_bytes(artifact.executable),
                    ..Executables::default()
                },
                function_offsets: artifact.function_offsets,
                source_map: artifact.source_map,
                aot: artifact.aot,
            },
        )
    }

    fn store_in_cache(&self, module: &Module, compiled: &CompiledModule) {
        if module.is_host_module {
            return;
        }
        if let Some(cache) = &self.cache {
            let bytes = serialize_compiled_module(
                &self.version,
                compiled.executables.executable.as_slice(),
                &compiled.function_offsets,
                &compiled.source_map,
                &compiled.aot,
            );
            cache.insert(
                file_cache_key(
                    module,
                    compiled.memory_isolation_enabled,
                    compiled.fuel_enabled,
                ),
                bytes,
            );
        }
    }

    fn fuel_enabled(&self) -> bool {
        self.fuel > 0
    }

    fn memory_isolation_enabled(&self) -> bool {
        self.secure_mode && signal_handler::signal_handler_supported()
    }

    fn compile_module_impl(
        &self,
        module: &Module,
        options: &CompileOptions,
    ) -> Result<Arc<CompiledModule>, EngineError> {
        if module.is_host_module {
            self.compile_host_module(module)
        } else {
            self.compile_wasm_module(module, options)
        }
    }

    fn compile_host_module(&self, module: &Module) -> Result<Arc<CompiledModule>, EngineError> {
        let mut function_offsets = Vec::with_capacity(module.code_section.len());
        let mut executable = Vec::new();

        for (index, code) in module.code_section.iter().enumerate() {
            let host_func = code.host_func.as_ref().ok_or_else(|| {
                EngineError::new("host module function missing host implementation")
            })?;
            let mut signature = Signature::new(
                SignatureId(module.function_section[index]),
                vec![Type::I64, Type::I64],
                vec![],
            );
            let mut typ = module.type_section[module.function_section[index] as usize].clone();
            typ.cache_num_in_u64();
            signature
                .params
                .extend(typ.params.iter().copied().map(wasm_type_to_ssa_type));
            signature
                .results
                .extend(typ.results.iter().copied().map(wasm_type_to_ssa_type));
            let exit_code = if Arc::strong_count(host_func) > 0 {
                ExitCode::call_go_module_function_with_index(index, false)
            } else {
                ExitCode::call_go_function_with_index(index, false)
            };
            align16_vec(&mut executable);
            function_offsets.push(executable.len());
            executable.extend(native_compile_host_trampoline(exit_code, &signature)?);
        }
        let mut executable = AlignedBytes::from_bytes(executable);
        executable
            .make_executable()
            .map_err(|err| EngineError::new(err.to_string()))?;

        Ok(Arc::new(CompiledModule {
            executables: Executables {
                executable,
                ..Executables::default()
            },
            function_offsets,
            module: module.clone(),
            offsets: ModuleContextOffsetData::new(module, false),
            aot: AotCompiledMetadata::default(),
            shared_functions: self.shared_functions.clone(),
            ensure_termination: false,
            fuel_enabled: false,
            fuel: 0,
            memory_isolation_enabled: false,
            source_map: SourceMap::default(),
        }))
    }

    fn compile_wasm_module(
        &self,
        module: &Module,
        options: &CompileOptions,
    ) -> Result<Arc<CompiledModule>, EngineError> {
        let memory_isolation_enabled = self.memory_isolation_enabled();
        let fuel_enabled = self.fuel_enabled();
        let executables = compile_entry_preambles(module)?;
        let mut function_offsets = Vec::with_capacity(module.code_section.len());
        let mut function_metadata = Vec::with_capacity(module.code_section.len());
        let mut executable = Vec::new();
        let mut relocations = Vec::<(usize, Vec<RelocationInfo>)>::new();
        let mut aot_relocations = Vec::new();
        let mut source_map = SourceMap::default();
        let compiled_functions = compile_functions(
            module,
            options.workers(),
            fuel_enabled,
            memory_isolation_enabled,
        )?;

        for (local_index, compiled) in compiled_functions.into_iter().enumerate() {
            align16_vec(&mut executable);
            let function_offset = executable.len();
            let wasm_function_index = module.import_function_count + local_index as u32;
            function_offsets.push(function_offset);
            function_metadata.push(AotFunctionMetadata {
                local_function_index: local_index as u32,
                wasm_function_index,
                type_index: module
                    .function_section
                    .get(local_index)
                    .copied()
                    .unwrap_or_default(),
                executable_offset: function_offset,
                executable_len: compiled.code.len(),
            });
            source_map.executable_offsets.extend(
                compiled
                    .source_offsets
                    .iter()
                    .map(|info| function_offset + info.executable_offset as usize),
            );
            source_map.wasm_binary_offsets.extend(
                compiled
                    .source_offsets
                    .iter()
                    .map(|info| info.source_offset.0 as u64),
            );
            aot_relocations.extend(relocations_for_function(
                wasm_function_index,
                function_offset,
                &compiled.relocations,
            ));
            executable.extend(compiled.code);
            relocations.push((function_offset, compiled.relocations));
        }

        let mut executable = AlignedBytes::from_bytes(executable);
        let mut ref_to_binary_offset =
            vec![0i32; module.import_function_count as usize + function_offsets.len()];
        for (local_index, offset) in function_offsets.iter().copied().enumerate() {
            ref_to_binary_offset[module.import_function_count as usize + local_index] =
                offset as i32;
        }
        for (function_offset, relocs) in relocations {
            native_resolve_relocations(
                &ref_to_binary_offset,
                module.import_function_count as usize,
                executable.as_mut_slice(),
                &relocs,
                &[],
                function_offset,
            );
        }
        executable
            .make_executable()
            .map_err(|err| EngineError::new(err.to_string()))?;

        let entry_preamble_offsets = executables.1;
        let compiled = CompiledModule {
            executables: Executables {
                executable,
                entry_preambles: executables.0,
                entry_preamble_offsets: entry_preamble_offsets.clone(),
            },
            function_offsets,
            module: module.clone(),
            offsets: ModuleContextOffsetData::new(module, false),
            aot: AotCompiledMetadata::new(
                module,
                entry_preamble_offsets,
                function_metadata,
                aot_relocations,
                ModuleContextOffsetData::new(module, false),
                source_map
                    .wasm_binary_offsets
                    .iter()
                    .copied()
                    .zip(source_map.executable_offsets.iter().copied())
                    .map(
                        |(wasm_binary_offset, executable_offset)| AotSourceMapEntry {
                            wasm_binary_offset,
                            executable_offset,
                        },
                    )
                    .collect(),
                memory_isolation_enabled,
            ),
            shared_functions: self.shared_functions.clone(),
            ensure_termination: module.ensure_termination,
            fuel_enabled,
            fuel: self.fuel,
            memory_isolation_enabled,
            source_map,
        };
        if compiled.memory_isolation_enabled && !compiled.executables.executable.is_empty() {
            let start = compiled.executables.executable.as_ptr() as usize;
            let end = start + compiled.executables.executable.len();
            signal_handler::register_jit_code_range(start, end);
        }
        Ok(Arc::new(compiled))
    }
}

impl WasmEngine for CompilerEngine {
    fn close(&mut self) -> Result<(), EngineError> {
        self.compiled_modules.clear();
        self.sorted_compiled_modules.clear();
        self.shared_functions = Arc::new(SharedFunctions::default());
        Ok(())
    }

    fn compile_module(&mut self, module: &Module) -> Result<(), EngineError> {
        self.compile_module_with_options(module, &CompileOptions::default())
    }

    fn compile_module_with_options(
        &mut self,
        module: &Module,
        options: &CompileOptions,
    ) -> Result<(), EngineError> {
        if self.compiled_modules.contains_key(&module.id) {
            if let Some(existing) = self.compiled_modules.get_mut(&module.id) {
                existing.ref_count += 1;
            }
            return Ok(());
        }
        if let Some(cached) = self.load_from_cache(module)? {
            self.add_compiled_module(module, cached);
            return Ok(());
        }
        let compiled = self.compile_module_impl(module, options)?;
        self.store_in_cache(module, &compiled);
        self.add_compiled_module(module, compiled);
        Ok(())
    }

    fn load_precompiled_module(
        &mut self,
        module: &Module,
        artifact: &[u8],
    ) -> Result<(), EngineError> {
        if self.compiled_modules.contains_key(&module.id) {
            return Ok(());
        }
        if self.fuel_enabled() {
            return Err(EngineError::new(
                "fuel-enabled compiler runtimes do not yet support precompiled artifacts",
            ));
        }
        let Some(artifact) = PrecompiledModuleArtifact::deserialize(artifact)
            .map_err(|err| EngineError::new(err.to_string()))?
        else {
            return Err(EngineError::new("precompiled artifact version mismatch"));
        };
        let compiled = Arc::new(self.precompiled_module_into_compiled(module, artifact)?);
        self.add_compiled_module(module, compiled);
        Ok(())
    }

    fn precompiled_module_bytes(&self, module: &Module) -> Option<Vec<u8>> {
        self.precompiled_module_artifact(module)
            .map(|artifact| artifact.serialize())
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
        let compiled = entry.compiled_module.clone();
        self.delete_compiled_module_from_sorted_list(&compiled);
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
        let mut module_engine = Box::new(CompilerModuleEngine::new(compiled, instance.clone()));
        module_engine.done_instantiation();
        Ok(module_engine)
    }
}

impl ModuleContextOffsetSource for Module {
    fn has_memory(&self) -> bool {
        self.memory_section.is_some()
    }

    fn import_memory_count(&self) -> u32 {
        self.import_memory_count
    }

    fn import_function_count(&self) -> u32 {
        self.import_function_count
    }

    fn import_global_count(&self) -> u32 {
        self.import_global_count
    }

    fn global_count(&self) -> usize {
        self.global_section.len()
    }

    fn import_table_count(&self) -> u32 {
        self.import_table_count
    }

    fn table_count(&self) -> usize {
        self.table_section.len()
    }
}

#[derive(Debug)]
struct CompiledFunction {
    code: Vec<u8>,
    relocations: Vec<RelocationInfo>,
    source_offsets: Vec<SourceOffsetInfo>,
}

fn compile_function(
    module: &Module,
    local_index: usize,
    fuel_enabled: bool,
    memory_isolation_enabled: bool,
) -> Result<CompiledFunction, EngineError> {
    #[cfg(test)]
    note_compile_thread();

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let mut frontend = FrontendCompiler::new(
            module,
            Builder::new(),
            Some(ModuleContextOffsetData::new(module, false)),
            module.ensure_termination,
            false,
            module.dwarf_lines.is_some(),
            fuel_enabled,
            memory_isolation_enabled,
        );
        frontend
            .init_with_module_function(module.import_function_count + local_index as u32, false);
        frontend.lower_to_ssa();
        let builder = std::mem::take(frontend.builder_mut());
        let mut backend = BackendCompiler::new(NativeMachine::new(), builder);
        let output = backend
            .compile()
            .map_err(|err| EngineError::new(err.to_string()))?;
        Ok(CompiledFunction {
            code: output.code,
            relocations: output.relocations,
            source_offsets: backend.source_offsets.clone(),
        })
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = (module, local_index, fuel_enabled, memory_isolation_enabled);
        Err(EngineError::new("unsupported architecture"))
    }
}

fn compile_functions(
    module: &Module,
    workers: usize,
    fuel_enabled: bool,
    memory_isolation_enabled: bool,
) -> Result<Vec<CompiledFunction>, EngineError> {
    let function_count = module.code_section.len();
    if workers <= 1 || function_count <= 1 {
        return (0..function_count)
            .map(|local_index| {
                compile_function(module, local_index, fuel_enabled, memory_isolation_enabled)
                    .map_err(|err| {
                        EngineError::new(format!(
                            "compile function {local_index}/{}: {err}",
                            function_count.saturating_sub(1)
                        ))
                    })
            })
            .collect();
    }

    let results = Mutex::new((0..function_count).map(|_| None).collect::<Vec<_>>());
    let first_error = Mutex::new(None);
    let next_index = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if first_error
                    .lock()
                    .expect("compile error state poisoned")
                    .is_some()
                {
                    return;
                }

                let local_index = next_index.fetch_add(1, Ordering::Relaxed);
                if local_index >= function_count {
                    return;
                }

                match compile_function(module, local_index, fuel_enabled, memory_isolation_enabled)
                {
                    Ok(compiled) => {
                        results.lock().expect("compile results poisoned")[local_index] =
                            Some(compiled);
                    }
                    Err(err) => {
                        let mut first = first_error.lock().expect("compile error state poisoned");
                        if first.is_none() {
                            *first = Some(EngineError::new(format!(
                                "compile function {local_index}/{}: {err}",
                                function_count.saturating_sub(1)
                            )));
                        }
                        return;
                    }
                }
            });
        }
    });

    if let Some(err) = first_error
        .into_inner()
        .expect("compile error state poisoned")
    {
        return Err(err);
    }

    results
        .into_inner()
        .expect("compile results poisoned")
        .into_iter()
        .enumerate()
        .map(|(local_index, compiled)| {
            compiled.ok_or_else(|| {
                EngineError::new(format!(
                    "compile function {local_index}/{}: worker exited before producing code",
                    function_count.saturating_sub(1)
                ))
            })
        })
        .collect()
}

#[cfg(test)]
type CompileThreadObserver = Arc<dyn Fn(ThreadId) + Send + Sync>;

#[cfg(test)]
fn compile_thread_observer() -> &'static Mutex<Option<CompileThreadObserver>> {
    static OBSERVER: OnceLock<Mutex<Option<CompileThreadObserver>>> = OnceLock::new();
    OBSERVER.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn note_compile_thread() {
    if let Some(observer) = compile_thread_observer()
        .lock()
        .expect("compile thread observer poisoned")
        .clone()
    {
        observer(thread::current().id());
    }
}

#[cfg(test)]
fn set_compile_thread_observer(observer: Option<CompileThreadObserver>) {
    *compile_thread_observer()
        .lock()
        .expect("compile thread observer poisoned") = observer;
}

fn compile_entry_preambles(module: &Module) -> Result<(AlignedBytes, Vec<usize>), EngineError> {
    let mut bytes = Vec::new();
    let mut offsets = Vec::with_capacity(module.type_section.len());
    for function_type in &module.type_section {
        let signature = signature_for_wasm_function_type(function_type);
        align16_vec(&mut bytes);
        offsets.push(bytes.len());
        bytes.extend(native_compile_entry_preamble(&signature)?);
    }
    let mut bytes = AlignedBytes::from_bytes(bytes);
    bytes
        .make_executable()
        .map_err(|err| EngineError::new(err.to_string()))?;
    Ok((bytes, offsets))
}

fn compile_shared_functions() -> SharedFunctions {
    let mut executable = Vec::new();
    let mut record = |buf: Vec<u8>, slot: &mut Option<usize>| {
        align16_vec(&mut executable);
        *slot = Some(executable.len());
        executable.extend(buf);
    };

    let mut shared = SharedFunctions::default();
    record(
        native_compile_host_trampoline(
            ExitCode::GROW_MEMORY,
            &Signature::new(SignatureId(0), vec![Type::I64, Type::I32], vec![Type::I32]),
        )
        .unwrap_or_default(),
        &mut shared.memory_grow_offset,
    );
    record(
        native_compile_host_trampoline(
            ExitCode::TABLE_GROW,
            &Signature::new(
                SignatureId(1),
                vec![Type::I64, Type::I32, Type::I32, Type::I64],
                vec![Type::I32],
            ),
        )
        .unwrap_or_default(),
        &mut shared.table_grow_offset,
    );
    record(
        native_compile_host_trampoline(
            ExitCode::CHECK_MODULE_EXIT_CODE,
            &Signature::new(SignatureId(2), vec![Type::I64], vec![]),
        )
        .unwrap_or_default(),
        &mut shared.check_module_exit_code_offset,
    );
    record(
        native_compile_host_trampoline(
            ExitCode::REF_FUNC,
            &Signature::new(SignatureId(3), vec![Type::I64, Type::I32], vec![Type::I64]),
        )
        .unwrap_or_default(),
        &mut shared.ref_func_offset,
    );
    record(
        native_compile_stack_grow_sequence().unwrap_or_default(),
        &mut shared.stack_grow_offset,
    );
    record(
        native_compile_host_trampoline(
            ExitCode::MEMORY_WAIT32,
            &Signature::new(
                SignatureId(4),
                vec![Type::I64, Type::I64, Type::I32, Type::I64],
                vec![Type::I32],
            ),
        )
        .unwrap_or_default(),
        &mut shared.memory_wait32_offset,
    );
    record(
        native_compile_host_trampoline(
            ExitCode::MEMORY_WAIT64,
            &Signature::new(
                SignatureId(5),
                vec![Type::I64, Type::I64, Type::I64, Type::I64],
                vec![Type::I32],
            ),
        )
        .unwrap_or_default(),
        &mut shared.memory_wait64_offset,
    );
    record(
        native_compile_host_trampoline(
            ExitCode::MEMORY_NOTIFY,
            &Signature::new(
                SignatureId(6),
                vec![Type::I64, Type::I32, Type::I64],
                vec![Type::I32],
            ),
        )
        .unwrap_or_default(),
        &mut shared.memory_notify_offset,
    );
    shared.executable = AlignedBytes::from_bytes(executable);
    shared
        .executable
        .make_executable()
        .expect("shared trampolines must map executable");
    shared
}

fn align16_vec(bytes: &mut Vec<u8>) {
    let rem = bytes.len() & 15;
    if rem != 0 {
        bytes.resize(bytes.len() + (16 - rem), 0);
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn native_compile_host_trampoline(
    exit_code: ExitCode,
    signature: &Signature,
) -> Result<Vec<u8>, EngineError> {
    let mut machine = NativeMachine::new();
    Ok(machine.compile_host_function_trampoline(exit_code, signature, true))
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn native_compile_host_trampoline(
    _exit_code: ExitCode,
    _signature: &Signature,
) -> Result<Vec<u8>, EngineError> {
    Err(EngineError::new("unsupported architecture"))
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn native_compile_stack_grow_sequence() -> Result<Vec<u8>, EngineError> {
    let mut machine = NativeMachine::new();
    Ok(machine.compile_stack_grow_call_sequence())
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn native_compile_stack_grow_sequence() -> Result<Vec<u8>, EngineError> {
    Err(EngineError::new("unsupported architecture"))
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn native_compile_entry_preamble(signature: &Signature) -> Result<Vec<u8>, EngineError> {
    let mut machine = NativeMachine::new();
    Ok(machine.compile_entry_preamble(signature, false))
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn native_compile_entry_preamble(_signature: &Signature) -> Result<Vec<u8>, EngineError> {
    Err(EngineError::new("unsupported architecture"))
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn native_resolve_relocations(
    ref_to_binary_offset: &[i32],
    imported_fns: usize,
    executable: &mut [u8],
    relocations: &[RelocationInfo],
    call_trampoline_island_offsets: &[i32],
    function_offset: usize,
) {
    if relocations.is_empty() {
        return;
    }
    let mut adjusted = relocations.to_vec();
    for relocation in &mut adjusted {
        relocation.offset += function_offset as i64;
    }
    let mut machine = NativeMachine::new();
    machine.resolve_relocations(
        ref_to_binary_offset,
        imported_fns,
        executable,
        &adjusted,
        call_trampoline_island_offsets,
    );
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn native_resolve_relocations(
    _ref_to_binary_offset: &[i32],
    _imported_fns: usize,
    _executable: &mut [u8],
    _relocations: &[RelocationInfo],
    _call_trampoline_island_offsets: &[i32],
    _function_offset: usize,
) {
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::{Arc, Mutex},
    };

    use object::{Object as _, ObjectKind, ObjectSection as _, SectionKind};

    use crate::aot::{
        deserialize_aot_metadata, AotCompiledMetadata, AotExecutionContextMetadata,
        AotFunctionMetadata, AotHelperId, AotRelocationMetadata, AotTargetArchitecture,
        AotTargetOperatingSystem,
    };
    use crate::engine_cache::InMemoryCompiledModuleCache;
    use crate::wazevoapi::ModuleContextOffsetData;
    use razero_wasm::engine::{CompileOptions, Engine as _};
    use razero_wasm::host_func::stack_host_func;
    use razero_wasm::module::{
        Code, CodeBody, FunctionType, Global, GlobalType, Import, Memory, Module, RefType, Table,
        ValueType,
    };

    use super::{
        compile_function, compile_shared_functions, linked_wasm_function_symbol_name,
        set_compile_thread_observer, signal_handler, AlignedBytes, CompiledModule, CompilerEngine,
        Executables, SharedFunctions, SourceMap,
    };

    fn function_type(params: &[ValueType], results: &[ValueType]) -> FunctionType {
        let mut ty = FunctionType::default();
        ty.params.extend_from_slice(params);
        ty.results.extend_from_slice(results);
        ty.cache_num_in_u64();
        ty
    }

    #[test]
    fn compile_module_caches_and_reuses_compilation() {
        let cache = Arc::new(InMemoryCompiledModuleCache::default());
        let mut engine = CompilerEngine::with_cache(Some(cache));
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
        assert_eq!(engine.compiled_module_count(), 1);
        engine.compile_module(&module).unwrap();
        assert_eq!(engine.compiled_module_count(), 1);
    }

    #[test]
    fn warm_cache_restores_entry_preambles_and_memory_isolation() {
        let cache = Arc::new(InMemoryCompiledModuleCache::default());
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x0b],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut cold_engine = CompilerEngine::with_cache_and_secure_mode(Some(cache.clone()), true);
        cold_engine.compile_module(&module).unwrap();

        let mut warm_engine = CompilerEngine::with_cache_and_secure_mode(Some(cache), true);
        warm_engine.compile_module(&module).unwrap();
        let compiled = warm_engine.compiled_module(&module).unwrap();

        assert!(compiled.entry_preamble_ptr(0).is_some());
        assert_eq!(
            compiled.memory_isolation_enabled,
            signal_handler::signal_handler_supported()
        );
        assert_eq!(compiled.aot.types.len(), 1);
        assert_eq!(compiled.aot.types[0].params, vec![ValueType::I32]);
        assert_eq!(compiled.aot.types[0].results, vec![ValueType::I32]);
        assert_eq!(compiled.aot.module_shape.local_function_count, 1);
        assert_eq!(compiled.aot.module_shape.import_function_count, 0);
        assert!(!compiled.aot.module_shape.has_any_memory);
        assert_eq!(
            compiled.aot.execution_context,
            AotExecutionContextMetadata::current()
        );
        assert_eq!(compiled.aot.helpers.len(), 9);
        assert_eq!(compiled.aot.helpers[0].id, AotHelperId::MemoryGrow);
        assert_eq!(compiled.aot.helpers[5].id, AotHelperId::Memmove);
    }

    #[test]
    fn warm_cache_restores_import_and_abi_descriptors() {
        let cache = Arc::new(InMemoryCompiledModuleCache::default());
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            import_section: vec![
                Import::function("env", "host", 0),
                Import::memory(
                    "env",
                    "memory",
                    Memory {
                        min: 1,
                        cap: 2,
                        max: 3,
                        is_max_encoded: true,
                        is_shared: false,
                    },
                ),
                Import::table(
                    "env",
                    "table",
                    Table {
                        min: 4,
                        max: Some(7),
                        ty: RefType::FUNCREF,
                    },
                ),
                Import::global(
                    "env",
                    "global",
                    GlobalType {
                        val_type: ValueType::I64,
                        mutable: true,
                    },
                ),
            ],
            import_function_count: 1,
            import_memory_count: 1,
            import_table_count: 1,
            import_global_count: 1,
            table_section: vec![Table {
                min: 8,
                max: Some(10),
                ty: RefType::EXTERNREF,
            }],
            memory_section: Some(Memory {
                min: 2,
                cap: 2,
                max: 5,
                is_max_encoded: true,
                is_shared: false,
            }),
            global_section: vec![Global {
                ty: GlobalType {
                    val_type: ValueType::I32,
                    mutable: false,
                },
                ..Global::default()
            }],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x0b],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut cold_engine = CompilerEngine::with_cache(Some(cache.clone()));
        cold_engine.compile_module(&module).unwrap();

        let mut warm_engine = CompilerEngine::with_cache(Some(cache));
        warm_engine.compile_module(&module).unwrap();
        let compiled = warm_engine.compiled_module(&module).unwrap();

        assert_eq!(compiled.aot.imports.len(), 4);
        assert_eq!(compiled.aot.imports[0].module, "env");
        assert_eq!(compiled.aot.imports[0].name, "host");
        assert_eq!(compiled.aot.imports[1].name, "memory");
        assert_eq!(compiled.aot.imports[1].index_per_type, 0);
        assert_eq!(compiled.aot.imports[2].name, "table");
        assert_eq!(compiled.aot.imports[3].name, "global");
        assert_eq!(compiled.aot.imports[3].index_per_type, 0);
        assert_eq!(compiled.aot.memory.as_ref().unwrap().max, 5);
        assert_eq!(compiled.aot.tables[0].max, Some(10));
        assert_eq!(compiled.aot.tables[0].ty, RefType::EXTERNREF);
        assert_eq!(compiled.aot.globals[0].val_type, ValueType::I32);
        assert!(!compiled.aot.globals[0].mutable);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn compile_module_uses_multiple_workers_when_requested() {
        static TEST_LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
        let _guard = TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("compile observer test lock poisoned");

        let seen_threads = Arc::new(Mutex::new(HashSet::<String>::new()));
        set_compile_thread_observer(Some(Arc::new({
            let seen_threads = seen_threads.clone();
            move |thread_id| {
                seen_threads
                    .lock()
                    .expect("seen threads poisoned")
                    .insert(format!("{thread_id:?}"));
            }
        })));

        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0; 16],
            code_section: vec![
                Code {
                    body: vec![0x20, 0x00, 0x0b],
                    ..Code::default()
                };
                16
            ],
            ..Module::default()
        };

        let mut engine = CompilerEngine::new();
        engine
            .compile_module_with_options(&module, &CompileOptions::new(4))
            .unwrap();

        set_compile_thread_observer(None);
        assert!(
            seen_threads.lock().expect("seen threads poisoned").len() > 1,
            "expected compilation to span multiple worker threads"
        );
    }

    #[test]
    fn host_module_compilation_emits_trampolines() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            is_host_module: true,
            type_section: vec![function_type(&[ValueType::I64], &[ValueType::I64])],
            function_section: vec![0],
            code_section: vec![Code {
                body_kind: CodeBody::Host,
                host_func: Some(stack_host_func(|stack| {
                    stack[0] += 1;
                    Ok(())
                })),
                ..Code::default()
            }],
            ..Module::default()
        };
        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        assert!(!compiled.executables.executable.is_empty());
        assert_eq!(compiled.function_offsets[0] & 15, 0);
    }

    #[test]
    fn wasm_module_compilation_emits_executable_code() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b],
                ..Code::default()
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();

        assert!(!compiled.executables.executable.is_empty());
        assert!(compiled.function_ptr(0).is_some());
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn emit_relocatable_object_emits_elf_text_symbols_and_sidecar() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            import_section: vec![Import::function("env", "host-fn", 0)],
            import_function_count: 1,
            function_section: vec![0, 0],
            code_section: vec![
                Code {
                    body: vec![0x0b],
                    ..Code::default()
                },
                Code {
                    body: vec![0x0b],
                    ..Code::default()
                },
            ],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        let artifact = compiled.emit_relocatable_object().unwrap();
        let file = object::File::parse(artifact.object_bytes.as_slice()).unwrap();
        let text = file.section_by_name(".text").unwrap();
        let expected_architecture = match AotTargetArchitecture::current() {
            AotTargetArchitecture::X86_64 => object::Architecture::X86_64,
            AotTargetArchitecture::Aarch64 => object::Architecture::Aarch64,
            other => panic!("unexpected native test architecture {other:?}"),
        };

        assert_eq!(file.kind(), ObjectKind::Relocatable);
        assert_eq!(file.architecture(), expected_architecture);
        assert_eq!(text.kind(), SectionKind::Text);
        assert_eq!(text.align(), 16);
        assert_eq!(
            text.data().unwrap(),
            compiled.executables.executable.as_slice()
        );
        assert!(file
            .symbol_by_name("razero_import_function_0_env_host_fn")
            .is_some());
        assert!(file.symbol_by_name("razero_wasm_function_1").is_some());
        assert!(file.symbol_by_name("razero_wasm_function_2").is_some());
        assert!(file
            .symbol_by_name(&linked_wasm_function_symbol_name(
                &compiled.aot.module_id,
                1
            ))
            .is_some());
        assert!(file
            .symbol_by_name(&linked_wasm_function_symbol_name(
                &compiled.aot.module_id,
                2
            ))
            .is_some());
        assert_eq!(text.relocations().count(), 0);

        let decoded = deserialize_aot_metadata(&artifact.metadata_sidecar_bytes).unwrap();
        assert_eq!(decoded, compiled.aot);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn emit_relocatable_object_preserves_x86_64_call_relocations() {
        let compiled = CompiledModule {
            executables: Executables::from_executable_bytes(vec![
                0xE8, 0x11, 0x22, 0x33, 0x44, 0xC3, 0x90, 0x90, 0xC3,
            ]),
            function_offsets: vec![0, 8],
            module: Module::default(),
            offsets: ModuleContextOffsetData::default(),
            aot: AotCompiledMetadata {
                import_function_count: 0,
                functions: vec![
                    AotFunctionMetadata {
                        local_function_index: 0,
                        wasm_function_index: 0,
                        type_index: 0,
                        executable_offset: 0,
                        executable_len: 6,
                    },
                    AotFunctionMetadata {
                        local_function_index: 1,
                        wasm_function_index: 1,
                        type_index: 0,
                        executable_offset: 8,
                        executable_len: 1,
                    },
                ],
                relocations: vec![AotRelocationMetadata {
                    source_wasm_function_index: 0,
                    target_function_index: 1,
                    executable_offset: 0,
                    is_tail_call: false,
                }],
                ..AotCompiledMetadata::default()
            },
            shared_functions: Arc::new(SharedFunctions::default()),
            ensure_termination: false,
            fuel_enabled: false,
            fuel: 0,
            memory_isolation_enabled: false,
            source_map: SourceMap::default(),
        };

        let artifact = compiled.emit_relocatable_object().unwrap();
        let file = object::File::parse(artifact.object_bytes.as_slice()).unwrap();
        let text = file.section_by_name(".text").unwrap();
        let relocations: Vec<_> = text.relocations().collect();

        assert_eq!(&text.data().unwrap()[..5], &[0xE8, 0, 0, 0, 0]);
        assert_eq!(relocations.len(), 1);
        assert_eq!(relocations[0].0, 1);
    }

    #[test]
    fn emit_relocatable_object_preserves_aarch64_call_relocations() {
        let compiled = CompiledModule {
            executables: Executables::from_executable_bytes(vec![
                0xff, 0xff, 0xff, 0xff, 0xc0, 0x03, 0x5f, 0xd6,
            ]),
            function_offsets: vec![0, 4],
            module: Module::default(),
            offsets: ModuleContextOffsetData::default(),
            aot: AotCompiledMetadata {
                target: crate::aot::AotTarget {
                    architecture: AotTargetArchitecture::Aarch64,
                    operating_system: AotTargetOperatingSystem::Linux,
                },
                import_function_count: 0,
                functions: vec![
                    AotFunctionMetadata {
                        local_function_index: 0,
                        wasm_function_index: 0,
                        type_index: 0,
                        executable_offset: 0,
                        executable_len: 4,
                    },
                    AotFunctionMetadata {
                        local_function_index: 1,
                        wasm_function_index: 1,
                        type_index: 0,
                        executable_offset: 4,
                        executable_len: 4,
                    },
                ],
                relocations: vec![AotRelocationMetadata {
                    source_wasm_function_index: 0,
                    target_function_index: 1,
                    executable_offset: 0,
                    is_tail_call: false,
                }],
                ..AotCompiledMetadata::default()
            },
            shared_functions: Arc::new(SharedFunctions::default()),
            ensure_termination: false,
            fuel_enabled: false,
            fuel: 0,
            memory_isolation_enabled: false,
            source_map: SourceMap::default(),
        };

        let artifact = compiled.emit_relocatable_object().unwrap();
        let file = object::File::parse(artifact.object_bytes.as_slice()).unwrap();
        let text = file.section_by_name(".text").unwrap();
        let relocations: Vec<_> = text.relocations().collect();

        assert_eq!(file.architecture(), object::Architecture::Aarch64);
        assert_eq!(&text.data().unwrap()[..4], &[0x00, 0x00, 0x00, 0x94]);
        assert_eq!(relocations.len(), 1);
        assert_eq!(relocations[0].0, 0);
    }

    #[test]
    fn compiled_module_lookup_by_address_follows_sorted_ranges() {
        let mut engine = CompilerEngine::new();
        let make = |size: usize| {
            Arc::new(CompiledModule {
                executables: Executables {
                    executable: AlignedBytes::from_bytes(vec![0; size]),
                    ..Executables::default()
                },
                function_offsets: vec![0],
                module: Module::default(),
                offsets: ModuleContextOffsetData::default(),
                aot: AotCompiledMetadata::default(),
                shared_functions: Arc::new(SharedFunctions::default()),
                ensure_termination: false,
                fuel_enabled: false,
                fuel: 0,
                memory_isolation_enabled: false,
                source_map: SourceMap::default(),
            })
        };
        let first = make(32);
        let second = make(32);
        engine.add_compiled_module_to_sorted_list(first.clone());
        engine.add_compiled_module_to_sorted_list(second.clone());

        let target = if (first.executables.executable.as_ptr() as usize)
            < second.executables.executable.as_ptr() as usize
        {
            first.clone()
        } else {
            second.clone()
        };
        let addr = target.executables.executable.as_ptr() as usize + 8;
        assert!(Arc::ptr_eq(
            &engine.compiled_module_of_addr(addr).unwrap(),
            &target
        ));
    }

    #[test]
    fn shared_functions_are_16_byte_aligned() {
        let shared = compile_shared_functions();
        for ptr in [
            shared.memory_grow_ptr(),
            shared.check_module_exit_code_ptr(),
            shared.stack_grow_ptr(),
            shared.table_grow_ptr(),
            shared.ref_func_ptr(),
            shared.memory_wait32_ptr(),
            shared.memory_wait64_ptr(),
            shared.memory_notify_ptr(),
        ]
        .into_iter()
        .flatten()
        {
            assert_eq!(ptr & 15, 0);
        }
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn compile_function_handles_termination_checks_in_loop_headers() {
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x03, 0x40, 0x0c, 0x00, 0x0b, 0x0b],
                ..Code::default()
            }],
            ensure_termination: true,
            ..Module::default()
        };

        let compiled = compile_function(&module, 0, false, false).unwrap();
        assert!(!compiled.code.is_empty());
        assert!(compiled.code.windows(5).any(|window| {
            window[0] == 0xE9 && i32::from_le_bytes(window[1..5].try_into().unwrap()) < 0
        }));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn compile_module_enables_memory_isolation_only_in_secure_mode_on_supported_linux_targets() {
        let mut engine = CompilerEngine::new();
        let mut secure_engine = CompilerEngine::with_secure_mode(true);
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
        let compiled = engine.compiled_module(&module).unwrap();
        secure_engine.compile_module(&module).unwrap();
        let secure_compiled = secure_engine.compiled_module(&module).unwrap();

        assert!(!compiled.memory_isolation_enabled);
        assert!(secure_compiled.memory_isolation_enabled);
    }

    #[test]
    fn compile_module_enables_fuel_only_when_runtime_has_positive_fuel() {
        let mut engine = CompilerEngine::new();
        let mut fuel_engine = CompilerEngine::with_fuel(7);
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
        let compiled = engine.compiled_module(&module).unwrap();
        fuel_engine.compile_module(&module).unwrap();
        let fuel_compiled = fuel_engine.compiled_module(&module).unwrap();

        assert!(!compiled.fuel_enabled);
        assert_eq!(0, compiled.fuel);
        assert!(fuel_compiled.fuel_enabled);
        assert_eq!(7, fuel_compiled.fuel);
    }
}
