#![doc = "Function import/export metadata."]

use std::collections::BTreeMap;
use std::fmt::{Debug, Formatter};

use crate::host_func::HostFuncRef;
use crate::module::{
    ExternType, FunctionType, Import, ImportDesc, Index, IndirectNameMap, Module, ValueType,
};
use crate::wasmdebug;

#[derive(Clone, Default)]
pub struct FunctionDefinition {
    pub module_name: String,
    pub index: Index,
    pub name: String,
    pub debug_name: String,
    pub host_func: Option<HostFuncRef>,
    pub functype: FunctionType,
    pub import_desc: Option<Import>,
    pub export_names: Vec<String>,
    pub param_names: Vec<String>,
    pub result_names: Vec<String>,
}

impl FunctionDefinition {
    pub fn module_name(&self) -> &str {
        &self.module_name
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn debug_name(&self) -> &str {
        &self.debug_name
    }

    pub fn import(&self) -> Option<(&str, &str)> {
        self.import_desc
            .as_ref()
            .map(|import| (import.module.as_str(), import.name.as_str()))
    }

    pub fn export_names(&self) -> &[String] {
        &self.export_names
    }

    pub fn host_func(&self) -> Option<&HostFuncRef> {
        self.host_func.as_ref()
    }

    pub fn param_types(&self) -> &[ValueType] {
        &self.functype.params
    }

    pub fn param_names(&self) -> &[String] {
        &self.param_names
    }

    pub fn result_types(&self) -> &[ValueType] {
        &self.functype.results
    }

    pub fn result_names(&self) -> &[String] {
        &self.result_names
    }
}

impl Debug for FunctionDefinition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FunctionDefinition")
            .field("module_name", &self.module_name)
            .field("index", &self.index)
            .field("name", &self.name)
            .field("debug_name", &self.debug_name)
            .field("has_host_func", &self.host_func.is_some())
            .field("functype", &self.functype)
            .field("import_desc", &self.import_desc)
            .field("export_names", &self.export_names)
            .field("param_names", &self.param_names)
            .field("result_names", &self.result_names)
            .finish()
    }
}

impl PartialEq for FunctionDefinition {
    fn eq(&self, other: &Self) -> bool {
        self.module_name == other.module_name
            && self.index == other.index
            && self.name == other.name
            && self.debug_name == other.debug_name
            && self.host_func.is_some() == other.host_func.is_some()
            && self.functype == other.functype
            && self.import_desc == other.import_desc
            && self.export_names == other.export_names
            && self.param_names == other.param_names
            && self.result_names == other.result_names
    }
}

impl Eq for FunctionDefinition {}

impl Module {
    pub fn imported_functions(&mut self) -> Vec<&FunctionDefinition> {
        self.build_function_definitions();
        self.function_definition_section
            .iter()
            .filter(|definition| definition.import_desc.is_some())
            .collect()
    }

    pub fn exported_functions(&mut self) -> BTreeMap<String, &FunctionDefinition> {
        self.build_function_definitions();
        let mut definitions = BTreeMap::new();
        for definition in &self.function_definition_section {
            for export_name in &definition.export_names {
                definitions.insert(export_name.clone(), definition);
            }
        }
        definitions
    }

    pub fn function_definition(&mut self, index: Index) -> &FunctionDefinition {
        self.build_function_definitions();
        &self.function_definition_section[index as usize]
    }

    pub fn build_function_definitions(&mut self) {
        let function_count = self.import_function_count as usize + self.function_section.len();
        if self.function_definition_section.len() == function_count {
            return;
        }

        self.function_definition_section.clear();
        if function_count == 0 {
            return;
        }

        self.function_definition_section
            .resize(function_count, FunctionDefinition::default());

        let (module_name, function_names, local_names, result_names) = self
            .name_section
            .as_ref()
            .map(|names| {
                (
                    names.module_name.clone(),
                    names.function_names.clone(),
                    names.local_names.clone(),
                    names.result_names.clone(),
                )
            })
            .unwrap_or_default();

        let mut import_func_index = 0usize;
        for import in &self.import_section {
            let ImportDesc::Func(type_index) = import.desc else {
                continue;
            };

            let functype = self.type_section[type_index as usize].clone();
            self.function_definition_section[import_func_index] = FunctionDefinition {
                index: import_func_index as Index,
                functype,
                import_desc: Some(import.clone()),
                ..FunctionDefinition::default()
            };
            import_func_index += 1;
        }

        for (code_index, type_index) in self.function_section.iter().copied().enumerate() {
            let function_index = import_func_index + code_index;
            let code = &self.code_section[code_index];
            self.function_definition_section[function_index] = FunctionDefinition {
                index: function_index as Index,
                functype: self.type_section[type_index as usize].clone(),
                host_func: code.host_func.clone(),
                ..FunctionDefinition::default()
            };
        }

        let mut name_cursor = 0usize;
        for definition in &mut self.function_definition_section {
            let func_idx = definition.index;
            let mut func_name = String::new();
            while name_cursor < function_names.len() {
                let next = &function_names[name_cursor];
                if next.index > func_idx {
                    break;
                }
                if next.index == func_idx {
                    func_name = next.name.clone();
                    break;
                }
                name_cursor += 1;
            }

            definition.module_name = module_name.clone();
            definition.name = func_name;
            definition.debug_name = wasmdebug::func_name(&module_name, &definition.name, func_idx);
            definition.param_names =
                param_names(&local_names, func_idx, definition.functype.params.len());
            definition.result_names =
                param_names(&result_names, func_idx, definition.functype.results.len());

            for export in &self.export_section {
                if export.ty == ExternType::FUNC && export.index == func_idx {
                    definition.export_names.push(export.name.clone());
                }
            }
        }
    }
}

fn param_names(local_names: &IndirectNameMap, func_idx: Index, param_len: usize) -> Vec<String> {
    for map in local_names {
        if map.index != func_idx || map.name_map.len() < param_len {
            continue;
        }

        let mut names = vec![String::new(); param_len];
        for param in &map.name_map {
            if (param.index as usize) < param_len {
                names[param.index as usize] = param.name.clone();
            }
        }
        return names;
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::FunctionDefinition;
    use crate::host_func::stack_host_func;
    use crate::module::{
        Code, CodeBody, Export, ExternType, FunctionType, Import, Module, NameAssoc, NameMapAssoc,
        NameSection, ValueType,
    };

    #[test]
    fn build_function_definitions_tracks_imports_hosts_and_exports() {
        let host = stack_host_func(|_| Ok(()));
        let mut module = Module {
            import_function_count: 1,
            type_section: vec![
                FunctionType {
                    params: vec![ValueType::I32],
                    results: vec![ValueType::I64],
                    ..FunctionType::default()
                },
                FunctionType {
                    params: vec![ValueType::F64],
                    results: vec![],
                    ..FunctionType::default()
                },
            ],
            import_section: vec![Import::function("env", "imported", 0)],
            function_section: vec![1],
            code_section: vec![Code {
                body_kind: CodeBody::Host,
                host_func: Some(host.clone()),
                ..Code::default()
            }],
            export_section: vec![
                Export {
                    ty: ExternType::FUNC,
                    name: "imported_export".to_string(),
                    index: 0,
                },
                Export {
                    ty: ExternType::FUNC,
                    name: "host_export".to_string(),
                    index: 1,
                },
            ],
            name_section: Some(NameSection {
                module_name: "mod".to_string(),
                function_names: vec![NameAssoc {
                    index: 1,
                    name: "run".to_string(),
                }],
                local_names: vec![NameMapAssoc {
                    index: 1,
                    name_map: vec![NameAssoc {
                        index: 0,
                        name: "x".to_string(),
                    }],
                }],
                result_names: vec![NameMapAssoc {
                    index: 0,
                    name_map: vec![NameAssoc {
                        index: 0,
                        name: "ret".to_string(),
                    }],
                }],
            }),
            ..Module::default()
        };

        module.build_function_definitions();

        assert_eq!(2, module.function_definition_section.len());
        assert_eq!("mod.$0", module.function_definition(0).debug_name());
        assert_eq!(
            Some(("env", "imported")),
            module.function_definition(0).import()
        );
        assert_eq!(
            vec!["ret".to_string()],
            module.function_definition(0).result_names
        );
        assert!(module.function_definition(1).host_func().is_some());
        assert_eq!("run", module.function_definition(1).name());
        assert_eq!(
            vec!["x".to_string()],
            module.function_definition(1).param_names
        );

        let imported = module.imported_functions();
        assert_eq!(1, imported.len());
        assert_eq!(0, imported[0].index());

        let exported = module.exported_functions();
        assert_eq!(2, exported.len());
        assert_eq!(1, exported["host_export"].index());
    }

    #[test]
    fn function_definition_equality_ignores_host_pointer_identity() {
        let left = FunctionDefinition {
            host_func: Some(stack_host_func(|_| Ok(()))),
            ..FunctionDefinition::default()
        };
        let right = FunctionDefinition {
            host_func: Some(stack_host_func(|_| Ok(()))),
            ..FunctionDefinition::default()
        };

        assert_eq!(left, right);
    }
}
