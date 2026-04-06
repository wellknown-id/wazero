#![doc = "Compiler engine cache plumbing."]

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex};

use razero_wasm::module::{Module, ModuleId};

use crate::engine::{AlignedBytes, Executables, SourceMap};

const MAGIC: &[u8; 6] = b"WAZEVO";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCacheError {
    InvalidHeader(String),
    Io(String),
    ChecksumMismatch { expected: u32, actual: u32 },
}

impl Display for EngineCacheError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHeader(message) | Self::Io(message) => f.write_str(message),
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

pub fn file_cache_key(module: &Module) -> ModuleId {
    let mut key = module.id;
    let arch = std::env::consts::ARCH.as_bytes();
    for (index, byte) in MAGIC.iter().chain(arch.iter()).copied().enumerate() {
        key[index % key.len()] ^= byte;
    }
    key
}

pub fn serialize_compiled_module(
    version: &str,
    executable: &[u8],
    function_offsets: &[usize],
    source_map: &SourceMap,
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

    let function_count = read_u32(&mut cursor)? as usize;
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
    )? as usize;
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
            )? as usize;
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

    Ok(Some(CachedCompiledModule {
        executables: Executables {
            executable: AlignedBytes::from_bytes(executable),
            ..Executables::default()
        },
        function_offsets,
        source_map,
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
    use crate::engine::SourceMap;
    use razero_wasm::module::Module;

    #[test]
    fn cache_key_rehashes_module_id() {
        let module = Module {
            id: [7; 32],
            ..Module::default()
        };
        assert_ne!(file_cache_key(&module), module.id);
    }

    #[test]
    fn serialize_and_deserialize_round_trip() {
        let source_map = SourceMap {
            executable_offsets: vec![4, 8],
            wasm_binary_offsets: vec![10, 20],
        };
        let bytes = serialize_compiled_module("0.0.0", &[1, 2, 3, 4], &[0, 2], &source_map);
        let cached = deserialize_compiled_module("0.0.0", &bytes)
            .unwrap()
            .unwrap();
        assert_eq!(cached.executables.executable.as_slice(), &[1, 2, 3, 4]);
        assert_eq!(cached.function_offsets, vec![0, 2]);
        assert_eq!(cached.source_map, source_map);
    }

    #[test]
    fn deserialize_rejects_checksum_mismatch() {
        let mut bytes = serialize_compiled_module("0.0.0", &[1, 2, 3], &[0], &SourceMap::default());
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        assert!(deserialize_compiled_module("0.0.0", &bytes).is_err());
    }

    #[test]
    fn version_mismatch_returns_stale_cache() {
        let bytes = serialize_compiled_module("0.0.0", &[1, 2, 3], &[0], &SourceMap::default());
        assert!(deserialize_compiled_module("1.0.0", &bytes)
            .unwrap()
            .is_none());
    }

    #[test]
    fn crc32c_matches_known_vector() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }
}
