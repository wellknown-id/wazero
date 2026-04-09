#![doc = "Export and indirect-function lookup helpers for module instances."]

use std::fmt;

use crate::global::GlobalInstance;
use crate::memory::MemoryInstance;
use crate::module::{Export, ExternType};
use crate::module_instance::{FunctionInstance, FunctionTypeId, ModuleInstance};
use crate::table::{decode_function_reference, TableInstance};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LookupError {
    ExportNotFound {
        module: String,
        name: String,
    },
    WrongExternType {
        module: String,
        name: String,
        actual: ExternType,
        expected: ExternType,
    },
    FunctionIndexOutOfBounds(u32),
    GlobalIndexOutOfBounds(u32),
    MemoryNotInstantiated,
    TableIndexOutOfBounds(u32),
    TableElementOutOfBounds {
        table_index: u32,
        offset: u32,
    },
    UninitializedTableElement {
        table_index: u32,
        offset: u32,
    },
    TypeMismatch {
        expected: FunctionTypeId,
        actual: FunctionTypeId,
    },
}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExportNotFound { module, name } => {
                write!(f, "\"{name}\" is not exported in module \"{module}\"")
            }
            Self::WrongExternType {
                module,
                name,
                actual,
                expected,
            } => write!(
                f,
                "export \"{name}\" in module \"{module}\" is a {}, not a {}",
                actual.name(),
                expected.name()
            ),
            Self::FunctionIndexOutOfBounds(index) => write!(f, "function[{index}] out of bounds"),
            Self::GlobalIndexOutOfBounds(index) => write!(f, "global[{index}] out of bounds"),
            Self::MemoryNotInstantiated => f.write_str("memory not instantiated"),
            Self::TableIndexOutOfBounds(index) => write!(f, "table[{index}] out of bounds"),
            Self::TableElementOutOfBounds {
                table_index,
                offset,
            } => {
                write!(f, "table[{table_index}] element[{offset}] out of bounds")
            }
            Self::UninitializedTableElement {
                table_index,
                offset,
            } => {
                write!(f, "table[{table_index}] element[{offset}] is uninitialized")
            }
            Self::TypeMismatch { expected, actual } => {
                write!(
                    f,
                    "indirect call type mismatch: expected {expected}, got {actual}"
                )
            }
        }
    }
}

impl std::error::Error for LookupError {}

impl ModuleInstance {
    pub fn get_export(&self, name: &str, expected: ExternType) -> Result<&Export, LookupError> {
        let export = self
            .exports
            .get(name)
            .ok_or_else(|| LookupError::ExportNotFound {
                module: self.module_name.clone(),
                name: name.to_string(),
            })?;

        if export.ty != expected {
            return Err(LookupError::WrongExternType {
                module: self.module_name.clone(),
                name: name.to_string(),
                actual: export.ty,
                expected,
            });
        }

        Ok(export)
    }

    pub fn exported_function(&self, name: &str) -> Result<&FunctionInstance, LookupError> {
        let export = self.get_export(name, ExternType::FUNC)?;
        self.functions
            .get(export.index as usize)
            .ok_or(LookupError::FunctionIndexOutOfBounds(export.index))
    }

    pub fn exported_global(&self, name: &str) -> Result<&GlobalInstance, LookupError> {
        let export = self.get_export(name, ExternType::GLOBAL)?;
        self.globals
            .get(export.index as usize)
            .ok_or(LookupError::GlobalIndexOutOfBounds(export.index))
    }

    pub fn exported_memory(&self, name: &str) -> Result<&MemoryInstance, LookupError> {
        let _ = self.get_export(name, ExternType::MEMORY)?;
        self.memory_instance
            .as_ref()
            .ok_or(LookupError::MemoryNotInstantiated)
    }

    pub fn exported_table(&self, name: &str) -> Result<&TableInstance, LookupError> {
        let export = self.get_export(name, ExternType::TABLE)?;
        self.tables
            .get(export.index as usize)
            .ok_or(LookupError::TableIndexOutOfBounds(export.index))
    }

    pub fn lookup_function(
        &self,
        table_index: u32,
        type_id: FunctionTypeId,
        table_offset: u32,
    ) -> Result<&FunctionInstance, LookupError> {
        let table = self
            .tables
            .get(table_index as usize)
            .ok_or(LookupError::TableIndexOutOfBounds(table_index))?;
        let function_index = table
            .get(table_offset as usize)
            .ok_or(LookupError::TableElementOutOfBounds {
                table_index,
                offset: table_offset,
            })?
            .ok_or(LookupError::UninitializedTableElement {
                table_index,
                offset: table_offset,
            })?;
        let (module_id, function_index) = decode_function_reference(function_index);
        if module_id != self.id {
            return Err(LookupError::FunctionIndexOutOfBounds(function_index));
        }
        let function = self
            .functions
            .get(function_index as usize)
            .ok_or(LookupError::FunctionIndexOutOfBounds(function_index))?;

        if function.type_id != type_id {
            return Err(LookupError::TypeMismatch {
                expected: type_id,
                actual: function.type_id,
            });
        }

        Ok(function)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{Export, ExternType, Module, RefType, Table};
    use crate::module_instance::ModuleInstance;
    use crate::table::encode_function_reference;

    #[test]
    fn exported_lookup_uses_go_style_errors() {
        let mut module = ModuleInstance::new(1, "math", Module::default(), Vec::new());
        module.exports.insert(
            "memory".to_string(),
            Export {
                ty: ExternType::MEMORY,
                name: "memory".to_string(),
                index: 0,
            },
        );

        assert_eq!(
            Err(LookupError::ExportNotFound {
                module: "math".to_string(),
                name: "run".to_string()
            }),
            module.exported_function("run")
        );
        assert_eq!(
            Err(LookupError::WrongExternType {
                module: "math".to_string(),
                name: "memory".to_string(),
                actual: ExternType::MEMORY,
                expected: ExternType::FUNC,
            }),
            module.exported_function("memory")
        );
    }

    #[test]
    fn lookup_function_checks_type_ids() {
        let mut module = ModuleInstance::new(1, "table", Module::default(), Vec::new());
        module.functions.push(FunctionInstance {
            module_id: 1,
            module_name: "table".to_string(),
            function_index: 0,
            type_id: 7,
            is_host: false,
        });
        module.table_types.push(Table {
            min: 1,
            max: Some(1),
            ..Table::default()
        });
        module.tables.push(TableInstance::from_elements(
            vec![Some(encode_function_reference(1, 0))],
            1,
            Some(1),
            RefType::FUNCREF,
        ));

        assert_eq!(0, module.lookup_function(0, 7, 0).unwrap().function_index);
        assert_eq!(
            Err(LookupError::TypeMismatch {
                expected: 8,
                actual: 7
            }),
            module.lookup_function(0, 8, 0)
        );
    }
}
