#![doc = "Runtime engine traits."]

use std::any::Any;
use std::error::Error;
use std::fmt::{Display, Formatter};

use crate::module::{Index, Module};
use crate::module_instance::ModuleInstance;
use crate::table::{Reference, TableInstance};

pub type FunctionTypeId = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError {
    message: String,
}

impl EngineError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for EngineError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for EngineError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompileOptions {
    workers: usize,
}

impl CompileOptions {
    pub fn new(workers: usize) -> Self {
        Self {
            workers: workers.max(1),
        }
    }

    pub fn workers(self) -> usize {
        self.workers
    }
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self::new(1)
    }
}

pub trait FunctionHandle: Send + Sync {
    fn index(&self) -> Index;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NullFunctionHandle {
    index: Index,
}

impl NullFunctionHandle {
    pub fn new(index: Index) -> Self {
        Self { index }
    }
}

impl FunctionHandle for NullFunctionHandle {
    fn index(&self) -> Index {
        self.index
    }
}

pub trait ModuleEngine: Send + Sync {
    fn as_any(&self) -> &dyn Any;

    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn done_instantiation(&mut self) {}

    fn new_function(&self, index: Index) -> Box<dyn FunctionHandle>;

    fn resolve_imported_function(
        &mut self,
        _index: Index,
        _desc_func: Index,
        _index_in_imported_module: Index,
        _imported_module_engine: &dyn ModuleEngine,
    ) {
    }

    fn resolve_imported_memory(&mut self, _imported_module_engine: &dyn ModuleEngine) {}

    fn memory_snapshot(&self) -> Option<(Vec<u8>, Option<u32>, bool)> {
        None
    }

    fn overwrite_memory(
        &mut self,
        _bytes: &[u8],
        _maximum_pages: Option<u32>,
        _shared: bool,
    ) -> bool {
        false
    }

    fn lookup_function(
        &self,
        _table: &TableInstance,
        _type_id: FunctionTypeId,
        _table_offset: Index,
    ) -> Option<(&ModuleInstance, Index)> {
        None
    }

    fn get_global_value(&self, _index: Index) -> (u64, u64) {
        (0, 0)
    }

    fn set_global_value(&mut self, _index: Index, _lo: u64, _hi: u64) {}

    fn owns_globals(&self) -> bool {
        false
    }

    fn function_instance_reference(&self, func_index: Index) -> Reference {
        Some(u64::from(func_index))
    }

    fn memory_grown(&mut self) {}
}

pub trait Engine: Send + Sync {
    fn close(&mut self) -> Result<(), EngineError> {
        Ok(())
    }

    fn compile_module_with_options(
        &mut self,
        module: &Module,
        _options: &CompileOptions,
    ) -> Result<(), EngineError> {
        self.compile_module(module)
    }

    fn compile_module(&mut self, _module: &Module) -> Result<(), EngineError> {
        Ok(())
    }

    fn compiled_module_count(&self) -> u32 {
        0
    }

    fn delete_compiled_module(&mut self, _module: &Module) {}

    fn load_precompiled_module(
        &mut self,
        _module: &Module,
        _artifact: &[u8],
    ) -> Result<(), EngineError> {
        Err(EngineError::new(
            "precompiled artifacts are unsupported by this engine",
        ))
    }

    fn precompiled_module_bytes(&self, _module: &Module) -> Option<Vec<u8>> {
        None
    }

    fn new_module_engine(
        &self,
        _module: &Module,
        _instance: &ModuleInstance,
    ) -> Result<Box<dyn ModuleEngine>, EngineError>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NullEngine;

impl ModuleEngine for NullEngine {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn new_function(&self, index: Index) -> Box<dyn FunctionHandle> {
        Box::new(NullFunctionHandle::new(index))
    }
}

impl Engine for NullEngine {
    fn new_module_engine(
        &self,
        _module: &Module,
        _instance: &ModuleInstance,
    ) -> Result<Box<dyn ModuleEngine>, EngineError> {
        Ok(Box::new(*self))
    }
}

#[cfg(test)]
mod tests {
    use super::{Engine, NullEngine};
    use crate::module::Module;
    use crate::module_instance::ModuleInstance;

    #[test]
    fn null_engine_creates_function_handles() {
        let engine = NullEngine;
        let module_engine = engine
            .new_module_engine(&Module::default(), &ModuleInstance::default())
            .unwrap();

        assert_eq!(7, module_engine.new_function(7).index());
        assert_eq!(Some(9), module_engine.function_instance_reference(9));
    }
}
