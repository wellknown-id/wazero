#![doc = "Runtime store bookkeeping and module instantiation."]

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "secure-memory")]
use razero_secmem::{GuardPageError, SecMemError};

use crate::engine::{
    CompileOptions, Engine as WasmEngine, EngineError, ModuleEngine as WasmModuleEngine, NullEngine,
};
use crate::module::{ExternType, FunctionType, ImportDesc, Module};
use crate::module_instance::{
    FunctionTypeId, ModuleInstance, ModuleInstantiationError, MAXIMUM_FUNCTION_TYPES,
};
use crate::module_instance_lookup::LookupError;
use crate::store_module_list::{ModuleInstanceId, StoreModuleList};

pub const NAME_TO_MODULE_SHRINK_THRESHOLD: usize = 100;

static NEXT_MODULE_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

pub struct Store<E = NullEngine> {
    pub module_list: StoreModuleList,
    pub module_engines: BTreeMap<ModuleInstanceId, Box<dyn WasmModuleEngine>>,
    pub name_to_module: BTreeMap<String, ModuleInstanceId>,
    pub name_to_module_cap: usize,
    pub engine: E,
    pub type_ids: BTreeMap<String, FunctionTypeId>,
    pub function_max_types: u32,
    pub modules: BTreeMap<ModuleInstanceId, ModuleInstance>,
    pub secure_memory: bool,
    closed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreError {
    AlreadyClosed,
    DuplicateModule(String),
    ModuleNotInstantiated(String),
    TypeIdCountMismatch { expected: usize, actual: usize },
    TooManyFunctionTypes,
    InvalidImport(String),
    InvalidFunctionType(u32),
    InvalidExport(String),
    Instantiation(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyClosed => f.write_str("already closed"),
            Self::DuplicateModule(name) => {
                write!(f, "module[{name}] has already been instantiated")
            }
            Self::ModuleNotInstantiated(name) => write!(f, "module[{name}] not instantiated"),
            Self::TypeIdCountMismatch { expected, actual } => {
                write!(
                    f,
                    "type ID count mismatch: expected {expected}, got {actual}"
                )
            }
            Self::TooManyFunctionTypes => f.write_str("too many function types in a store"),
            Self::InvalidImport(message)
            | Self::InvalidExport(message)
            | Self::Instantiation(message) => f.write_str(message),
            Self::InvalidFunctionType(index) => write!(f, "function type[{index}] out of range"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<LookupError> for StoreError {
    fn from(value: LookupError) -> Self {
        Self::InvalidExport(value.to_string())
    }
}

impl From<ModuleInstantiationError> for StoreError {
    fn from(value: ModuleInstantiationError) -> Self {
        Self::Instantiation(value.to_string())
    }
}

impl From<EngineError> for StoreError {
    fn from(value: EngineError) -> Self {
        Self::Instantiation(value.to_string())
    }
}

impl<E: Default> Default for Store<E> {
    fn default() -> Self {
        Self::new(E::default())
    }
}

impl<E> Store<E> {
    pub fn new(engine: E) -> Self {
        Self {
            module_list: StoreModuleList::default(),
            module_engines: BTreeMap::new(),
            name_to_module: BTreeMap::new(),
            name_to_module_cap: NAME_TO_MODULE_SHRINK_THRESHOLD,
            engine,
            type_ids: BTreeMap::new(),
            function_max_types: MAXIMUM_FUNCTION_TYPES,
            modules: BTreeMap::new(),
            secure_memory: false,
            closed: false,
        }
    }

    pub fn set_secure_memory(&mut self, enabled: bool) {
        self.secure_memory = enabled;
    }

    pub fn module(&self, module_name: &str) -> Result<&ModuleInstance, StoreError> {
        let id = self
            .name_to_module
            .get(module_name)
            .ok_or_else(|| StoreError::ModuleNotInstantiated(module_name.to_string()))?;
        self.modules
            .get(id)
            .ok_or_else(|| StoreError::ModuleNotInstantiated(module_name.to_string()))
    }

    pub fn module_mut(&mut self, module_name: &str) -> Result<&mut ModuleInstance, StoreError> {
        let id = *self
            .name_to_module
            .get(module_name)
            .ok_or_else(|| StoreError::ModuleNotInstantiated(module_name.to_string()))?;
        self.modules
            .get_mut(&id)
            .ok_or_else(|| StoreError::ModuleNotInstantiated(module_name.to_string()))
    }

    pub fn instance(&self, id: ModuleInstanceId) -> Option<&ModuleInstance> {
        self.modules.get(&id)
    }

    pub fn instance_mut(&mut self, id: ModuleInstanceId) -> Option<&mut ModuleInstance> {
        self.modules.get_mut(&id)
    }

    pub fn module_engine(&self, id: ModuleInstanceId) -> Option<&dyn WasmModuleEngine> {
        self.module_engines.get(&id).map(Box::as_ref)
    }

    pub fn module_engine_mut(
        &mut self,
        id: ModuleInstanceId,
    ) -> Option<&mut (dyn WasmModuleEngine + '_)> {
        self.module_engines
            .get_mut(&id)
            .map(|engine| engine.as_mut() as &mut (dyn WasmModuleEngine + '_))
    }

    pub fn get_function_type_ids(
        &mut self,
        types: &mut [FunctionType],
    ) -> Result<Vec<FunctionTypeId>, StoreError> {
        let mut ids = Vec::with_capacity(types.len());
        for function_type in types {
            ids.push(self.get_function_type_id(function_type)?);
        }
        Ok(ids)
    }

    pub fn get_function_type_id(
        &mut self,
        function_type: &mut FunctionType,
    ) -> Result<FunctionTypeId, StoreError> {
        let key = function_type.key().to_string();
        if let Some(existing) = self.type_ids.get(&key) {
            return Ok(*existing);
        }

        let next = self.type_ids.len() as u32;
        if next >= self.function_max_types {
            return Err(StoreError::TooManyFunctionTypes);
        }

        self.type_ids.insert(key, next);
        Ok(next)
    }

    pub fn register_module(
        &mut self,
        module: ModuleInstance,
    ) -> Result<ModuleInstanceId, StoreError> {
        if self.closed {
            return Err(StoreError::AlreadyClosed);
        }

        if !module.module_name.is_empty() {
            if self.name_to_module.contains_key(&module.module_name) {
                return Err(StoreError::DuplicateModule(module.module_name));
            }
            self.name_to_module
                .insert(module.module_name.clone(), module.id);
            if self.name_to_module.len() > self.name_to_module_cap {
                self.name_to_module_cap = self.name_to_module.len();
            }
        }

        let id = module.id;
        self.module_list.push_front(id);
        self.modules.insert(id, module);
        self.sync_store_links(id);
        Ok(id)
    }

    pub fn delete_module(&mut self, id: ModuleInstanceId) -> Result<(), StoreError> {
        let Some(module) = self.modules.get(&id).cloned() else {
            return Ok(());
        };

        let removed_links = self.module_list.remove(id);
        self.module_engines.remove(&id);
        self.modules.remove(&id);

        if !module.module_name.is_empty() {
            self.name_to_module.remove(&module.module_name);
            let mut new_cap = self.name_to_module.len();
            if new_cap < NAME_TO_MODULE_SHRINK_THRESHOLD {
                new_cap = NAME_TO_MODULE_SHRINK_THRESHOLD;
            }
            if new_cap * 2 <= self.name_to_module_cap {
                self.name_to_module_cap = new_cap;
            }
        }

        if let Some(links) = removed_links {
            if let Some(prev) = links.prev {
                self.sync_store_links(prev);
            }
            if let Some(next) = links.next {
                self.sync_store_links(next);
            }
        }

        Ok(())
    }

    pub fn close_module_with_exit_code(
        &mut self,
        id: ModuleInstanceId,
        exit_code: u32,
    ) -> Result<(), StoreError> {
        if let Some(module) = self.modules.get_mut(&id) {
            module.close_with_exit_code(exit_code);
        }
        self.delete_module(id)
    }

    pub fn close_with_exit_code(&mut self, exit_code: u32) -> Result<(), StoreError> {
        let module_ids = self.module_list.iter().collect::<Vec<_>>();
        for id in module_ids {
            if let Some(module) = self.modules.get_mut(&id) {
                module.close_with_exit_code(exit_code);
            }
        }

        self.module_list = StoreModuleList::default();
        self.name_to_module.clear();
        self.name_to_module_cap = 0;
        self.type_ids.clear();
        self.module_engines.clear();
        self.modules.clear();
        self.closed = true;
        Ok(())
    }

    fn instantiate_module(&self, instance: &mut ModuleInstance) -> Result<(), StoreError> {
        instance.rebuild_exports();

        let source = instance.source.clone();

        for import in &source.import_section {
            let imported_module = self.module(&import.module)?;
            let imported = imported_module.get_export(&import.name, import.ty)?;

            match &import.desc {
                ImportDesc::Func(type_index) => {
                    let expected = source
                        .type_section
                        .get(*type_index as usize)
                        .ok_or(StoreError::InvalidFunctionType(*type_index))?;
                    let actual = imported_module
                        .source
                        .type_of_function(imported.index)
                        .ok_or(StoreError::InvalidFunctionType(imported.index))?;
                    if !actual.equals_signature(&expected.params, &expected.results) {
                        return Err(StoreError::InvalidImport(format!(
                            "import {}[{}.{}]: signature mismatch: {} != {}",
                            ExternType::FUNC.name(),
                            import.module,
                            import.name,
                            expected,
                            actual
                        )));
                    }
                    let imported_function = imported_module
                        .functions
                        .get(imported.index as usize)
                        .ok_or(StoreError::InvalidFunctionType(imported.index))?;
                    instance.functions.push(imported_function.clone());
                }
                ImportDesc::Table(expected) => {
                    let actual_type = imported_module
                        .table_types
                        .get(imported.index as usize)
                        .ok_or_else(|| {
                            StoreError::InvalidImport(format!(
                                "import {}[{}.{}]: table[{}] out of bounds",
                                ExternType::TABLE.name(),
                                import.module,
                                import.name,
                                imported.index
                            ))
                        })?;
                    if expected.ty != actual_type.ty {
                        return Err(StoreError::InvalidImport(format!(
                            "import {}[{}.{}]: table type mismatch: {} != {}",
                            ExternType::TABLE.name(),
                            import.module,
                            import.name,
                            expected.ty.name(),
                            actual_type.ty.name()
                        )));
                    }
                    let actual_len = imported_module
                        .tables
                        .get(imported.index as usize)
                        .map(|table| table.len())
                        .unwrap_or_default() as u32;
                    if expected.min > actual_len {
                        return Err(StoreError::InvalidImport(format!(
                            "import {}[{}.{}]: minimum size mismatch: {} > {}",
                            ExternType::TABLE.name(),
                            import.module,
                            import.name,
                            expected.min,
                            actual_len
                        )));
                    }
                    if let Some(expected_max) = expected.max {
                        match actual_type.max {
                            Some(actual_max) if expected_max < actual_max => {
                                return Err(StoreError::InvalidImport(format!(
                                    "import {}[{}.{}]: maximum size mismatch: {} < {}",
                                    ExternType::TABLE.name(),
                                    import.module,
                                    import.name,
                                    expected_max,
                                    actual_max
                                )));
                            }
                            None => {
                                return Err(StoreError::InvalidImport(format!(
                                    "import {}[{}.{}]: maximum size mismatch: {}, but actual has no max",
                                    ExternType::TABLE.name(),
                                    import.module,
                                    import.name,
                                    expected_max
                                )));
                            }
                            _ => {}
                        }
                    }
                    instance
                        .tables
                        .push(imported_module.tables[imported.index as usize].clone());
                    instance.table_types.push(actual_type.clone());
                }
                ImportDesc::Memory(expected) => {
                    let actual_type = imported_module.memory_type.clone().ok_or_else(|| {
                        StoreError::InvalidImport(format!(
                            "import {}[{}.{}]: memory not instantiated",
                            ExternType::MEMORY.name(),
                            import.module,
                            import.name
                        ))
                    })?;
                    let snapshot = self
                        .module_engine(imported_module.id)
                        .and_then(|engine| engine.memory_snapshot());
                    let actual_pages = snapshot
                        .as_ref()
                        .map(|(bytes, _, _)| (bytes.len() / 65_536) as u32)
                        .or_else(|| {
                            imported_module
                                .memory_instance
                                .as_ref()
                                .map(|memory| (memory.bytes.len() / 65_536) as u32)
                        })
                        .unwrap_or_default();
                    if expected.min > actual_pages {
                        return Err(StoreError::InvalidImport(format!(
                            "import {}[{}.{}]: minimum size mismatch: {} > {}",
                            ExternType::MEMORY.name(),
                            import.module,
                            import.name,
                            expected.min,
                            actual_pages
                        )));
                    }
                    if expected.max < actual_type.max {
                        return Err(StoreError::InvalidImport(format!(
                            "import {}[{}.{}]: maximum size mismatch: {} < {}",
                            ExternType::MEMORY.name(),
                            import.module,
                            import.name,
                            expected.max,
                            actual_type.max
                        )));
                    }
                    let mut imported_memory = imported_module.memory_instance.clone();
                    if let (Some(memory), Some((bytes, _, _))) =
                        (imported_memory.as_mut(), snapshot)
                    {
                        let imported_pages = (bytes.len() / 65_536) as u32;
                        let current_pages = memory.pages();
                        if imported_pages > current_pages {
                            let _ = memory.grow(imported_pages - current_pages);
                        }
                        let _ = memory.write(0, &bytes);
                    }
                    instance.memory_instance = imported_memory;
                    instance.memory_type = Some(actual_type);
                    instance.imported_memory_module_id = Some(imported_module.id);
                }
                ImportDesc::Global(expected) => {
                    let actual_type = imported_module
                        .global_types
                        .get(imported.index as usize)
                        .copied()
                        .ok_or_else(|| {
                            StoreError::InvalidImport(format!(
                                "import {}[{}.{}]: global[{}] out of bounds",
                                ExternType::GLOBAL.name(),
                                import.module,
                                import.name,
                                imported.index
                            ))
                        })?;
                    if expected.mutable != actual_type.mutable {
                        return Err(StoreError::InvalidImport(format!(
                            "import {}[{}.{}]: mutability mismatch: {} != {}",
                            ExternType::GLOBAL.name(),
                            import.module,
                            import.name,
                            expected.mutable,
                            actual_type.mutable
                        )));
                    }
                    if expected.val_type != actual_type.val_type {
                        return Err(StoreError::InvalidImport(format!(
                            "import {}[{}.{}]: value type mismatch: {} != {}",
                            ExternType::GLOBAL.name(),
                            import.module,
                            import.name,
                            expected.val_type.name(),
                            actual_type.val_type.name()
                        )));
                    }
                    instance
                        .globals
                        .push(imported_module.globals[imported.index as usize].clone());
                    instance.global_types.push(actual_type);
                }
            }
        }

        for (index, type_index) in source.function_section.iter().copied().enumerate() {
            let type_id = *instance
                .type_ids
                .get(type_index as usize)
                .ok_or(StoreError::InvalidFunctionType(type_index))?;
            instance.add_defined_function(type_id, source.import_function_count + index as u32);
        }

        for table in &source.table_section {
            instance.add_defined_table(table);
        }

        if instance.memory_instance.is_none() {
            if let Some(memory) = &source.memory_section {
                #[cfg(feature = "secure-memory")]
                if self.secure_memory {
                    match instance.define_memory_guarded(memory) {
                        Ok(()) => {}
                        Err(SecMemError::Platform(GuardPageError::Unsupported(_))) => {
                            instance.define_memory(memory);
                        }
                        Err(_) => {
                            return Err(StoreError::Instantiation(
                                "memory allocation failed".to_string(),
                            ));
                        }
                    }
                } else {
                    instance.define_memory(memory);
                }
                #[cfg(not(feature = "secure-memory"))]
                instance.define_memory(memory);
            }
        }

        for global in &source.global_section {
            let value = instance.evaluate_global_initializer(&global.init)?;
            instance.add_defined_global(global.ty, value);
        }

        instance.build_element_instances(&source.element_section)?;
        instance.validate_elements(&source.element_section)?;
        if !source
            .enabled_features
            .contains(razero_features::CoreFeatures::REFERENCE_TYPES)
        {
            instance.validate_data(&source.data_section)?;
        }
        if !source.data_section.is_empty() {
            instance.apply_data(&source.data_section)?;
        }
        instance.apply_elements(&source.element_section)?;
        Ok(())
    }

    fn sync_store_links(&mut self, id: ModuleInstanceId) {
        let Some(links) = self.module_list.links(id) else {
            return;
        };
        if let Some(module) = self.modules.get_mut(&id) {
            module.set_store_links(links);
        }
    }

    fn persist_imported_memory(&mut self, instance: &ModuleInstance) {
        let (Some(imported_id), Some(memory)) = (
            instance.imported_memory_module_id,
            instance.memory_instance.as_ref(),
        ) else {
            return;
        };
        if let Some(imported_module) = self.modules.get_mut(&imported_id) {
            imported_module.memory_instance = Some(memory.clone());
        }
        if let Some(engine) = self.module_engines.get_mut(&imported_id) {
            let _ = engine.overwrite_memory(
                memory.bytes.as_ref(),
                instance
                    .memory_type
                    .as_ref()
                    .and_then(|memory_type| memory_type.is_max_encoded.then_some(memory_type.max)),
                memory.shared,
            );
        }
    }
}

impl<E: WasmEngine> Store<E> {
    pub fn instantiate(
        &mut self,
        module: Module,
        name: impl Into<String>,
        type_ids: Option<Vec<FunctionTypeId>>,
    ) -> Result<ModuleInstanceId, StoreError> {
        self.instantiate_with_options(module, name, type_ids, CompileOptions::default())
    }

    pub fn instantiate_with_options(
        &mut self,
        mut module: Module,
        name: impl Into<String>,
        type_ids: Option<Vec<FunctionTypeId>>,
        options: CompileOptions,
    ) -> Result<ModuleInstanceId, StoreError> {
        if self.closed {
            return Err(StoreError::AlreadyClosed);
        }

        self.engine.compile_module_with_options(&module, &options)?;

        let type_ids = match type_ids {
            Some(type_ids) => {
                if type_ids.len() != module.type_section.len() {
                    return Err(StoreError::TypeIdCountMismatch {
                        expected: module.type_section.len(),
                        actual: type_ids.len(),
                    });
                }
                type_ids
            }
            None => self.get_function_type_ids(&mut module.type_section)?,
        };

        let id = NEXT_MODULE_INSTANCE_ID.fetch_add(1, Ordering::Relaxed);

        let mut instance = ModuleInstance::new(id, name, module, type_ids);
        if let Err(err) = self.instantiate_module(&mut instance) {
            self.persist_imported_memory(&instance);
            return Err(err);
        }
        let module_engine = self.instantiate_module_engine(&instance)?;
        let id = self.register_module(instance)?;
        self.module_engines.insert(id, module_engine);
        Ok(id)
    }

    fn instantiate_module_engine(
        &self,
        instance: &ModuleInstance,
    ) -> Result<Box<dyn WasmModuleEngine>, StoreError> {
        let mut module_engine = self.engine.new_module_engine(&instance.source, instance)?;
        for import in &instance.source.import_section {
            let imported_module = self.module(&import.module)?;
            let imported = imported_module.get_export(&import.name, import.ty)?;
            let imported_module_engine =
                self.module_engine(imported_module.id).ok_or_else(|| {
                    StoreError::Instantiation(format!(
                        "module[{}] missing engine state",
                        import.module
                    ))
                })?;
            match &import.desc {
                ImportDesc::Func(type_index) => module_engine.resolve_imported_function(
                    import.index_per_type,
                    *type_index,
                    imported.index,
                    imported_module_engine,
                ),
                ImportDesc::Memory(_) => {
                    module_engine.resolve_imported_memory(imported_module_engine);
                }
                _ => {}
            }
        }
        Ok(module_engine)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::const_expr::ConstExpr;
    use crate::engine::{EngineError, FunctionHandle, ModuleEngine, NullFunctionHandle};
    use crate::memory::MemoryBytes;
    use crate::module::{
        Code, DataSegment, ElementMode, ElementSegment, Export, ExternType, FunctionType, Global,
        GlobalType, Memory, Module, RefType, Table, ValueType,
    };

    #[derive(Clone, Default)]
    struct TestEngine {
        events: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Clone, Default)]
    struct TestModuleEngine {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl ModuleEngine for TestModuleEngine {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn new_function(&self, index: u32) -> Box<dyn FunctionHandle> {
            Box::new(NullFunctionHandle::new(index))
        }

        fn resolve_imported_function(
            &mut self,
            index: u32,
            desc_func: u32,
            index_in_imported_module: u32,
            _imported_module_engine: &dyn ModuleEngine,
        ) {
            self.events.lock().unwrap().push(format!(
                "resolve-func:{index}:{desc_func}:{index_in_imported_module}"
            ));
        }

        fn resolve_imported_memory(&mut self, _imported_module_engine: &dyn ModuleEngine) {
            self.events
                .lock()
                .unwrap()
                .push("resolve-memory".to_string());
        }
    }

    impl crate::engine::Engine for TestEngine {
        fn compile_module(&mut self, module: &Module) -> Result<(), EngineError> {
            self.events.lock().unwrap().push(format!(
                "compile:{}:{}",
                module.import_section.len(),
                module.function_section.len()
            ));
            Ok(())
        }

        fn new_module_engine(
            &self,
            _module: &Module,
            instance: &ModuleInstance,
        ) -> Result<Box<dyn ModuleEngine>, EngineError> {
            self.events
                .lock()
                .unwrap()
                .push(format!("new-engine:{}", instance.id));
            Ok(Box::new(TestModuleEngine {
                events: self.events.clone(),
            }))
        }
    }

    #[test]
    fn register_delete_and_lookup_module_match_go_behavior() {
        let mut store = Store::<NullEngine>::default();
        let m1 = ModuleInstance::new(1, "m1", Module::default(), Vec::new());
        let m2 = ModuleInstance::new(2, "m2", Module::default(), Vec::new());

        store.register_module(m1).unwrap();
        store.register_module(m2).unwrap();

        assert_eq!(Some(2), store.module_list.head());
        assert_eq!(2, store.module("m2").unwrap().id);
        assert_eq!(NAME_TO_MODULE_SHRINK_THRESHOLD, store.name_to_module_cap);

        store.delete_module(2).unwrap();
        assert_eq!(Some(1), store.module_list.head());
        assert_eq!(
            Err(StoreError::ModuleNotInstantiated("m2".to_string())),
            store.module("m2")
        );
    }

    #[test]
    fn function_type_ids_are_interned_and_bounded() {
        let mut store = Store::<NullEngine>::default();
        let mut function_type = FunctionType {
            params: vec![ValueType::I32],
            results: vec![ValueType::I64],
            ..FunctionType::default()
        };

        assert_eq!(0, store.get_function_type_id(&mut function_type).unwrap());
        assert_eq!(0, store.get_function_type_id(&mut function_type).unwrap());

        store.function_max_types = 1;
        let mut another = FunctionType {
            params: vec![ValueType::I64],
            ..FunctionType::default()
        };
        assert_eq!(
            Err(StoreError::TooManyFunctionTypes),
            store.get_function_type_id(&mut another)
        );
    }

    #[test]
    fn instantiate_builds_runtime_state_from_module() {
        let mut store = Store::<NullEngine>::default();
        let module = Module {
            type_section: vec![FunctionType {
                params: vec![ValueType::I32],
                results: vec![ValueType::I64],
                ..FunctionType::default()
            }],
            function_section: vec![0],
            table_section: vec![Table {
                min: 4,
                max: Some(4),
                ty: RefType::FUNCREF,
            }],
            memory_section: Some(Memory {
                min: 1,
                cap: 1,
                max: 1,
                ..Memory::default()
            }),
            global_section: vec![Global {
                ty: GlobalType {
                    val_type: ValueType::I64,
                    mutable: false,
                },
                init: ConstExpr::from_i64(7),
            }],
            export_section: vec![
                Export {
                    ty: ExternType::FUNC,
                    name: "run".to_string(),
                    index: 0,
                },
                Export {
                    ty: ExternType::MEMORY,
                    name: "memory".to_string(),
                    index: 0,
                },
            ],
            data_section: vec![DataSegment {
                offset_expression: ConstExpr::from_i32(1),
                init: vec![0xaa, 0xbb],
                passive: false,
            }],
            element_section: vec![ElementSegment {
                mode: ElementMode::Active,
                table_index: 0,
                offset_expr: ConstExpr::from_i32(0),
                init: vec![ConstExpr::from_i32(0)],
                ty: RefType::FUNCREF,
            }],
            ..Module::default()
        };

        let instance_id = store.instantiate(module, "demo", None).unwrap();
        let instance = store.instance(instance_id).unwrap();

        assert_eq!("demo", instance.name());
        assert_eq!(1, instance.functions.len());
        assert_eq!(Some(0), instance.tables[0].get(0).flatten());
        assert_eq!(Some(instance_id), store.name_to_module.get("demo").copied());
        assert_eq!(7, instance.globals[0].value().0);
        assert_eq!(
            &[0xaa, 0xbb],
            &instance.memory_instance.as_ref().unwrap().bytes[1..3]
        );
    }

    #[cfg(all(target_os = "linux", feature = "secure-memory"))]
    #[test]
    fn instantiate_with_secure_memory_uses_guarded_backing_when_supported() {
        let mut store = Store::<NullEngine>::default();
        store.set_secure_memory(true);
        let module = Module {
            memory_section: Some(Memory {
                min: 1,
                cap: 1,
                max: 1,
                ..Memory::default()
            }),
            ..Module::default()
        };

        let instance_id = store.instantiate(module, "secure", None).unwrap();
        let instance = store.instance(instance_id).unwrap();

        assert!(matches!(
            instance
                .memory_instance
                .as_ref()
                .expect("memory instance")
                .bytes,
            MemoryBytes::Guarded { .. }
        ));
    }

    #[cfg(all(not(target_os = "linux"), feature = "secure-memory"))]
    #[test]
    fn instantiate_with_secure_memory_falls_back_to_plain_backing_when_guard_pages_unsupported() {
        let mut store = Store::<NullEngine>::default();
        store.set_secure_memory(true);
        let module = Module {
            memory_section: Some(Memory {
                min: 1,
                cap: 1,
                max: 1,
                ..Memory::default()
            }),
            ..Module::default()
        };

        let instance_id = store.instantiate(module, "secure", None).unwrap();
        let instance = store.instance(instance_id).unwrap();

        assert!(matches!(
            instance
                .memory_instance
                .as_ref()
                .expect("memory instance")
                .bytes,
            MemoryBytes::Plain(..)
        ));
    }

    #[test]
    fn instantiate_validates_function_import_signatures() {
        let mut store = Store::<NullEngine>::default();
        let imported = Module {
            type_section: vec![FunctionType::default()],
            function_section: vec![0],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            ..Module::default()
        };
        store.instantiate(imported, "env", None).unwrap();

        let importing = Module {
            type_section: vec![FunctionType {
                results: vec![ValueType::F32],
                ..FunctionType::default()
            }],
            import_section: vec![crate::module::Import::function("env", "run", 0)],
            ..Module::default()
        };

        assert_eq!(
            Err(StoreError::InvalidImport(
                "import func[env.run]: signature mismatch: v_f32 != v_v".to_string()
            )),
            store.instantiate(importing, "consumer", None)
        );
    }

    #[test]
    fn instantiate_wires_module_engine_and_resolves_imports() {
        let engine = TestEngine::default();
        let events = engine.events.clone();
        let mut store = Store::new(engine);
        let host = Module {
            type_section: vec![FunctionType::default()],
            function_section: vec![0],
            code_section: vec![Code::default()],
            memory_section: Some(Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                ..Memory::default()
            }),
            export_section: vec![
                Export {
                    ty: ExternType::FUNC,
                    name: "run".to_string(),
                    index: 0,
                },
                Export {
                    ty: ExternType::MEMORY,
                    name: "memory".to_string(),
                    index: 0,
                },
            ],
            ..Module::default()
        };
        let host_id = store.instantiate(host, "env", None).unwrap();

        let consumer = Module {
            type_section: vec![FunctionType::default()],
            import_section: vec![
                crate::module::Import::function("env", "run", 0),
                crate::module::Import::memory(
                    "env",
                    "memory",
                    Memory {
                        min: 1,
                        cap: 1,
                        max: 1,
                        is_max_encoded: true,
                        ..Memory::default()
                    },
                ),
            ],
            import_function_count: 1,
            import_memory_count: 1,
            function_section: vec![0],
            code_section: vec![Code::default()],
            ..Module::default()
        };
        let consumer_id = store.instantiate(consumer, "consumer", None).unwrap();

        assert!(store.module_engine(consumer_id).is_some());
        assert_eq!(
            vec![
                "compile:0:1".to_string(),
                format!("new-engine:{host_id}"),
                "compile:2:1".to_string(),
                format!("new-engine:{consumer_id}"),
                "resolve-func:0:0:0".to_string(),
                "resolve-memory".to_string(),
            ],
            *events.lock().unwrap()
        );

        store.delete_module(consumer_id).unwrap();
        assert!(store.module_engine(consumer_id).is_none());
    }

    #[test]
    fn close_store_clears_bookkeeping() {
        let mut store = Store::<NullEngine>::default();
        store
            .register_module(ModuleInstance::new(1, "m1", Module::default(), Vec::new()))
            .unwrap();
        store.close_with_exit_code(2).unwrap();

        assert!(store.module_list.is_empty());
        assert!(store.modules.is_empty());
        assert!(store.type_ids.is_empty());
        assert_eq!(0, store.name_to_module_cap);
        assert_eq!(
            Err(StoreError::AlreadyClosed),
            store.register_module(ModuleInstance::new(2, "m2", Module::default(), Vec::new()))
        );
    }
}
