#![doc = "Validation harness for razero-wasm runtime-state modules."]

pub mod module {
    use crate::memory_definition::MemoryDefinition;

    pub type Index = u32;

    #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub struct ValueType(pub u8);

    impl ValueType {
        pub const I32: Self = Self(0x7f);
        pub const I64: Self = Self(0x7e);
        pub const F32: Self = Self(0x7d);
        pub const F64: Self = Self(0x7c);
        pub const V128: Self = Self(0x7b);
    }

    #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub struct RefType(pub u8);

    impl RefType {
        pub const FUNCREF: Self = Self(0x70);
        pub const EXTERNREF: Self = Self(0x6f);
    }

    #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub struct ExternType(pub u8);

    impl ExternType {
        pub const FUNC: Self = Self(0);
        pub const MEMORY: Self = Self(2);
    }

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct GlobalType {
        pub val_type: ValueType,
        pub mutable: bool,
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct Memory {
        pub min: u32,
        pub cap: u32,
        pub max: u32,
        pub is_max_encoded: bool,
        pub is_shared: bool,
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct Table {
        pub min: u32,
        pub max: Option<u32>,
        pub ty: RefType,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum ImportDesc {
        Func(Index),
        Memory(Memory),
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct Import {
        pub ty: ExternType,
        pub module: String,
        pub name: String,
        pub desc: ImportDesc,
    }

    impl Import {
        pub fn function(
            module: impl Into<String>,
            name: impl Into<String>,
            type_index: Index,
        ) -> Self {
            Self {
                ty: ExternType::FUNC,
                module: module.into(),
                name: name.into(),
                desc: ImportDesc::Func(type_index),
            }
        }

        pub fn memory(module: impl Into<String>, name: impl Into<String>, memory: Memory) -> Self {
            Self {
                ty: ExternType::MEMORY,
                module: module.into(),
                name: name.into(),
                desc: ImportDesc::Memory(memory),
            }
        }
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct Export {
        pub ty: ExternType,
        pub name: String,
        pub index: Index,
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct NameSection {
        pub module_name: String,
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct Module {
        pub import_section: Vec<Import>,
        pub export_section: Vec<Export>,
        pub name_section: Option<NameSection>,
        pub memory_section: Option<Memory>,
        pub memory_definition_section: Vec<MemoryDefinition>,
    }
}

#[path = "../../src/global.rs"]
pub mod global;
#[path = "../../src/memory.rs"]
pub mod memory;
#[path = "../../src/memory_definition.rs"]
pub mod memory_definition;
#[path = "../../src/table.rs"]
pub mod table;
