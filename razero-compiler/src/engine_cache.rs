#![doc = "Compiler engine cache plumbing."]

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex};

use razero_wasm::module::{Module, ModuleId};

use crate::aot::{
    deserialize_aot_metadata, serialize_aot_metadata, AotCompiledMetadata, AotMetadataError,
};
use crate::engine::{AlignedBytes, Executables, SourceMap};

const MAGIC: &[u8; 6] = b"WAZEVO";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCacheError {
    InvalidHeader(String),
    Io(String),
    ChecksumMismatch { expected: u32, actual: u32 },
    AotMetadata(String),
}

impl Display for EngineCacheError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHeader(message) | Self::Io(message) | Self::AotMetadata(message) => {
                f.write_str(message)
            }
            Self::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "compilationcache: checksum mismatch (expected {expected}, got {actual})"
                )
            }
        }
    }
}

impl std::error::Error for EngineCacheError {}

#[derive(Clone, Debug, Default)]
pub struct CachedCompiledModule {
    pub executables: Executables,
    pub function_offsets: Vec<usize>,
    pub source_map: SourceMap,
    pub aot: AotCompiledMetadata,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrecompiledModuleArtifact {
    pub executable: Vec<u8>,
    pub function_offsets: Vec<usize>,
    pub source_map: SourceMap,
    pub aot: AotCompiledMetadata,
}

impl PrecompiledModuleArtifact {
    pub fn serialize(&self) -> Vec<u8> {
        serialize_compiled_module(
            env!("CARGO_PKG_VERSION"),
            &self.executable,
            &self.function_offsets,
            &self.source_map,
            &self.aot,
        )
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Option<Self>, EngineCacheError> {
        deserialize_compiled_module(env!("CARGO_PKG_VERSION"), bytes)
            .map(|cached| cached.map(Self::from))
    }
}

impl From<CachedCompiledModule> for PrecompiledModuleArtifact {
    fn from(value: CachedCompiledModule) -> Self {
        Self {
            executable: value.executables.executable.as_slice().to_vec(),
            function_offsets: value.function_offsets,
            source_map: value.source_map,
            aot: value.aot,
        }
    }
}

pub trait CompiledModuleCache: Send + Sync {
    fn get(&self, key: &ModuleId) -> Option<Vec<u8>>;
    fn insert(&self, key: ModuleId, bytes: Vec<u8>);
    fn delete(&self, key: &ModuleId);
}

#[derive(Clone, Default)]
pub struct InMemoryCompiledModuleCache {
    inner: Arc<Mutex<BTreeMap<ModuleId, Vec<u8>>>>,
}

impl CompiledModuleCache for InMemoryCompiledModuleCache {
    fn get(&self, key: &ModuleId) -> Option<Vec<u8>> {
        self.inner.lock().unwrap().get(key).cloned()
    }

    fn insert(&self, key: ModuleId, bytes: Vec<u8>) {
        self.inner.lock().unwrap().insert(key, bytes);
    }

    fn delete(&self, key: &ModuleId) {
        self.inner.lock().unwrap().remove(key);
    }
}

pub fn file_cache_key(
    module: &Module,
    memory_isolation_enabled: bool,
    fuel_enabled: bool,
) -> ModuleId {
    let mut key = module.id;
    let arch = std::env::consts::ARCH.as_bytes();
    for (index, byte) in MAGIC.iter().chain(arch.iter()).copied().enumerate() {
        key[index % key.len()] ^= byte;
    }
    if memory_isolation_enabled {
        key[0] ^= 0x80;
    }
    if fuel_enabled {
        key[1] ^= 0x40;
    }
    key
}

pub fn serialize_compiled_module(
    version: &str,
    executable: &[u8],
    function_offsets: &[usize],
    source_map: &SourceMap,
    aot: &AotCompiledMetadata,
) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(MAGIC);
    buf.push(version.len() as u8);
    buf.extend_from_slice(version.as_bytes());
    buf.extend_from_slice(&(function_offsets.len() as u32).to_le_bytes());
    for &offset in function_offsets {
        buf.extend_from_slice(&(offset as u64).to_le_bytes());
    }
    buf.extend_from_slice(&(executable.len() as u64).to_le_bytes());
    buf.extend_from_slice(executable);
    buf.extend_from_slice(&crc32c(executable).to_le_bytes());
    if source_map.executable_offsets.is_empty() {
        buf.push(0);
    } else {
        buf.push(1);
        buf.extend_from_slice(&(source_map.executable_offsets.len() as u64).to_le_bytes());
        for (wasm_offset, executable_offset) in source_map
            .wasm_binary_offsets
            .iter()
            .copied()
            .zip(source_map.executable_offsets.iter().copied())
        {
            buf.extend_from_slice(&wasm_offset.to_le_bytes());
            buf.extend_from_slice(&(executable_offset as u64).to_le_bytes());
        }
    }
    let aot_bytes = serialize_aot_metadata(aot);
    buf.extend_from_slice(&(aot_bytes.len() as u64).to_le_bytes());
    buf.extend_from_slice(&aot_bytes);
    buf
}

pub fn deserialize_compiled_module(
    version: &str,
    bytes: &[u8],
) -> Result<Option<CachedCompiledModule>, EngineCacheError> {
    let mut cursor = Cursor::new(bytes);
    let mut magic = [0u8; 6];
    read_exact(
        &mut cursor,
        &mut magic,
        "compilationcache: invalid header length",
    )?;
    if &magic != MAGIC {
        return Err(EngineCacheError::InvalidHeader(format!(
            "compilationcache: invalid magic number: got {} but want {}",
            String::from_utf8_lossy(&magic),
            String::from_utf8_lossy(MAGIC)
        )));
    }

    let version_len = read_u8(&mut cursor)?;
    let mut cached_version = vec![0; version_len as usize];
    read_exact(
        &mut cursor,
        &mut cached_version,
        "compilationcache: invalid header length",
    )?;
    if cached_version != version.as_bytes() {
        return Ok(None);
    }

    let function_count = read_u32(&mut cursor)? as u64;
    let function_count = checked_vec_len(
        &cursor,
        function_count,
        8,
        "compilationcache: invalid function offset table length",
    )?;
    let mut function_offsets = Vec::with_capacity(function_count);
    for index in 0..function_count {
        function_offsets.push(read_u64_named(
            &mut cursor,
            &format!("compilationcache: error reading func[{index}] executable offset"),
        )? as usize);
    }

    let executable_len = read_u64_named(
        &mut cursor,
        "compilationcache: error reading executable size",
    )?;
    let executable_len = checked_vec_len(
        &cursor,
        executable_len,
        1,
        "compilationcache: invalid executable size",
    )?;
    let mut executable = vec![0; executable_len];
    read_exact(
        &mut cursor,
        &mut executable,
        &format!("compilationcache: error reading executable (len={executable_len})"),
    )?;

    let expected = crc32c(&executable);
    let actual = read_u32_named(&mut cursor, "compilationcache: could not read checksum")?;
    if expected != actual {
        return Err(EngineCacheError::ChecksumMismatch { expected, actual });
    }

    let source_map = match read_u8(&mut cursor)? {
        0 => SourceMap::default(),
        1 => {
            let len = read_u64_named(
                &mut cursor,
                "compilationcache: could not read source map length",
            )?;
            let len = checked_vec_len(
                &cursor,
                len,
                16,
                "compilationcache: invalid source map length",
            )?;
            let mut executable_offsets = Vec::with_capacity(len);
            let mut wasm_binary_offsets = Vec::with_capacity(len);
            for _ in 0..len {
                wasm_binary_offsets.push(read_u64_named(
                    &mut cursor,
                    "compilationcache: could not read wasm source offset",
                )?);
                executable_offsets.push(read_u64_named(
                    &mut cursor,
                    "compilationcache: could not read executable source offset",
                )? as usize);
            }
            SourceMap {
                executable_offsets,
                wasm_binary_offsets,
            }
        }
        other => {
            return Err(EngineCacheError::InvalidHeader(format!(
                "compilationcache: invalid source map flag {other}"
            )))
        }
    };

    let aot_len = read_u64_named(
        &mut cursor,
        "compilationcache: could not read aot metadata length",
    )?;
    let aot_len = checked_vec_len(
        &cursor,
        aot_len,
        1,
        "compilationcache: invalid aot metadata length",
    )?;
    let mut aot_bytes = vec![0; aot_len];
    read_exact(
        &mut cursor,
        &mut aot_bytes,
        "compilationcache: could not read aot metadata",
    )?;
    let aot = deserialize_aot_metadata(&aot_bytes).map_err(|err| match err {
        AotMetadataError::InvalidHeader(message) | AotMetadataError::Io(message) => {
            EngineCacheError::AotMetadata(message)
        }
    })?;

    Ok(Some(CachedCompiledModule {
        executables: Executables {
            executable: AlignedBytes::from_bytes(executable),
            ..Executables::default()
        },
        function_offsets,
        source_map,
        aot,
    }))
}

fn read_exact(
    cursor: &mut Cursor<&[u8]>,
    buf: &mut [u8],
    context: &str,
) -> Result<(), EngineCacheError> {
    cursor
        .read_exact(buf)
        .map_err(|err| EngineCacheError::Io(format!("{context}: {err}")))
}

fn read_u8(cursor: &mut Cursor<&[u8]>) -> Result<u8, EngineCacheError> {
    let mut buf = [0u8; 1];
    read_exact(cursor, &mut buf, "compilationcache: invalid header length")?;
    Ok(buf[0])
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> Result<u32, EngineCacheError> {
    let mut buf = [0u8; 4];
    read_exact(cursor, &mut buf, "compilationcache: invalid header length")?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u32_named(cursor: &mut Cursor<&[u8]>, context: &str) -> Result<u32, EngineCacheError> {
    let mut buf = [0u8; 4];
    read_exact(cursor, &mut buf, context)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64_named(cursor: &mut Cursor<&[u8]>, context: &str) -> Result<u64, EngineCacheError> {
    let mut buf = [0u8; 8];
    read_exact(cursor, &mut buf, context)?;
    Ok(u64::from_le_bytes(buf))
}

fn checked_vec_len(
    cursor: &Cursor<&[u8]>,
    len: u64,
    element_size: usize,
    context: &str,
) -> Result<usize, EngineCacheError> {
    let len =
        usize::try_from(len).map_err(|_| EngineCacheError::InvalidHeader(context.to_string()))?;
    let bytes_needed = len
        .checked_mul(element_size)
        .ok_or_else(|| EngineCacheError::InvalidHeader(context.to_string()))?;
    if bytes_needed > remaining(cursor) {
        return Err(EngineCacheError::InvalidHeader(context.to_string()));
    }
    Ok(len)
}

fn remaining(cursor: &Cursor<&[u8]>) -> usize {
    cursor
        .get_ref()
        .len()
        .saturating_sub(cursor.position() as usize)
}

fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82f63b78 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::{crc32c, deserialize_compiled_module, file_cache_key, serialize_compiled_module};
    use crate::aot::{
        AotCompiledMetadata, AotFunctionMetadata, AotGlobalInitializerMetadata,
        AotGlobalTypeMetadata, AotImportDescMetadata, AotImportMetadata, AotMemoryMetadata,
        AotModuleContextMetadata, AotModuleShapeMetadata, AotSourceMapEntry, AotTableMetadata,
    };
    use crate::engine::SourceMap;
    use razero_wasm::module::{ExternType, Module, RefType, ValueType};

    #[test]
    fn cache_key_rehashes_module_id() {
        let module = Module {
            id: [7; 32],
            ..Module::default()
        };
        assert_ne!(file_cache_key(&module, false, false), module.id);
    }

    #[test]
    fn cache_key_distinguishes_memory_isolation_mode() {
        let module = Module {
            id: [7; 32],
            ..Module::default()
        };
        assert_ne!(
            file_cache_key(&module, false, false),
            file_cache_key(&module, true, false)
        );
    }

    #[test]
    fn cache_key_distinguishes_fuel_mode() {
        let module = Module {
            id: [7; 32],
            ..Module::default()
        };
        assert_ne!(
            file_cache_key(&module, false, false),
            file_cache_key(&module, false, true)
        );
    }

    #[test]
    fn serialize_and_deserialize_round_trip() {
        let source_map = SourceMap {
            executable_offsets: vec![4, 8],
            wasm_binary_offsets: vec![10, 20],
        };
        let aot = AotCompiledMetadata {
            module_id: [9; 32],
            import_function_count: 0,
            entry_preamble_offsets: vec![0, 8],
            imports: vec![AotImportMetadata {
                ty: ExternType::MEMORY,
                module: "env".to_string(),
                name: "memory".to_string(),
                desc: AotImportDescMetadata::Memory(AotMemoryMetadata {
                    min: 1,
                    cap: 2,
                    max: 3,
                    is_max_encoded: true,
                    is_shared: false,
                }),
                index_per_type: 0,
            }],
            memory: Some(AotMemoryMetadata {
                min: 2,
                cap: 2,
                max: 6,
                is_max_encoded: true,
                is_shared: false,
            }),
            tables: vec![AotTableMetadata {
                min: 4,
                max: Some(5),
                ty: RefType::FUNCREF,
            }],
            global_initializers: vec![AotGlobalInitializerMetadata {
                init_expression: vec![0x42, 0x2a, 0x0b],
            }],
            globals: vec![AotGlobalTypeMetadata {
                val_type: ValueType::I64,
                mutable: true,
            }],
            functions: vec![AotFunctionMetadata {
                local_function_index: 0,
                wasm_function_index: 0,
                type_index: 0,
                executable_offset: 0,
                executable_len: 4,
            }],
            relocations: Vec::new(),
            module_context: AotModuleContextMetadata {
                total_size: 16,
                module_instance_offset: 0,
                local_memory_begin: 8,
                imported_memory_begin: -1,
                imported_functions_begin: -1,
                globals_begin: -1,
                type_ids_1st_element: -1,
                tables_begin: -1,
                before_listener_trampolines_1st_element: -1,
                after_listener_trampolines_1st_element: -1,
                data_instances_1st_element: 8,
                element_instances_1st_element: 16,
            },
            source_map: vec![AotSourceMapEntry {
                wasm_binary_offset: 10,
                executable_offset: 4,
            }],
            module_shape: AotModuleShapeMetadata {
                import_function_count: 0,
                import_memory_count: 1,
                local_function_count: 1,
                local_global_count: 1,
                local_table_count: 1,
                has_local_memory: true,
                has_any_memory: true,
                ..AotModuleShapeMetadata::default()
            },
            ensure_termination: false,
            memory_isolation_enabled: false,
            ..AotCompiledMetadata::default()
        };
        let bytes = serialize_compiled_module("0.0.0", &[1, 2, 3, 4], &[0, 2], &source_map, &aot);
        let cached = deserialize_compiled_module("0.0.0", &bytes)
            .unwrap()
            .unwrap();
        assert_eq!(cached.executables.executable.as_slice(), &[1, 2, 3, 4]);
        assert_eq!(cached.function_offsets, vec![0, 2]);
        assert_eq!(cached.source_map, source_map);
        assert_eq!(cached.aot, aot);
    }

    #[test]
    fn deserialize_rejects_checksum_mismatch() {
        let version = "0.0.0";
        let executable = [1, 2, 3];
        let mut bytes = serialize_compiled_module(
            version,
            &executable,
            &[0],
            &SourceMap::default(),
            &AotCompiledMetadata::default(),
        );
        let checksum_offset = super::MAGIC.len() + 1 + version.len() + 4 + 8 + 8 + executable.len();
        bytes[checksum_offset] ^= 0xff;
        assert!(deserialize_compiled_module(version, &bytes).is_err());
    }

    #[test]
    fn version_mismatch_returns_stale_cache() {
        let bytes = serialize_compiled_module(
            "0.0.0",
            &[1, 2, 3],
            &[0],
            &SourceMap::default(),
            &AotCompiledMetadata::default(),
        );
        assert!(deserialize_compiled_module("1.0.0", &bytes)
            .unwrap()
            .is_none());
    }

    #[test]
    fn crc32c_matches_known_vector() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }
}
