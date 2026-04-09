use std::{collections::HashMap, sync::Mutex};

use razero_wasm::module::Module as WasmModule;

#[derive(Clone, Debug)]
pub struct BinaryCompilationArtifact {
    pub(crate) bytes: Vec<u8>,
    pub(crate) module: WasmModule,
}

impl BinaryCompilationArtifact {
    pub(crate) fn new(bytes: Vec<u8>, module: WasmModule) -> Self {
        Self { bytes, module }
    }
}

pub trait CompilationCache: Send + Sync {
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    fn insert(&self, key: &str, bytes: &[u8]);

    fn get_binary_artifact(&self, key: &str) -> Option<BinaryCompilationArtifact> {
        let _ = key;
        None
    }

    fn insert_binary_artifact(&self, key: &str, artifact: BinaryCompilationArtifact) {
        let _ = (key, artifact);
    }
}

#[derive(Debug, Default)]
pub struct InMemoryCompilationCache {
    modules: Mutex<HashMap<String, Vec<u8>>>,
    binary_artifacts: Mutex<HashMap<String, BinaryCompilationArtifact>>,
}

impl InMemoryCompilationCache {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CompilationCache for InMemoryCompilationCache {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.modules.lock().ok()?.get(key).cloned()
    }

    fn insert(&self, key: &str, bytes: &[u8]) {
        if let Ok(mut modules) = self.modules.lock() {
            modules.insert(key.to_string(), bytes.to_vec());
        }
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
