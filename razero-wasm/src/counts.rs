#![doc = "Section counting helpers."]

use crate::module::{Module, SectionId};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleCounts {
    pub custom: u32,
    pub types: u32,
    pub imports: u32,
    pub functions: u32,
    pub tables: u32,
    pub memories: u32,
    pub globals: u32,
    pub exports: u32,
    pub starts: u32,
    pub elements: u32,
    pub code: u32,
    pub data: u32,
    pub data_count: u32,
}

impl ModuleCounts {
    pub fn from_module(module: &Module) -> Self {
        Self {
            custom: module.section_element_count(SectionId::CUSTOM),
            types: module.section_element_count(SectionId::TYPE),
            imports: module.section_element_count(SectionId::IMPORT),
            functions: module.section_element_count(SectionId::FUNCTION),
            tables: module.section_element_count(SectionId::TABLE),
            memories: module.section_element_count(SectionId::MEMORY),
            globals: module.section_element_count(SectionId::GLOBAL),
            exports: module.section_element_count(SectionId::EXPORT),
            starts: module.section_element_count(SectionId::START),
            elements: module.section_element_count(SectionId::ELEMENT),
            code: module.section_element_count(SectionId::CODE),
            data: module.section_element_count(SectionId::DATA),
            data_count: module.section_element_count(SectionId::DATA_COUNT),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ModuleCounts;
    use crate::module::{
        Code, CustomSection, DataSegment, ElementSegment, Export, FunctionType, Global, Import,
        Memory, Module, NameSection, SectionId, Table,
    };

    #[test]
    fn module_counts_match_section_element_count() {
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

        let counts = ModuleCounts::from_module(&module);

        assert_eq!(
            module.section_element_count(SectionId::CUSTOM),
            counts.custom
        );
        assert_eq!(module.section_element_count(SectionId::TYPE), counts.types);
        assert_eq!(
            module.section_element_count(SectionId::IMPORT),
            counts.imports
        );
        assert_eq!(
            module.section_element_count(SectionId::FUNCTION),
            counts.functions
        );
        assert_eq!(
            module.section_element_count(SectionId::TABLE),
            counts.tables
        );
        assert_eq!(
            module.section_element_count(SectionId::MEMORY),
            counts.memories
        );
        assert_eq!(
            module.section_element_count(SectionId::GLOBAL),
            counts.globals
        );
        assert_eq!(
            module.section_element_count(SectionId::EXPORT),
            counts.exports
        );
        assert_eq!(
            module.section_element_count(SectionId::START),
            counts.starts
        );
        assert_eq!(
            module.section_element_count(SectionId::ELEMENT),
            counts.elements
        );
        assert_eq!(module.section_element_count(SectionId::CODE), counts.code);
        assert_eq!(module.section_element_count(SectionId::DATA), counts.data);
        assert_eq!(
            module.section_element_count(SectionId::DATA_COUNT),
            counts.data_count
        );
    }
}
