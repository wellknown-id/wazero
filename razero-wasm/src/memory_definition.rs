#![doc = "Memory import/export metadata."]

use std::collections::BTreeMap;

use crate::module::{ExternType, ImportDesc, Index, Memory, Module};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryDefinition {
    pub module_name: String,
    pub index: Index,
    pub import_desc: Option<(String, String)>,
    pub export_names: Vec<String>,
    pub memory: Memory,
}

impl MemoryDefinition {
    pub fn module_name(&self) -> &str {
        &self.module_name
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn import(&self) -> Option<(&str, &str)> {
        self.import_desc
            .as_ref()
            .map(|(module_name, name)| (module_name.as_str(), name.as_str()))
    }

    pub fn export_names(&self) -> &[String] {
        &self.export_names
    }

    pub fn min(&self) -> u32 {
        self.memory.min
    }

    pub fn max(&self) -> (u32, bool) {
        (self.memory.max, self.memory.is_max_encoded)
    }
}

impl Module {
    pub fn imported_memories(&self) -> Vec<&MemoryDefinition> {
        self.memory_definition_section
            .iter()
            .filter(|definition| definition.import_desc.is_some())
            .collect()
    }

    pub fn exported_memories(&self) -> BTreeMap<String, &MemoryDefinition> {
        let mut definitions = BTreeMap::new();
        for definition in &self.memory_definition_section {
            for export_name in &definition.export_names {
                definitions.insert(export_name.clone(), definition);
            }
        }
        definitions
    }

    pub fn build_memory_definitions(&mut self) {
        let memory_count = self
            .import_section
            .iter()
            .filter(|import| import.ty == ExternType::MEMORY)
            .count()
            + usize::from(self.memory_section.is_some());

        self.memory_definition_section.clear();
        if memory_count == 0 {
            return;
        }

        self.memory_definition_section.reserve(memory_count);
        let module_name = self
            .name_section
            .as_ref()
            .map(|names| names.module_name.clone())
            .unwrap_or_default();

        let mut import_memory_index = 0;
        for import in &self.import_section {
            let ImportDesc::Memory(memory) = &import.desc else {
                continue;
            };

            self.memory_definition_section.push(MemoryDefinition {
                module_name: module_name.clone(),
                index: import_memory_index,
                import_desc: Some((import.module.clone(), import.name.clone())),
                export_names: Vec::new(),
                memory: memory.clone(),
            });
            import_memory_index += 1;
        }

        if let Some(memory) = &self.memory_section {
            self.memory_definition_section.push(MemoryDefinition {
                module_name: module_name.clone(),
                index: import_memory_index,
                import_desc: None,
                export_names: Vec::new(),
                memory: memory.clone(),
            });
        }

        for definition in &mut self.memory_definition_section {
            for export in &self.export_section {
                if export.ty == ExternType::MEMORY && export.index == definition.index {
                    definition.export_names.push(export.name.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MemoryDefinition;
    use crate::module::{Export, ExternType, Import, Memory, Module, NameSection};

    #[test]
    fn build_memory_definitions_tracks_imports_and_exports() {
        let mut module = Module {
            name_section: Some(NameSection {
                module_name: "mod".to_string(),
                ..NameSection::default()
            }),
            import_section: vec![
                Import::memory(
                    "env",
                    "imported",
                    Memory {
                        min: 1,
                        cap: 1,
                        max: 2,
                        is_max_encoded: true,
                        is_shared: false,
                    },
                ),
                Import::function("env", "f", 0),
            ],
            export_section: vec![
                Export {
                    ty: ExternType::MEMORY,
                    name: "imported_memory".to_string(),
                    index: 0,
                },
                Export {
                    ty: ExternType::MEMORY,
                    name: "defined_memory".to_string(),
                    index: 1,
                },
            ],
            memory_section: Some(Memory {
                min: 2,
                cap: 2,
                max: 3,
                is_max_encoded: true,
                is_shared: false,
            }),
            ..Module::default()
        };

        module.build_memory_definitions();

        assert_eq!(
            module.memory_definition_section,
            vec![
                MemoryDefinition {
                    module_name: "mod".to_string(),
                    index: 0,
                    import_desc: Some(("env".to_string(), "imported".to_string())),
                    export_names: vec!["imported_memory".to_string()],
                    memory: Memory {
                        min: 1,
                        cap: 1,
                        max: 2,
                        is_max_encoded: true,
                        is_shared: false,
                    },
                },
                MemoryDefinition {
                    module_name: "mod".to_string(),
                    index: 1,
                    import_desc: None,
                    export_names: vec!["defined_memory".to_string()],
                    memory: Memory {
                        min: 2,
                        cap: 2,
                        max: 3,
                        is_max_encoded: true,
                        is_shared: false,
                    },
                },
            ]
        );

        let imported = module.imported_memories();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].import(), Some(("env", "imported")));

        let exported = module.exported_memories();
        assert_eq!(exported["imported_memory"].index(), 0);
        assert_eq!(exported["defined_memory"].index(), 1);
    }

    #[test]
    fn build_memory_definitions_handles_empty_modules() {
        let mut module = Module::default();
        module.build_memory_definitions();
        assert!(module.memory_definition_section.is_empty());
        assert!(module.imported_memories().is_empty());
        assert!(module.exported_memories().is_empty());
    }
}
