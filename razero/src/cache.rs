use std::{collections::HashMap, sync::Mutex};

pub trait CompilationCache: Send + Sync {
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    fn insert(&self, key: &str, bytes: &[u8]);
}

#[derive(Debug, Default)]
pub struct InMemoryCompilationCache {
    modules: Mutex<HashMap<String, Vec<u8>>>,
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
}
