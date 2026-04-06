#![doc = "Instantiated Wasm modules and close-state bookkeeping."]

use std::collections::BTreeMap;
use std::fmt;

use crate::const_expr::ConstExpr;
use crate::global::GlobalInstance;
use crate::instruction::{
    OPCODE_END, OPCODE_GLOBAL_GET, OPCODE_I32_CONST, OPCODE_I64_CONST, OPCODE_REF_FUNC,
    OPCODE_REF_NULL,
};
use crate::leb128;
use crate::memory::MemoryInstance;
use crate::module::{
    DataSegment, ElementMode, ElementSegment, Export, GlobalType, Memory, Module, SectionId, Table,
    ValueType,
};
use crate::store_module_list::{ModuleInstanceId, ModuleLinks};
use crate::table::TableInstance;

pub type FunctionTypeId = u32;
pub type DataInstance = Vec<u8>;
pub type ElementInstance = Vec<Option<u32>>;

pub const MAXIMUM_FUNCTION_TYPES: u32 = 1 << 27;

pub const EXIT_CODE_FLAG_MASK: u64 = 0xff;
pub const EXIT_CODE_FLAG_RESOURCE_CLOSED: u64 = 1;
pub const EXIT_CODE_FLAG_RESOURCE_NOT_CLOSED: u64 = 1 << 1;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FunctionInstance {
    pub module_name: String,
    pub function_index: u32,
    pub type_id: FunctionTypeId,
    pub is_host: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModuleInstance {
    pub id: ModuleInstanceId,
    pub module_name: String,
    pub exports: BTreeMap<String, Export>,
    pub globals: Vec<GlobalInstance>,
    pub global_types: Vec<GlobalType>,
    pub memory_instance: Option<MemoryInstance>,
    pub memory_type: Option<Memory>,
    pub tables: Vec<TableInstance>,
    pub table_types: Vec<Table>,
    pub functions: Vec<FunctionInstance>,
    pub type_ids: Vec<FunctionTypeId>,
    pub data_instances: Vec<DataInstance>,
    pub element_instances: Vec<ElementInstance>,
    pub closed: u64,
    pub store_links: ModuleLinks,
    pub source: Module,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleExitError {
    pub exit_code: u32,
}

impl fmt::Display for ModuleExitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "module exited with code {}", self.exit_code)
    }
}

impl std::error::Error for ModuleExitError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleInstantiationError {
    DataOutOfBounds {
        index: usize,
    },
    InvalidConstExpression(String),
    MissingMemory,
    MissingTable(u32),
    TableOutOfBounds {
        table_index: u32,
        offset: usize,
        len: usize,
    },
}

impl fmt::Display for ModuleInstantiationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DataOutOfBounds { index } => {
                write!(
                    f,
                    "{}[{index}]: out of bounds memory access",
                    SectionId::DATA.name()
                )
            }
            Self::InvalidConstExpression(message) => f.write_str(message),
            Self::MissingMemory => f.write_str("memory not instantiated"),
            Self::MissingTable(table_index) => write!(f, "table[{table_index}] not instantiated"),
            Self::TableOutOfBounds {
                table_index,
                offset,
                len,
            } => write!(
                f,
                "table[{table_index}] element range starting at {offset} exceeds table length {len}"
            ),
        }
    }
}

impl std::error::Error for ModuleInstantiationError {}

impl ModuleInstance {
    pub fn new(
        id: ModuleInstanceId,
        module_name: impl Into<String>,
        source: Module,
        type_ids: Vec<FunctionTypeId>,
    ) -> Self {
        Self {
            id,
            module_name: module_name.into(),
            type_ids,
            source,
            ..Self::default()
        }
    }

    pub fn name(&self) -> &str {
        &self.module_name
    }

    pub fn is_closed(&self) -> bool {
        self.closed != 0
    }

    pub fn close(&mut self) -> bool {
        self.close_with_exit_code(0)
    }

    pub fn close_with_exit_code(&mut self, exit_code: u32) -> bool {
        self.set_exit_code(exit_code, EXIT_CODE_FLAG_RESOURCE_CLOSED)
    }

    pub fn close_with_exit_code_without_resources(&mut self, exit_code: u32) -> bool {
        self.set_exit_code(exit_code, EXIT_CODE_FLAG_RESOURCE_NOT_CLOSED)
    }

    pub fn fail_if_closed(&self) -> Result<(), ModuleExitError> {
        match self.exit_code() {
            Some(exit_code) => Err(ModuleExitError { exit_code }),
            None => Ok(()),
        }
    }

    pub fn exit_code(&self) -> Option<u32> {
        (self.closed != 0).then_some((self.closed >> 32) as u32)
    }

    pub fn set_store_links(&mut self, links: ModuleLinks) {
        self.store_links = links;
    }

    pub fn rebuild_exports(&mut self) {
        self.exports.clear();
        for export in &self.source.export_section {
            self.exports.insert(export.name.clone(), export.clone());
        }
    }

    pub fn build_element_instances(
        &mut self,
        elements: &[ElementSegment],
    ) -> Result<(), ModuleInstantiationError> {
        self.element_instances = vec![Vec::new(); elements.len()];
        for (index, element) in elements.iter().enumerate() {
            if element.mode == ElementMode::Passive {
                let mut instance = Vec::with_capacity(element.init.len());
                for init in &element.init {
                    instance.push(self.evaluate_reference(init)?);
                }
                self.element_instances[index] = instance;
            }
        }
        Ok(())
    }

    pub fn validate_data(&self, data: &[DataSegment]) -> Result<(), ModuleInstantiationError> {
        let memory = self
            .memory_instance
            .as_ref()
            .ok_or(ModuleInstantiationError::MissingMemory)?;
        for (index, segment) in data.iter().enumerate() {
            if segment.is_passive() {
                continue;
            }
            let offset = self.evaluate_offset(&segment.offset_expression)?;
            let end = offset
                .checked_add(segment.init.len())
                .ok_or(ModuleInstantiationError::DataOutOfBounds { index })?;
            if end > memory.bytes.len() {
                return Err(ModuleInstantiationError::DataOutOfBounds { index });
            }
        }
        Ok(())
    }

    pub fn apply_data(&mut self, data: &[DataSegment]) -> Result<(), ModuleInstantiationError> {
        self.data_instances = vec![Vec::new(); data.len()];
        for (index, segment) in data.iter().enumerate() {
            self.data_instances[index] = segment.init.clone();
            if segment.is_passive() {
                continue;
            }

            let offset = self.evaluate_offset(&segment.offset_expression)?;
            let memory = self
                .memory_instance
                .as_mut()
                .ok_or(ModuleInstantiationError::MissingMemory)?;
            let end = offset
                .checked_add(segment.init.len())
                .ok_or(ModuleInstantiationError::DataOutOfBounds { index })?;
            if end > memory.bytes.len() {
                return Err(ModuleInstantiationError::DataOutOfBounds { index });
            }
            memory.bytes[offset..end].copy_from_slice(&segment.init);
        }
        Ok(())
    }

    pub fn apply_elements(
        &mut self,
        elements: &[ElementSegment],
    ) -> Result<(), ModuleInstantiationError> {
        for element in elements {
            if !element.is_active() || element.init.is_empty() {
                continue;
            }

            let offset = self.evaluate_offset(&element.offset_expr)?;
            let table_index = element.table_index as usize;
            let mut values = Vec::with_capacity(element.init.len());
            for init in &element.init {
                values.push(self.evaluate_reference(init)?);
            }

            let table = self
                .tables
                .get_mut(table_index)
                .ok_or(ModuleInstantiationError::MissingTable(element.table_index))?;
            let end = offset.checked_add(values.len()).ok_or(
                ModuleInstantiationError::TableOutOfBounds {
                    table_index: element.table_index,
                    offset,
                    len: table.elements.len(),
                },
            )?;
            if end > table.elements.len() {
                return Err(ModuleInstantiationError::TableOutOfBounds {
                    table_index: element.table_index,
                    offset,
                    len: table.elements.len(),
                });
            }
            table.elements[offset..end].clone_from_slice(&values);
        }
        Ok(())
    }

    pub fn define_memory(&mut self, memory: &Memory) {
        self.memory_type = Some(memory.clone());
        self.memory_instance = Some(MemoryInstance::new(memory));
    }

    pub fn add_defined_table(&mut self, table: &Table) {
        self.table_types.push(table.clone());
        self.tables.push(TableInstance::new(table));
    }

    pub fn add_defined_global(&mut self, global_type: GlobalType, value: u64) {
        self.global_types.push(global_type);
        self.globals.push(GlobalInstance {
            ty: global_type,
            value,
            value_hi: 0,
            mutable: global_type.mutable,
        });
    }

    pub fn add_defined_function(&mut self, type_id: FunctionTypeId, function_index: u32) {
        self.functions.push(FunctionInstance {
            module_name: self.module_name.clone(),
            function_index,
            type_id,
            is_host: self.source.is_host_module,
        });
    }

    pub fn evaluate_global_initializer(
        &self,
        expr: &ConstExpr,
    ) -> Result<u64, ModuleInstantiationError> {
        self.evaluate_const_expr(expr).map(|(values, _)| values[0])
    }

    fn evaluate_offset(&self, expr: &ConstExpr) -> Result<usize, ModuleInstantiationError> {
        let (values, value_type) = self.evaluate_const_expr(expr)?;
        match value_type {
            ValueType::I32 | ValueType::I64 => Ok(values[0] as usize),
            _ => Err(ModuleInstantiationError::InvalidConstExpression(
                "offset expression must evaluate to an integer".to_string(),
            )),
        }
    }

    fn evaluate_reference(
        &self,
        expr: &ConstExpr,
    ) -> Result<Option<u32>, ModuleInstantiationError> {
        let data = &expr.data;
        let Some(&opcode) = data.first() else {
            return Err(ModuleInstantiationError::InvalidConstExpression(
                "reference expression cannot be empty".to_string(),
            ));
        };

        match opcode {
            OPCODE_REF_NULL => Ok(None),
            OPCODE_REF_FUNC => {
                let (index, _) =
                    leb128::load_u32(data.get(1..).unwrap_or_default()).map_err(|err| {
                        ModuleInstantiationError::InvalidConstExpression(format!(
                            "read ref.func index: {err}"
                        ))
                    })?;
                Ok(Some(index))
            }
            _ => self
                .evaluate_const_expr(expr)
                .map(|(values, _)| Some(values[0] as u32)),
        }
    }

    fn evaluate_const_expr(
        &self,
        expr: &ConstExpr,
    ) -> Result<(Vec<u64>, ValueType), ModuleInstantiationError> {
        let data = &expr.data;
        let Some(&opcode) = data.first() else {
            return Err(ModuleInstantiationError::InvalidConstExpression(
                "constant expression cannot be empty".to_string(),
            ));
        };

        match opcode {
            OPCODE_I32_CONST => {
                let (value, read) =
                    leb128::load_i32(data.get(1..).unwrap_or_default()).map_err(|err| {
                        ModuleInstantiationError::InvalidConstExpression(format!("read i32: {err}"))
                    })?;
                self.expect_end(data, 1 + read)?;
                Ok((vec![u64::from(value as u32)], ValueType::I32))
            }
            OPCODE_I64_CONST => {
                let (value, read) =
                    leb128::load_i64(data.get(1..).unwrap_or_default()).map_err(|err| {
                        ModuleInstantiationError::InvalidConstExpression(format!("read i64: {err}"))
                    })?;
                self.expect_end(data, 1 + read)?;
                Ok((vec![value as u64], ValueType::I64))
            }
            OPCODE_GLOBAL_GET => {
                let (index, read) =
                    leb128::load_u32(data.get(1..).unwrap_or_default()).map_err(|err| {
                        ModuleInstantiationError::InvalidConstExpression(format!(
                            "read index of global: {err}"
                        ))
                    })?;
                self.expect_end(data, 1 + read)?;
                let global = self.globals.get(index as usize).ok_or_else(|| {
                    ModuleInstantiationError::InvalidConstExpression(
                        "global index out of range".to_string(),
                    )
                })?;
                let global_type = self.global_types.get(index as usize).ok_or_else(|| {
                    ModuleInstantiationError::InvalidConstExpression(
                        "global type index out of range".to_string(),
                    )
                })?;
                Ok((vec![global.value], global_type.val_type))
            }
            OPCODE_REF_NULL => {
                self.expect_end(data, 2)?;
                Ok((vec![0], ValueType(data[1])))
            }
            OPCODE_REF_FUNC => {
                let (index, read) =
                    leb128::load_u32(data.get(1..).unwrap_or_default()).map_err(|err| {
                        ModuleInstantiationError::InvalidConstExpression(format!(
                            "read ref.func index: {err}"
                        ))
                    })?;
                self.expect_end(data, 1 + read)?;
                Ok((vec![u64::from(index)], ValueType::FUNCREF))
            }
            _ => Err(ModuleInstantiationError::InvalidConstExpression(format!(
                "unsupported constant expression opcode: 0x{opcode:x}"
            ))),
        }
    }

    fn expect_end(
        &self,
        data: &[u8],
        immediate_end: usize,
    ) -> Result<(), ModuleInstantiationError> {
        if data.get(immediate_end) == Some(&OPCODE_END) {
            Ok(())
        } else {
            Err(ModuleInstantiationError::InvalidConstExpression(
                "constant expression missing end opcode".to_string(),
            ))
        }
    }

    fn set_exit_code(&mut self, exit_code: u32, flag: u64) -> bool {
        if self.closed != 0 {
            return false;
        }
        self.closed = flag | ((exit_code as u64) << 32);
        true
    }
}

impl fmt::Display for ModuleInstance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Module[{}]", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::const_expr::ConstExpr;
    use crate::module::{ElementMode, Export, ExternType, FunctionType, RefType};

    #[test]
    fn close_state_matches_go_encoding() {
        let mut module = ModuleInstance::new(1, "math", Module::default(), Vec::new());

        assert_eq!("math", module.name());
        assert_eq!("Module[math]", module.to_string());
        assert!(module.close_with_exit_code(255));
        assert_eq!(Some(255), module.exit_code());
        assert_eq!(
            EXIT_CODE_FLAG_RESOURCE_CLOSED | ((255_u64) << 32),
            module.closed
        );
        assert!(!module.close());
        assert_eq!(
            Err(ModuleExitError { exit_code: 255 }),
            module.fail_if_closed()
        );
    }

    #[test]
    fn apply_data_copies_bytes_and_tracks_segments() {
        let mut module = ModuleInstance::new(1, "data", Module::default(), Vec::new());
        module.define_memory(&Memory {
            min: 1,
            cap: 1,
            max: 1,
            ..Memory::default()
        });

        module
            .apply_data(&[
                DataSegment {
                    offset_expression: ConstExpr::from_i32(0),
                    init: vec![0x0a, 0x0f],
                    passive: false,
                },
                DataSegment {
                    offset_expression: ConstExpr::from_i32(8),
                    init: vec![0x01, 0x05],
                    passive: false,
                },
            ])
            .unwrap();

        let memory = module.memory_instance.as_ref().unwrap();
        assert_eq!(&[0x0a, 0x0f], &memory.bytes[..2]);
        assert_eq!(&[0x01, 0x05], &memory.bytes[8..10]);
        assert_eq!(
            vec![vec![0x0a, 0x0f], vec![0x01, 0x05]],
            module.data_instances
        );
    }

    #[test]
    fn apply_elements_initializes_passive_and_active_segments() {
        let source = Module {
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            type_section: vec![FunctionType {
                params: vec![ValueType::I32],
                results: vec![ValueType::I64],
                ..FunctionType::default()
            }],
            function_section: vec![0],
            ..Module::default()
        };
        let mut module = ModuleInstance::new(1, "elem", source, vec![7]);
        module.add_defined_function(7, 0);
        module.add_defined_table(&Table {
            min: 4,
            max: Some(4),
            ty: RefType::FUNCREF,
        });

        let elements = vec![
            ElementSegment {
                mode: ElementMode::Passive,
                ty: RefType::FUNCREF,
                init: vec![
                    ConstExpr::from_i32(0),
                    ConstExpr::from_opcode(OPCODE_REF_NULL, &[RefType::FUNCREF.0]),
                ],
                ..ElementSegment::default()
            },
            ElementSegment {
                mode: ElementMode::Active,
                table_index: 0,
                offset_expr: ConstExpr::from_i32(1),
                ty: RefType::FUNCREF,
                init: vec![
                    ConstExpr::from_i32(0),
                    ConstExpr::from_opcode(OPCODE_REF_NULL, &[RefType::FUNCREF.0]),
                ],
            },
        ];

        module.build_element_instances(&elements).unwrap();
        module.apply_elements(&elements).unwrap();

        assert_eq!(vec![Some(0), None], module.element_instances[0]);
        assert_eq!(vec![None, Some(0), None, None], module.tables[0].elements);
    }

    #[test]
    fn validate_data_reports_go_style_out_of_bounds_error() {
        let mut module = ModuleInstance::new(1, "data", Module::default(), Vec::new());
        module.memory_instance = Some(MemoryInstance::default());
        module.memory_instance.as_mut().unwrap().bytes = vec![0; 5];

        assert_eq!(
            Err(ModuleInstantiationError::DataOutOfBounds { index: 0 }),
            module.validate_data(&[DataSegment {
                offset_expression: ConstExpr::from_i32(5),
                init: vec![0],
                passive: false,
            }])
        );
    }

    #[test]
    fn build_element_instances_tracks_passive_externref_segments() {
        let mut module = ModuleInstance::new(1, "elem", Module::default(), Vec::new());
        module
            .build_element_instances(&[ElementSegment {
                mode: ElementMode::Passive,
                ty: RefType::EXTERNREF,
                init: vec![ConstExpr::from_opcode(
                    OPCODE_REF_NULL,
                    &[RefType::EXTERNREF.0],
                )],
                ..ElementSegment::default()
            }])
            .unwrap();

        assert_eq!(vec![vec![None]], module.element_instances);
    }

    #[test]
    fn apply_elements_reports_out_of_bounds_error() {
        let mut module = ModuleInstance::new(1, "elem", Module::default(), Vec::new());
        module.add_defined_table(&Table {
            min: 1,
            max: Some(1),
            ty: RefType::FUNCREF,
        });

        assert_eq!(
            Err(ModuleInstantiationError::TableOutOfBounds {
                table_index: 0,
                offset: 1,
                len: 1,
            }),
            module.apply_elements(&[ElementSegment {
                mode: ElementMode::Active,
                table_index: 0,
                offset_expr: ConstExpr::from_i32(1),
                ty: RefType::FUNCREF,
                init: vec![ConstExpr::from_opcode(
                    OPCODE_REF_NULL,
                    &[RefType::FUNCREF.0]
                )],
            }])
        );
    }
}
