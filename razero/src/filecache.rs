#![doc = "Filesystem-backed compilation cache.\n\nThis module performs direct filesystem I/O and is only included when the\n`filecache` feature is enabled. Embedders who do not need disk caching\ncan use [`crate::InMemoryCompilationCache`] instead."]

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

use crate::cache::{BinaryCompilationArtifact, CompilationCache};

pub type Key = [u8; 32];

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default)]
pub struct FileCompilationCache {
    root: PathBuf,
    binary_artifacts: Arc<Mutex<HashMap<String, BinaryCompilationArtifact>>>,
}

impl FileCompilationCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            binary_artifacts: Arc::default(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, key: Key) -> PathBuf {
        self.root.join(hex_key(&key))
    }

    pub fn get_entry(&self, key: Key) -> io::Result<Option<File>> {
        match File::open(self.path(key)) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn add_entry<R: Read>(&self, key: Key, mut content: R) -> io::Result<()> {
        self.write_atomically(&self.path(key), &mut content)
    }

    pub fn delete_entry(&self, key: Key) -> io::Result<()> {
        match fs::remove_file(self.path(key)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn get_bytes(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.cache_path(key)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn insert_bytes(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        self.write_atomically(&self.cache_path(key), &mut io::Cursor::new(bytes))
    }

    pub fn delete(&self, key: &str) -> io::Result<()> {
        match fs::remove_file(self.cache_path(key)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn cache_path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    fn write_atomically<R: Read>(&self, destination: &Path, content: &mut R) -> io::Result<()> {
        let directory = destination.parent().unwrap_or(self.root.as_path());
        fs::create_dir_all(directory)?;

        let temp_path = self.temp_path(destination);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;

        let write_result = (|| -> io::Result<()> {
            io::copy(content, &mut file)?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp_path, destination)?;
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }

        write_result
    }

    fn temp_path(&self, destination: &Path) -> PathBuf {
        let file_name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cache");
        let unique = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        destination.with_file_name(format!("{file_name}.{}.{}.tmp", std::process::id(), unique))
    }
}

impl CompilationCache for FileCompilationCache {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.get_bytes(key).ok().flatten()
    }

    fn insert(&self, key: &str, bytes: &[u8]) {
        let _ = self.insert_bytes(key, bytes);
    }

    fn get_binary_artifact(&self, key: &str) -> Option<BinaryCompilationArtifact> {
        self.binary_artifacts.lock().ok()?.get(key).cloned()
    }

    fn insert_binary_artifact(&self, key: &str, artifact: BinaryCompilationArtifact) {
        if let Ok(mut modules) = self.binary_artifacts.lock() {
            modules.insert(key.to_string(), artifact);
        }
    }
}

fn hex_key(key: &Key) -> String {
    let mut encoded = String::with_capacity(key.len() * 2);
    for byte in key {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{:02x}", byte);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{FileCompilationCache, Key};

    static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn add_entry_creates_or_replaces_file() {
        let scratch = ScratchDir::new("filecache-add");
        let cache = FileCompilationCache::new(scratch.path());
        let content = [1, 2, 3, 4, 5];
        let key = key([1, 2, 3, 4, 5, 6, 7]);

        cache
            .add_entry(key, std::io::Cursor::new(content))
            .expect("add should succeed");
        assert_eq!(
            content.as_slice(),
            std::fs::read(cache.path(key))
                .expect("read should succeed")
                .as_slice()
        );

        cache
            .add_entry(key, std::io::Cursor::new(content))
            .expect("replace should succeed");
        assert_eq!(
            content.as_slice(),
            std::fs::read(cache.path(key))
                .expect("read should succeed")
                .as_slice()
        );
    }

    #[test]
    fn delete_entry_ignores_missing_files() {
        let scratch = ScratchDir::new("filecache-delete");
        let cache = FileCompilationCache::new(scratch.path());
        let key = key([1, 2, 3]);

        cache.delete_entry(key).expect("delete should succeed");
        cache
            .add_entry(key, std::io::Cursor::new([9]))
            .expect("add should succeed");
        cache.delete_entry(key).expect("delete should succeed");
        assert!(!cache.path(key).exists());
    }

    #[test]
    fn get_entry_matches_go_contract() {
        let scratch = ScratchDir::new("filecache-get");
        let cache = FileCompilationCache::new(scratch.path());
        let key = key([1, 2, 3]);

        assert!(cache.get_entry(key).expect("get should succeed").is_none());

        cache
            .add_entry(key, std::io::Cursor::new([1, 2, 3, 4, 5]))
            .expect("add should succeed");
        let mut file = cache
            .get_entry(key)
            .expect("get should succeed")
            .expect("entry should exist");
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).expect("read should succeed");
        assert_eq!(vec![1, 2, 3, 4, 5], actual);
    }

    #[test]
    fn path_matches_go_hex_encoding() {
        let cache = FileCompilationCache::new("target/test-scratch/path");
        assert_eq!(
            PathBuf::from(
                "target/test-scratch/path/0102030405000000000000000000000000000000000000000000000000000000",
            ),
            cache.path(key([1, 2, 3, 4, 5]))
        );
    }

    fn key(prefix: impl AsRef<[u8]>) -> Key {
        let prefix = prefix.as_ref();
        let mut key = [0_u8; 32];
        key[..prefix.len()].copy_from_slice(prefix);
        key
    }

    struct ScratchDir {
        path: PathBuf,
    }

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let mut path = std::env::current_dir().expect("cwd should exist");
            path.push("target");
            path.push("test-scratch");
            path.push(format!(
                "{name}-{}-{}",
                std::process::id(),
                SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("scratch dir should be created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
