use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::cache::CompilationCache;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FileCompilationCache {
    root: PathBuf,
}

impl FileCompilationCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let _ = fs::create_dir_all(&root);
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn cache_path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }
}

impl CompilationCache for FileCompilationCache {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        fs::read(self.cache_path(key)).ok()
    }

    fn insert(&self, key: &str, bytes: &[u8]) {
        let _ = fs::create_dir_all(&self.root);
        let _ = fs::write(self.cache_path(key), bytes);
    }
}
