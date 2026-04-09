#![doc = "Core decoded Wasm module data model."]

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use razero_features::CoreFeatures;

use crate::const_expr::{evaluate_const_expr, ConstExprError};
use crate::func_validation::{
    validate_wasm_function_with_context, wasm_function_uses_memory_with_context,
};
use crate::function_definition::FunctionDefinition;
use crate::host_func::HostFuncRef;
use crate::instruction::OPCODE_REF_FUNC;
use crate::memory_definition::MemoryDefinition;
use crate::table::check_segment_bounds;
use crate::wasmdebug::DWARFLines;

pub use crate::const_expr::ConstExpr;

pub type Index = u32;
pub type ModuleId = [u8; 32];

pub const MAXIMUM_GLOBALS: u32 = 1 << 27;
pub const MAXIMUM_FUNCTION_INDEX: u32 = 1 << 27;
pub const MAXIMUM_TABLE_INDEX: u32 = 1 << 27;
pub const MEMORY_LIMIT_PAGES: u32 = 65_536;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SectionId(pub u8);

impl SectionId {
    pub const CUSTOM: Self = Self(0);
    pub const TYPE: Self = Self(1);
    pub const IMPORT: Self = Self(2);
    pub const FUNCTION: Self = Self(3);
    pub const TABLE: Self = Self(4);
    pub const MEMORY: Self = Self(5);
    pub const GLOBAL: Self = Self(6);
    pub const EXPORT: Self = Self(7);
    pub const START: Self = Self(8);
    pub const ELEMENT: Self = Self(9);
    pub const CODE: Self = Self(10);
    pub const DATA: Self = Self(11);
    pub const DATA_COUNT: Self = Self(12);

    pub fn name(self) -> &'static str {
        match self.0 {
            0 => "custom",
            1 => "type",
            2 => "import",
            3 => "function",
            4 => "table",
            5 => "memory",
            6 => "global",
            7 => "export",
            8 => "start",
            9 => "element",
            10 => "code",
            11 => "data",
            12 => "data_count",
            _ => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ValueType(pub u8);

impl ValueType {
    pub const I32: Self = Self(0x7f);
    pub const I64: Self = Self(0x7e);
    pub const F32: Self = Self(0x7d);
    pub const F64: Self = Self(0x7c);
    pub const V128: Self = Self(0x7b);
    pub const FUNCREF: Self = Self(0x70);
    pub const EXTERNREF: Self = Self(0x6f);

    pub fn name(self) -> Cow<'static, str> {
        match self.0 {
            0x7f => Cow::Borrowed("i32"),
            0x7e => Cow::Borrowed("i64"),
            0x7d => Cow::Borrowed("f32"),
            0x7c => Cow::Borrowed("f64"),
            0x7b => Cow::Borrowed("v128"),
            0x70 => Cow::Borrowed("funcref"),
            0x6f => Cow::Borrowed("externref"),
            other => Cow::Owned(format!("unknown(0x{other:x})")),
        }
    }

    pub fn is_reference_type(self) -> bool {
        matches!(self.0, 0x70 | 0x6f)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExternType(pub u8);

impl ExternType {
    pub const FUNC: Self = Self(0);
    pub const TABLE: Self = Self(1);
    pub const MEMORY: Self = Self(2);
    pub const GLOBAL: Self = Self(3);

    pub fn name(self) -> Cow<'static, str> {
        match self.0 {
            0 => Cow::Borrowed("func"),
            1 => Cow::Borrowed("table"),
            2 => Cow::Borrowed("memory"),
            3 => Cow::Borrowed("global"),
            other => Cow::Owned(format!("unknown(0x{other:x})")),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RefType(pub u8);

impl RefType {
    pub const FUNCREF: Self = Self(ValueType::FUNCREF.0);
    pub const EXTERNREF: Self = Self(ValueType::EXTERNREF.0);

    pub fn name(self) -> Cow<'static, str> {
        match self.0 {
            0x70 => Cow::Borrowed("funcref"),
            0x6f => Cow::Borrowed("externref"),
            other => Cow::Owned(format!("unknown(0x{other:x})")),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ElementMode {
    #[default]
    Active,
    Passive,
    Declarative,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FunctionType {
    pub params: Vec<ValueType>,
    pub results: Vec<ValueType>,
    pub(crate) cached_key: String,
    pub param_num_in_u64: usize,
    pub result_num_in_u64: usize,
}

impl FunctionType {
    pub fn cache_num_in_u64(&mut self) {
        if self.param_num_in_u64 == 0 {
            self.param_num_in_u64 = value_type_slots(&self.params);
        }
        if self.result_num_in_u64 == 0 {
            self.result_num_in_u64 = value_type_slots(&self.results);
        }
    }

    pub fn equals_signature(&self, params: &[ValueType], results: &[ValueType]) -> bool {
        self.params == params && self.results == results
    }

    pub fn key(&mut self) -> &str {
        if self.cached_key.is_empty() {
            self.cached_key = self.render_key();
        }
        &self.cached_key
    }

    fn render_key(&self) -> String {
        let mut rendered = String::new();
        if self.params.is_empty() {
            rendered.push_str("v_");
        } else {
            for param in &self.params {
                rendered.push_str(param.name().as_ref());
            }
            rendered.push('_');
        }

        if self.results.is_empty() {
            rendered.push('v');
        } else {
            for result in &self.results {
                rendered.push_str(result.name().as_ref());
            }
        }
        rendered
    }
}

impl fmt::Display for FunctionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render_key())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportDesc {
    Func(Index),
    Table(Table),
    Memory(Memory),
    Global(GlobalType),
}

impl ImportDesc {
    pub fn extern_type(&self) -> ExternType {
        match self {
            Self::Func(_) => ExternType::FUNC,
            Self::Table(_) => ExternType::TABLE,
            Self::Memory(_) => ExternType::MEMORY,
            Self::Global(_) => ExternType::GLOBAL,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Import {
    pub ty: ExternType,
    pub module: String,
    pub name: String,
    pub desc: ImportDesc,
    pub index_per_type: Index,
}

impl Import {
    pub fn function(module: impl Into<String>, name: impl Into<String>, type_index: Index) -> Self {
        Self {
            ty: ExternType::FUNC,
            module: module.into(),
            name: name.into(),
            desc: ImportDesc::Func(type_index),
            index_per_type: 0,
        }
    }

    pub fn table(module: impl Into<String>, name: impl Into<String>, table: Table) -> Self {
        Self {
            ty: ExternType::TABLE,
            module: module.into(),
            name: name.into(),
            desc: ImportDesc::Table(table),
            index_per_type: 0,
        }
    }

    pub fn memory(module: impl Into<String>, name: impl Into<String>, memory: Memory) -> Self {
        Self {
            ty: ExternType::MEMORY,
            module: module.into(),
            name: name.into(),
            desc: ImportDesc::Memory(memory),
            index_per_type: 0,
        }
    }

    pub fn global(module: impl Into<String>, name: impl Into<String>, global: GlobalType) -> Self {
        Self {
            ty: ExternType::GLOBAL,
            module: module.into(),
            name: name.into(),
            desc: ImportDesc::Global(global),
            index_per_type: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Memory {
    pub min: u32,
    pub cap: u32,
    pub max: u32,
    pub is_max_encoded: bool,
    pub is_shared: bool,
}

impl Memory {
    pub fn validate(&self, memory_limit_pages: u32) -> Result<(), ModuleError> {
        let min = self.min;
        let capacity = self.cap;
        let max = self.max;

        if max > memory_limit_pages {
            return Err(ModuleError::Memory(format!(
                "max {max} pages ({}) over limit of {memory_limit_pages} pages ({})",
                pages_to_unit_of_bytes(max),
                pages_to_unit_of_bytes(memory_limit_pages)
            )));
        } else if min > memory_limit_pages {
            return Err(ModuleError::Memory(format!(
                "min {min} pages ({}) over limit of {memory_limit_pages} pages ({})",
                pages_to_unit_of_bytes(min),
                pages_to_unit_of_bytes(memory_limit_pages)
            )));
        } else if min > max {
            return Err(ModuleError::Memory(format!(
                "min {min} pages ({}) > max {max} pages ({})",
                pages_to_unit_of_bytes(min),
                pages_to_unit_of_bytes(max)
            )));
        } else if capacity < min {
            return Err(ModuleError::Memory(format!(
                "capacity {capacity} pages ({}) less than minimum {min} pages ({})",
                pages_to_unit_of_bytes(capacity),
                pages_to_unit_of_bytes(min)
            )));
        } else if capacity > memory_limit_pages {
            return Err(ModuleError::Memory(format!(
                "capacity {capacity} pages ({}) over limit of {memory_limit_pages} pages ({})",
                pages_to_unit_of_bytes(capacity),
                pages_to_unit_of_bytes(memory_limit_pages)
            )));
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GlobalType {
    pub val_type: ValueType,
    pub mutable: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Global {
    pub ty: GlobalType,
    pub init: ConstExpr,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Table {
    pub min: u32,
    pub max: Option<u32>,
    pub ty: RefType,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Export {
    pub ty: ExternType,
    pub name: String,
    pub index: Index,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum CodeBody {
    #[default]
    Wasm,
    Host,
}

#[derive(Clone, Default)]
pub struct Code {
    pub local_types: Vec<ValueType>,
    pub body: Vec<u8>,
    pub body_kind: CodeBody,
    pub body_offset_in_code_section: u64,
    pub host_func: Option<HostFuncRef>,
}

impl Code {
    pub fn is_host_function(&self) -> bool {
        self.body_kind == CodeBody::Host || self.host_func.is_some()
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Code")
            .field("local_types", &self.local_types)
            .field("body", &self.body)
            .field("body_kind", &self.body_kind)
            .field(
                "body_offset_in_code_section",
                &self.body_offset_in_code_section,
            )
            .field("has_host_func", &self.host_func.is_some())
            .finish()
    }
}

impl PartialEq for Code {
    fn eq(&self, other: &Self) -> bool {
        self.local_types == other.local_types
            && self.body == other.body
            && self.body_kind == other.body_kind
            && self.body_offset_in_code_section == other.body_offset_in_code_section
            && self.host_func.is_some() == other.host_func.is_some()
    }
}

impl Eq for Code {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ElementSegment {
    pub offset_expr: ConstExpr,
    pub table_index: Index,
    pub init: Vec<ConstExpr>,
    pub ty: RefType,
    pub mode: ElementMode,
}

impl ElementSegment {
    pub fn is_active(&self) -> bool {
        self.mode == ElementMode::Active
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DataSegment {
    pub offset_expression: ConstExpr,
    pub init: Vec<u8>,
    pub passive: bool,
}

impl DataSegment {
    pub fn is_passive(&self) -> bool {
        self.passive
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NameSection {
    pub module_name: String,
    pub function_names: NameMap,
    pub local_names: IndirectNameMap,
    pub result_names: IndirectNameMap,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CustomSection {
    pub name: String,
    pub data: Vec<u8>,
}

pub type NameMap = Vec<NameAssoc>;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NameAssoc {
    pub index: Index,
    pub name: String,
}

pub type IndirectNameMap = Vec<NameMapAssoc>;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NameMapAssoc {
    pub index: Index,
    pub name_map: NameMap,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Module {
    pub enabled_features: CoreFeatures,
    pub type_section: Vec<FunctionType>,
    pub import_section: Vec<Import>,
    pub import_function_count: Index,
    pub import_global_count: Index,
    pub import_memory_count: Index,
    pub import_table_count: Index,
    pub import_per_module: BTreeMap<String, Vec<usize>>,
    pub function_section: Vec<Index>,
    pub table_section: Vec<Table>,
    pub memory_section: Option<Memory>,
    pub global_section: Vec<Global>,
    pub export_section: Vec<Export>,
    pub exports: BTreeMap<String, usize>,
    pub start_section: Option<Index>,
    pub element_section: Vec<ElementSegment>,
    pub code_section: Vec<Code>,
    pub data_section: Vec<DataSegment>,
    pub name_section: Option<NameSection>,
    pub custom_sections: Vec<CustomSection>,
    pub data_count_section: Option<u32>,
    pub id: ModuleId,
    pub ensure_termination: bool,
    pub is_host_module: bool,
    pub function_definition_section: Vec<FunctionDefinition>,
    pub memory_definition_section: Vec<MemoryDefinition>,
    pub dwarf_lines: Option<DWARFLines>,
}

impl Module {
    pub fn validate(
        &self,
        enabled_features: CoreFeatures,
        memory_limit_pages: u32,
    ) -> Result<(), ModuleError> {
        self.validate_start_section()?;
        let declarations = self.all_declarations()?;

        self.validate_imports(enabled_features)?;
        self.validate_globals(
            enabled_features,
            &declarations.globals,
            declarations.functions.len() as u32,
            MAXIMUM_GLOBALS,
        )?;
        if let Some(memory) = declarations.memory.as_ref() {
            memory.validate(memory_limit_pages)?;
        }
        self.validate_memory(
            enabled_features,
            declarations.memory.as_ref(),
            &declarations.globals,
        )?;
        self.validate_exports(
            enabled_features,
            &declarations.functions,
            &declarations.globals,
            declarations.memory.as_ref(),
            &declarations.tables,
        )?;
        self.validate_functions(
            enabled_features,
            &declarations.tables,
            &declarations.globals,
            MAXIMUM_FUNCTION_INDEX,
        )?;
        self.validate_function_memory_usage(enabled_features, &declarations)?;
        self.validate_tables(enabled_features, &declarations.tables, MAXIMUM_TABLE_INDEX)?;
        self.validate_data_count_section()?;
        Ok(())
    }

    pub fn assign_module_id(
        &mut self,
        wasm: &[u8],
        listener_presence: &[bool],
        with_ensure_termination: bool,
    ) {
        let mut input = Vec::with_capacity(wasm.len() + listener_presence.len() * 5 + 1);
        input.extend_from_slice(wasm);

        for (index, has_listener) in listener_presence.iter().copied().enumerate() {
            input.extend_from_slice(&(index as u32).to_le_bytes());
            input.push(u8::from(has_listener));
        }

        input.push(u8::from(with_ensure_termination));
        self.id = sha256(&input);
    }

    pub fn rebuild_import_per_module(&mut self) {
        self.import_per_module.clear();
        for (index, import) in self.import_section.iter().enumerate() {
            self.import_per_module
                .entry(import.module.clone())
                .or_default()
                .push(index);
        }
    }

    pub fn rebuild_exports(&mut self) {
        self.exports.clear();
        for (index, export) in self.export_section.iter().enumerate() {
            self.exports.insert(export.name.clone(), index);
        }
    }

    pub fn type_of_function(&self, func_idx: Index) -> Option<&FunctionType> {
        let type_section_len = self.type_section.len() as u32;

        if func_idx < self.import_function_count {
            let mut current = 0;
            for import in &self.import_section {
                if import.ty != ExternType::FUNC {
                    continue;
                }

                if func_idx == current {
                    let type_idx = match import.desc {
                        ImportDesc::Func(type_idx) => type_idx,
                        _ => return None,
                    };
                    if type_idx >= type_section_len {
                        return None;
                    }
                    return self.type_section.get(type_idx as usize);
                }
                current += 1;
            }
        }

        let func_section_idx = func_idx.checked_sub(self.import_function_count)? as usize;
        let type_idx = *self.function_section.get(func_section_idx)? as usize;
        self.type_section.get(type_idx)
    }

    pub fn all_declarations(&self) -> Result<AllDeclarations, ModuleError> {
        let mut functions = Vec::new();
        let mut globals = Vec::new();
        let mut memory = None;
        let mut tables = Vec::new();

        for import in &self.import_section {
            match &import.desc {
                ImportDesc::Func(type_index) => functions.push(*type_index),
                ImportDesc::Global(global) => globals.push(*global),
                ImportDesc::Memory(imported_memory) => memory = Some(imported_memory.clone()),
                ImportDesc::Table(table) => tables.push(table.clone()),
            }
        }

        functions.extend(self.function_section.iter().copied());
        globals.extend(self.global_section.iter().map(|global| global.ty));

        if let Some(defined_memory) = &self.memory_section {
            if memory.is_some() {
                return Err(ModuleError::MultipleMemories(
                    "at most one table allowed in module".to_string(),
                ));
            }
            memory = Some(defined_memory.clone());
        }

        tables.extend(self.table_section.iter().cloned());

        Ok(AllDeclarations {
            functions,
            globals,
            memory,
            tables,
        })
    }

    pub fn section_element_count(&self, section_id: SectionId) -> u32 {
        match section_id {
            SectionId::CUSTOM => {
                self.custom_sections.len() as u32 + u32::from(self.name_section.is_some())
            }
            SectionId::TYPE => self.type_section.len() as u32,
            SectionId::IMPORT => self.import_section.len() as u32,
            SectionId::FUNCTION => self.function_section.len() as u32,
            SectionId::TABLE => self.table_section.len() as u32,
            SectionId::MEMORY => u32::from(self.memory_section.is_some()),
            SectionId::GLOBAL => self.global_section.len() as u32,
            SectionId::EXPORT => self.export_section.len() as u32,
            SectionId::START => u32::from(self.start_section.is_some()),
            SectionId::ELEMENT => self.element_section.len() as u32,
            SectionId::CODE => self.code_section.len() as u32,
            SectionId::DATA => self.data_section.len() as u32,
            SectionId::DATA_COUNT => u32::from(self.data_count_section.is_some()),
            _ => panic!("BUG: unknown section: {}", section_id.0),
        }
    }

    fn validate_start_section(&self) -> Result<(), ModuleError> {
        let Some(start_index) = self.start_section else {
            return Ok(());
        };

        let Some(function_type) = self.type_of_function(start_index) else {
            return Err(validation_error(format!(
                "invalid start function: func[{start_index}] has an invalid type"
            )));
        };
        if !function_type.params.is_empty() || !function_type.results.is_empty() {
            return Err(validation_error(format!(
                "invalid start function: func[{start_index}] must have an empty (nullary) signature: {function_type}"
            )));
        }
        Ok(())
    }

    fn validate_imports(&self, enabled_features: CoreFeatures) -> Result<(), ModuleError> {
        for (index, import) in self.import_section.iter().enumerate() {
            if import.module.is_empty() {
                return Err(validation_error(format!(
                    "import[{index}] has an empty module name"
                )));
            }
            match &import.desc {
                ImportDesc::Func(type_index) => {
                    if *type_index as usize >= self.type_section.len() {
                        return Err(validation_error(format!(
                            "invalid import[\"{}\".\"{}\"] function: type index out of range",
                            import.module, import.name
                        )));
                    }
                }
                ImportDesc::Global(global) if global.mutable => {
                    if !enabled_features.contains(CoreFeatures::MUTABLE_GLOBAL) {
                        return Err(validation_error(format!(
                            "invalid import[\"{}\".\"{}\"] global: feature \"mutable-global\" is disabled",
                            import.module, import.name
                        )));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn validate_globals(
        &self,
        enabled_features: CoreFeatures,
        globals: &[GlobalType],
        num_functions: u32,
        max_globals: u32,
    ) -> Result<(), ModuleError> {
        if globals.len() as u32 > max_globals {
            return Err(validation_error("too many globals in a module"));
        }

        for (index, global) in self.global_section.iter().enumerate() {
            validate_const_expression(
                &global.init,
                global.ty.val_type,
                num_functions,
                |global_index| {
                    self.resolve_const_expr_global_type(
                        enabled_features,
                        SectionId::GLOBAL,
                        index as u32,
                        global_index,
                    )
                },
            )?;
        }
        Ok(())
    }

    fn validate_memory(
        &self,
        enabled_features: CoreFeatures,
        memory: Option<&Memory>,
        _globals: &[GlobalType],
    ) -> Result<(), ModuleError> {
        let has_active_data = self
            .data_section
            .iter()
            .any(|segment| !segment.is_passive());
        if has_active_data && memory.is_none() {
            return Err(validation_error("unknown memory"));
        }

        for segment in self
            .data_section
            .iter()
            .filter(|segment| !segment.is_passive())
        {
            validate_const_expression(
                &segment.offset_expression,
                ValueType::I32,
                0,
                |global_index| {
                    self.resolve_const_expr_global_type(
                        enabled_features,
                        SectionId::DATA,
                        0,
                        global_index,
                    )
                },
            )
            .map_err(|err| validation_error(format!("calculate offset: {err}")))?;
        }
        Ok(())
    }

    fn validate_exports(
        &self,
        enabled_features: CoreFeatures,
        functions: &[Index],
        globals: &[GlobalType],
        memory: Option<&Memory>,
        tables: &[Table],
    ) -> Result<(), ModuleError> {
        for export in &self.export_section {
            match export.ty {
                ExternType::FUNC => {
                    if export.index as usize >= functions.len() {
                        return Err(validation_error(format!(
                            "unknown function for export[\"{}\"]",
                            export.name
                        )));
                    }
                }
                ExternType::GLOBAL => {
                    let Some(global) = globals.get(export.index as usize) else {
                        return Err(validation_error(format!(
                            "unknown global for export[\"{}\"]",
                            export.name
                        )));
                    };
                    if global.mutable && !enabled_features.contains(CoreFeatures::MUTABLE_GLOBAL) {
                        return Err(validation_error(format!(
                            "invalid export[\"{}\"] global[{}]: feature \"mutable-global\" is disabled",
                            export.name, export.index
                        )));
                    }
                }
                ExternType::MEMORY => {
                    if export.index != 0 || memory.is_none() {
                        return Err(validation_error(format!(
                            "memory for export[\"{}\"] out of range",
                            export.name
                        )));
                    }
                }
                ExternType::TABLE => {
                    if export.index as usize >= tables.len() {
                        return Err(validation_error(format!(
                            "table for export[\"{}\"] out of range",
                            export.name
                        )));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn validate_functions(
        &self,
        enabled_features: CoreFeatures,
        tables: &[Table],
        globals: &[GlobalType],
        maximum_function_index: u32,
    ) -> Result<(), ModuleError> {
        let function_count = self.section_element_count(SectionId::FUNCTION);
        let code_count = self.section_element_count(SectionId::CODE);
        let total_functions = self.import_function_count + function_count;
        if total_functions > maximum_function_index {
            return Err(validation_error(format!(
                "too many functions ({total_functions}) in a module"
            )));
        }
        if function_count == 0 && code_count == 0 {
            return Ok(());
        }
        if code_count != function_count {
            return Err(validation_error(format!(
                "code count ({code_count}) != function count ({function_count})"
            )));
        }

        let type_count = self.type_section.len() as u32;
        let declarations = self
            .all_declarations()
            .map_err(|err| validation_error(err.to_string()))?;
        let has_memory = declarations.memory.is_some();
        let functions = declarations.functions;
        let element_types: Vec<RefType> = self
            .element_section
            .iter()
            .map(|element| element.ty)
            .collect();
        let declared_function_indexes = self.declared_function_indexes(functions.len());
        let has_declared_function_indexes = declared_function_indexes
            .iter()
            .copied()
            .any(|declared| declared);
        for (index, type_index) in self.function_section.iter().copied().enumerate() {
            let desc = self.func_desc(SectionId::FUNCTION, index as u32);
            if type_index >= type_count {
                return Err(validation_error(format!(
                    "invalid {desc}: type section index {type_index} out of range"
                )));
            }
            let Some(code) = self.code_section.get(index) else {
                return Err(validation_error(format!(
                    "code count ({code_count}) != function count ({function_count})"
                )));
            };
            if !has_declared_function_indexes && code.body.contains(&OPCODE_REF_FUNC) {
                return Err(validation_error(format!(
                    "invalid {desc}: undeclared function reference"
                )));
            }
            let func_type = &self.type_section[type_index as usize];
            validate_wasm_function_with_context(
                code,
                enabled_features,
                &self.type_section,
                &functions,
                tables,
                globals,
                &element_types,
                &declared_function_indexes,
                has_memory,
                self.data_count_section,
                func_type,
            )
            .map_err(|err| validation_error(format!("invalid {desc}: {err}")))?;
        }
        Ok(())
    }

    fn validate_function_memory_usage(
        &self,
        enabled_features: CoreFeatures,
        declarations: &AllDeclarations,
    ) -> Result<(), ModuleError> {
        if declarations.memory.is_some() {
            return Ok(());
        }
        let element_types: Vec<RefType> = self
            .element_section
            .iter()
            .map(|element| element.ty)
            .collect();
        let declared_function_indexes =
            self.declared_function_indexes(declarations.functions.len());
        for (index, type_index) in self.function_section.iter().copied().enumerate() {
            let Some(code) = self.code_section.get(index) else {
                return Err(validation_error("code count does not match function count"));
            };
            let Some(func_type) = self.type_section.get(type_index as usize) else {
                return Err(validation_error(format!(
                    "invalid function[{index}] type index {type_index} out of range"
                )));
            };
            if wasm_function_uses_memory_with_context(
                code,
                enabled_features,
                &self.type_section,
                &declarations.functions,
                &declarations.tables,
                &declarations.globals,
                &element_types,
                &declared_function_indexes,
                declarations.memory.is_some(),
                self.data_count_section,
                func_type,
            )
            .map_err(|err| validation_error(format!("invalid function body: {err}")))?
            {
                return Err(validation_error("unknown memory"));
            }
        }
        Ok(())
    }

    fn declared_function_indexes(&self, function_count: usize) -> Vec<bool> {
        let mut declared = vec![false; function_count];
        for export in &self.export_section {
            if export.ty == ExternType::FUNC {
                if let Some(slot) = declared.get_mut(export.index as usize) {
                    *slot = true;
                }
            }
        }
        for (global_index, global) in self.global_section.iter().enumerate() {
            let _ = evaluate_const_expr(
                &global.init,
                |index| {
                    let value_type = self
                        .resolve_const_expr_global_type(
                            CoreFeatures::V2,
                            SectionId::GLOBAL,
                            global_index as u32,
                            index,
                        )
                        .map_err(ConstExprError::new)?;
                    Ok((value_type, 0, 0))
                },
                |function_index| {
                    if let Some(slot) = declared.get_mut(function_index as usize) {
                        *slot = true;
                    }
                    Ok(None)
                },
            );
        }
        for (element_index, element) in self.element_section.iter().enumerate() {
            for init in &element.init {
                let _ = evaluate_const_expr(
                    init,
                    |index| {
                        let value_type = self
                            .resolve_const_expr_global_type(
                                CoreFeatures::V2,
                                SectionId::ELEMENT,
                                element_index as u32,
                                index,
                            )
                            .map_err(ConstExprError::new)?;
                        Ok((value_type, 0, 0))
                    },
                    |function_index| {
                        if let Some(slot) = declared.get_mut(function_index as usize) {
                            *slot = true;
                        }
                        Ok(None)
                    },
                );
            }
        }
        declared
    }

    fn validate_tables(
        &self,
        enabled_features: CoreFeatures,
        tables: &[Table],
        maximum_table_index: u32,
    ) -> Result<(), ModuleError> {
        if tables.len() as u32 > maximum_table_index {
            return Err(validation_error(format!(
                "too many tables in a module: {} given with limit {maximum_table_index}",
                tables.len()
            )));
        }

        let function_count =
            self.import_function_count + self.section_element_count(SectionId::FUNCTION);
        let imported_table_count = self.import_table_count;
        for (index, element) in self.element_section.iter().enumerate() {
            for (init_index, init) in element.init.iter().enumerate() {
                let (_, init_type) = evaluate_const_expr(
                    init,
                    |global_index| {
                        let value_type = self
                            .resolve_const_expr_global_type(
                                enabled_features,
                                SectionId::ELEMENT,
                                index as u32,
                                global_index,
                            )
                            .map_err(ConstExprError::new)?;
                        Ok((value_type, 0, 0))
                    },
                    |function_index| {
                        if function_index >= function_count {
                            return Err(ConstExprError::new(format!(
                                "element[{index}].init[{init_index}] func index {function_index} out of range"
                            )));
                        }
                        Ok(None)
                    },
                )
                .map_err(|err| validation_error(err.to_string()))?;

                match element.ty {
                    RefType::FUNCREF if init_type != ValueType::FUNCREF => {
                        return Err(validation_error(format!(
                            "element[{index}].init[{init_index}] must be funcref but was {}",
                            init_type.name()
                        )));
                    }
                    RefType::EXTERNREF if init_type != ValueType::EXTERNREF => {
                        return Err(validation_error(format!(
                            "element[{index}].init[{init_index}] must be externref but was {}",
                            init_type.name()
                        )));
                    }
                    _ => {}
                }
            }

            if !element.is_active() {
                continue;
            }
            let Some(table) = tables.get(element.table_index as usize) else {
                return Err(validation_error(format!(
                    "unknown table {} as active element target",
                    element.table_index
                )));
            };
            if table.ty != element.ty {
                return Err(validation_error(format!(
                    "element type mismatch: table has {} but element has {}",
                    table.ty.name(),
                    element.ty.name()
                )));
            }

            let mut has_global_ref = false;
            let (offsets, offset_type) = evaluate_const_expr(
                &element.offset_expr,
                |global_index| {
                    has_global_ref = true;
                    let value_type = self
                        .resolve_const_expr_global_type(
                            enabled_features,
                            SectionId::ELEMENT,
                            index as u32,
                            global_index,
                        )
                        .map_err(ConstExprError::new)?;
                    if value_type != ValueType::I32 {
                        return Err(ConstExprError::new(format!(
                            "element[{index}] (global.get {global_index}): import[{index}].global.ValType != i32"
                        )));
                    }
                    Ok((ValueType::I32, 0, 0))
                },
                |_| Ok(None),
            )
            .map_err(|err| {
                validation_error(format!(
                    "element[{index}] couldn't evaluate offset expression: {err}"
                ))
            })?;
            if offset_type != ValueType::I32 {
                return Err(validation_error(format!(
                    "element[{index}] offset expression must return i32 but was {}",
                    offset_type.name()
                )));
            }

            if !enabled_features.contains(CoreFeatures::REFERENCE_TYPES)
                && !has_global_ref
                && element.table_index >= imported_table_count
                && !check_segment_bounds(
                    table.min,
                    u64::from(offsets[0] as u32) + element.init.len() as u64,
                )
            {
                return Err(validation_error(format!(
                    "element[{index}].init exceeds min table size"
                )));
            }
        }
        Ok(())
    }

    fn validate_data_count_section(&self) -> Result<(), ModuleError> {
        if let Some(data_count) = self.data_count_section {
            if data_count as usize != self.data_section.len() {
                return Err(validation_error(format!(
                    "data count section ({data_count}) doesn't match the length of data section ({})",
                    self.data_section.len()
                )));
            }
        }
        Ok(())
    }

    fn resolve_const_expr_global_type(
        &self,
        enabled_features: CoreFeatures,
        section_id: SectionId,
        section_index: Index,
        global_index: Index,
    ) -> Result<ValueType, String> {
        if global_index < self.import_global_count {
            let mut current = 0;
            for import in &self.import_section {
                if let ImportDesc::Global(global) = &import.desc {
                    if global_index == current {
                        return Ok(global.val_type);
                    }
                    current += 1;
                }
            }
            return Err(format!(
                "index {global_index} not found in imported globals"
            ));
        }

        if !enabled_features.contains(CoreFeatures::EXTENDED_CONST) {
            return Err(format!(
                "{}[{section_index}] (global.get {global_index}): out of range of imported globals",
                section_id.name()
            ));
        }

        let local_index = global_index - self.import_global_count;
        if section_id == SectionId::GLOBAL && local_index >= section_index {
            return Err(format!(
                "{}[{section_index}] global {local_index} out of range of initialized globals",
                section_id.name()
            ));
        }
        let Some(global) = self.global_section.get(local_index as usize) else {
            return Err(format!(
                "{}[{section_index}] (global.get {global_index}): out of range of initialized globals",
                section_id.name()
            ));
        };
        Ok(global.ty.val_type)
    }

    fn func_desc(&self, section_id: SectionId, section_index: Index) -> String {
        let function_index = section_index + self.import_function_count;
        let mut export_names: Vec<_> = self
            .export_section
            .iter()
            .filter(|export| export.ty == ExternType::FUNC && export.index == function_index)
            .map(|export| format!("\"{}\"", export.name))
            .collect();
        if export_names.is_empty() {
            return format!("{}[{section_index}]", section_id.name());
        }
        export_names.sort();
        format!(
            "{}[{section_index}] export[{}]",
            section_id.name(),
            export_names.join(",")
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AllDeclarations {
    pub functions: Vec<Index>,
    pub globals: Vec<GlobalType>,
    pub memory: Option<Memory>,
    pub tables: Vec<Table>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleError {
    Validation(String),
    Memory(String),
    MultipleMemories(String),
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) | Self::Memory(message) | Self::MultipleMemories(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for ModuleError {}

fn validate_const_expression<GR>(
    expr: &ConstExpr,
    expected_type: ValueType,
    num_functions: u32,
    mut global_resolver: GR,
) -> Result<(), ModuleError>
where
    GR: FnMut(Index) -> Result<ValueType, String>,
{
    let (_, actual_type) = evaluate_const_expr(
        expr,
        |global_index| {
            let value_type = global_resolver(global_index).map_err(ConstExprError::new)?;
            Ok((value_type, 0, 0))
        },
        |function_index| {
            if function_index >= num_functions {
                return Err(ConstExprError::new(format!(
                    "ref.func index out of range [{function_index}] with length {}",
                    num_functions.saturating_sub(1)
                )));
            }
            Ok(None)
        },
    )
    .map_err(|err| validation_error(err.to_string()))?;

    if actual_type != expected_type {
        return Err(validation_error(format!(
            "const expression type mismatch expected {} but got {}",
            expected_type.name(),
            actual_type.name()
        )));
    }
    Ok(())
}

fn validation_error(message: impl Into<String>) -> ModuleError {
    ModuleError::Validation(message.into())
}

fn value_type_slots(types: &[ValueType]) -> usize {
    types
        .iter()
        .map(|ty| if *ty == ValueType::V128 { 2 } else { 1 })
        .sum()
}

fn pages_to_unit_of_bytes(pages: u32) -> String {
    let kib = pages.saturating_mul(64);
    if kib < 1024 {
        return format!("{kib} Ki");
    }

    let mib = kib / 1024;
    if mib < 1024 {
        return format!("{mib} Mi");
    }

    let gib = mib / 1024;
    if gib < 1024 {
        return format!("{gib} Gi");
    }

    format!("{} Ti", gib / 1024)
}

fn sha256(input: &[u8]) -> [u8; 32] {
    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut state = H0;
    let mut padded = input.to_vec();
    padded.push(0x80);

    while padded.len() % 64 != 56 {
        padded.push(0);
    }

    padded.extend_from_slice(&((input.len() as u64) * 8).to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut schedule = [0u32; 64];
        for (index, word) in schedule.iter_mut().take(16).enumerate() {
            let base = index * 4;
            *word = u32::from_be_bytes([
                chunk[base],
                chunk[base + 1],
                chunk[base + 2],
                chunk[base + 3],
            ]);
        }

        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }

        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];

        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut digest = [0u8; 32];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    use razero_features::CoreFeatures;

    #[test]
    fn function_type_key_and_cache() {
        let mut function_type = FunctionType {
            params: vec![ValueType::I64, ValueType::F32, ValueType::F64],
            results: vec![ValueType::F32, ValueType::I32, ValueType::F64],
            ..FunctionType::default()
        };

        assert_eq!("i64f32f64_f32i32f64", function_type.to_string());
        assert_eq!("i64f32f64_f32i32f64", function_type.key());
        assert_eq!("i64f32f64_f32i32f64", function_type.cached_key);
    }

    #[test]
    fn function_type_cache_num_in_u64_counts_v128_as_two() {
        let mut function_type = FunctionType {
            params: vec![ValueType::I32, ValueType::V128],
            results: vec![ValueType::V128, ValueType::F64],
            ..FunctionType::default()
        };

        function_type.cache_num_in_u64();

        assert_eq!(3, function_type.param_num_in_u64);
        assert_eq!(3, function_type.result_num_in_u64);
    }

    #[test]
    fn section_id_name_matches_go() {
        assert_eq!("custom", SectionId::CUSTOM.name());
        assert_eq!("type", SectionId::TYPE.name());
        assert_eq!("import", SectionId::IMPORT.name());
        assert_eq!("function", SectionId::FUNCTION.name());
        assert_eq!("table", SectionId::TABLE.name());
        assert_eq!("memory", SectionId::MEMORY.name());
        assert_eq!("global", SectionId::GLOBAL.name());
        assert_eq!("export", SectionId::EXPORT.name());
        assert_eq!("start", SectionId::START.name());
        assert_eq!("element", SectionId::ELEMENT.name());
        assert_eq!("code", SectionId::CODE.name());
        assert_eq!("data", SectionId::DATA.name());
        assert_eq!("data_count", SectionId::DATA_COUNT.name());
        assert_eq!("unknown", SectionId(100).name());
    }

    #[test]
    fn memory_validate_matches_go_error_text() {
        let memory = Memory {
            min: 2,
            cap: 1,
            max: 2,
            ..Memory::default()
        };

        assert_eq!(
            Err(ModuleError::Memory(
                "capacity 1 pages (64 Ki) less than minimum 2 pages (128 Ki)".to_string()
            )),
            memory.validate(MEMORY_LIMIT_PAGES)
        );
    }

    #[test]
    fn data_segment_passive_flag() {
        let segment = DataSegment {
            passive: true,
            ..DataSegment::default()
        };

        assert!(segment.is_passive());
    }

    #[test]
    fn element_segment_active_flag() {
        let segment = ElementSegment {
            mode: ElementMode::Passive,
            ..ElementSegment::default()
        };

        assert!(!segment.is_active());
    }

    #[test]
    fn type_of_function_skips_non_function_imports() {
        let module = Module {
            start_section: Some(1),
            type_section: vec![
                FunctionType::default(),
                FunctionType {
                    results: vec![ValueType::I32],
                    ..FunctionType::default()
                },
            ],
            import_function_count: 2,
            import_section: vec![
                Import::function("env", "one", 1),
                Import::global("env", "g", GlobalType::default()),
                Import::function("env", "two", 0),
            ],
            ..Module::default()
        };

        assert_eq!(Some(&module.type_section[0]), module.type_of_function(1));
        assert_eq!(Some(&module.type_section[1]), module.type_of_function(0));
    }

    #[test]
    fn all_declarations_matches_go_shapes() {
        let module = Module {
            import_section: vec![
                Import::function("env", "f", 10_000),
                Import::global(
                    "env",
                    "g",
                    GlobalType {
                        mutable: false,
                        ..GlobalType::default()
                    },
                ),
                Import::memory(
                    "env",
                    "mem",
                    Memory {
                        min: 1,
                        max: 10,
                        ..Memory::default()
                    },
                ),
                Import::table(
                    "env",
                    "table",
                    Table {
                        min: 1,
                        ..Table::default()
                    },
                ),
            ],
            function_section: vec![10, 20, 30],
            global_section: vec![Global {
                ty: GlobalType {
                    mutable: true,
                    ..GlobalType::default()
                },
                ..Global::default()
            }],
            table_section: vec![Table {
                min: 10,
                ..Table::default()
            }],
            ..Module::default()
        };

        let declarations = module.all_declarations().unwrap();

        assert_eq!(vec![10_000, 10, 20, 30], declarations.functions);
        assert_eq!(
            vec![
                GlobalType {
                    mutable: false,
                    ..GlobalType::default()
                },
                GlobalType {
                    mutable: true,
                    ..GlobalType::default()
                }
            ],
            declarations.globals
        );
        assert_eq!(
            Some(Memory {
                min: 1,
                max: 10,
                ..Memory::default()
            }),
            declarations.memory
        );
        assert_eq!(
            vec![
                Table {
                    min: 1,
                    ..Table::default()
                },
                Table {
                    min: 10,
                    ..Table::default()
                }
            ],
            declarations.tables
        );
    }

    #[test]
    fn section_element_count_matches_go_behavior() {
        let module = Module {
            type_section: vec![FunctionType::default()],
            import_section: vec![Import::function("env", "f", 0)],
            function_section: vec![0],
            table_section: vec![Table::default(), Table::default()],
            memory_section: Some(Memory::default()),
            global_section: vec![Global::default()],
            export_section: vec![Export::default()],
            start_section: Some(0),
            element_section: vec![ElementSegment::default()],
            code_section: vec![Code::default()],
            data_section: vec![DataSegment::default()],
            name_section: Some(NameSection::default()),
            custom_sections: vec![CustomSection::default()],
            data_count_section: Some(1),
            ..Module::default()
        };

        assert_eq!(2, module.section_element_count(SectionId::CUSTOM));
        assert_eq!(1, module.section_element_count(SectionId::TYPE));
        assert_eq!(1, module.section_element_count(SectionId::IMPORT));
        assert_eq!(1, module.section_element_count(SectionId::FUNCTION));
        assert_eq!(2, module.section_element_count(SectionId::TABLE));
        assert_eq!(1, module.section_element_count(SectionId::MEMORY));
        assert_eq!(1, module.section_element_count(SectionId::GLOBAL));
        assert_eq!(1, module.section_element_count(SectionId::EXPORT));
        assert_eq!(1, module.section_element_count(SectionId::START));
        assert_eq!(1, module.section_element_count(SectionId::ELEMENT));
        assert_eq!(1, module.section_element_count(SectionId::CODE));
        assert_eq!(1, module.section_element_count(SectionId::DATA));
        assert_eq!(1, module.section_element_count(SectionId::DATA_COUNT));
    }

    #[test]
    fn assign_module_id_is_stable() {
        let mut module = Module::default();
        module.assign_module_id(b"\0asm", &[true, false, true], true);

        assert_eq!(
            [
                0x66, 0xac, 0x5c, 0x26, 0xc9, 0x7c, 0x7e, 0x67, 0x7e, 0x70, 0x71, 0x68, 0x9c, 0x5f,
                0xb5, 0x91, 0x59, 0x40, 0xd5, 0x81, 0xb6, 0xce, 0x45, 0x0d, 0xe8, 0x83, 0x60, 0x96,
                0x53, 0xea, 0x61, 0x39,
            ],
            module.id
        );
    }

    #[test]
    fn rebuild_maps_tracks_section_indexes() {
        let mut module = Module {
            import_section: vec![
                Import::function("env", "a", 0),
                Import::global("wasi", "b", GlobalType::default()),
                Import::function("env", "c", 1),
            ],
            export_section: vec![
                Export {
                    name: "run".to_string(),
                    ..Export::default()
                },
                Export {
                    name: "memory".to_string(),
                    ..Export::default()
                },
            ],
            ..Module::default()
        };

        module.rebuild_import_per_module();
        module.rebuild_exports();

        assert_eq!(Some(&vec![0, 2]), module.import_per_module.get("env"));
        assert_eq!(Some(&vec![1]), module.import_per_module.get("wasi"));
        assert_eq!(Some(&0), module.exports.get("run"));
        assert_eq!(Some(&1), module.exports.get("memory"));
    }

    #[test]
    fn module_validate_rejects_active_data_without_memory() {
        let module = Module {
            data_section: vec![DataSegment {
                offset_expression: ConstExpr::from_i32(0),
                init: vec![1],
                passive: false,
            }],
            ..Module::default()
        };

        assert_eq!(
            Err(ModuleError::Validation("unknown memory".to_string())),
            module.validate(CoreFeatures::empty(), MEMORY_LIMIT_PAGES)
        );
    }

    #[test]
    fn module_validate_rejects_mutable_global_export_without_feature() {
        let module = Module {
            export_section: vec![Export {
                ty: ExternType::GLOBAL,
                name: "g".to_string(),
                index: 0,
            }],
            global_section: vec![Global {
                ty: GlobalType {
                    val_type: ValueType::I32,
                    mutable: true,
                },
                init: ConstExpr::from_i32(0),
            }],
            ..Module::default()
        };

        assert_eq!(
            Err(ModuleError::Validation(
                "invalid export[\"g\"] global[0]: feature \"mutable-global\" is disabled"
                    .to_string()
            )),
            module.validate(CoreFeatures::empty(), MEMORY_LIMIT_PAGES)
        );
    }

    #[test]
    fn module_validate_checks_table_segment_bounds() {
        let module = Module {
            table_section: vec![Table {
                min: 1,
                max: Some(1),
                ty: RefType::FUNCREF,
            }],
            type_section: vec![FunctionType::default()],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x00, crate::instruction::OPCODE_END],
                ..Code::default()
            }],
            element_section: vec![ElementSegment {
                mode: ElementMode::Active,
                table_index: 0,
                offset_expr: ConstExpr::from_i32(1),
                ty: RefType::FUNCREF,
                init: vec![ConstExpr::from_opcode(
                    crate::instruction::OPCODE_REF_FUNC,
                    &[0],
                )],
            }],
            ..Module::default()
        };

        assert_eq!(
            Err(ModuleError::Validation(
                "element[0].init exceeds min table size".to_string()
            )),
            module.validate(CoreFeatures::empty(), MEMORY_LIMIT_PAGES)
        );
    }

    #[test]
    fn module_validate_rejects_unknown_tail_call_indirect_table() {
        let module = Module {
            type_section: vec![FunctionType::default()],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![
                    crate::instruction::OPCODE_I32_CONST,
                    0x00,
                    crate::instruction::OPCODE_TAIL_CALL_RETURN_CALL_INDIRECT,
                    0x00,
                    0x01,
                    crate::instruction::OPCODE_END,
                ],
                ..Code::default()
            }],
            ..Module::default()
        };

        assert_eq!(
            Err(ModuleError::Validation(
                "invalid function[0]: unknown table index: 1".to_string()
            )),
            module.validate(
                CoreFeatures::V2 | CoreFeatures::TAIL_CALL,
                MEMORY_LIMIT_PAGES
            )
        );
    }
}
