#![doc = "Host module compilation helpers."]

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};

use crate::host_func::HostFuncRef;
use crate::module::{
    Code, CodeBody, Export, ExternType, FunctionType, Index, Module, NameAssoc, NameMapAssoc,
    NameSection, SectionId, ValueType,
};

#[derive(Clone, Default)]
pub struct HostFunc {
    pub export_name: String,
    pub name: String,
    pub param_types: Vec<ValueType>,
    pub param_names: Vec<String>,
    pub result_types: Vec<ValueType>,
    pub result_names: Vec<String>,
    pub code: Code,
}

impl HostFunc {
    pub fn new(
        export_name: impl Into<String>,
        name: impl Into<String>,
        param_types: Vec<ValueType>,
        result_types: Vec<ValueType>,
        host_func: HostFuncRef,
    ) -> Self {
        Self {
            export_name: export_name.into(),
            name: name.into(),
            param_types,
            result_types,
            code: Code {
                body_kind: CodeBody::Host,
                host_func: Some(host_func),
                ..Code::default()
            },
            ..Self::default()
        }
    }

    pub fn with_host_func(&self, host_func: HostFuncRef) -> Self {
        let mut cloned = self.clone();
        cloned.code.host_func = Some(host_func);
        cloned.code.body_kind = CodeBody::Host;
        cloned
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostModuleError {
    message: String,
}

impl HostModuleError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for HostModuleError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for HostModuleError {}

pub fn new_host_module<S>(
    module_name: impl Into<String>,
    export_names: &[S],
    name_to_host_func: &BTreeMap<String, HostFunc>,
    multi_value_enabled: bool,
) -> Result<Module, HostModuleError>
where
    S: AsRef<str>,
{
    let module_name = module_name.into();
    if module_name.is_empty() {
        return Err(HostModuleError::new("a module name must not be empty"));
    }

    let mut module = Module {
        name_section: Some(NameSection {
            module_name,
            ..NameSection::default()
        }),
        is_host_module: true,
        ..Module::default()
    };

    if !export_names.is_empty() {
        module.export_section.reserve(export_names.len());
        add_funcs(
            &mut module,
            export_names,
            name_to_host_func,
            multi_value_enabled,
        )?;
        module.rebuild_exports();
    }

    let mut module = Box::new(module);
    let unique = format!("@@@@@@@@{:p}", &*module);
    module.assign_module_id(unique.as_bytes(), &[], false);
    Ok(*module)
}

fn add_funcs<S>(
    module: &mut Module,
    export_names: &[S],
    name_to_host_func: &BTreeMap<String, HostFunc>,
    multi_value_enabled: bool,
) -> Result<(), HostModuleError>
where
    S: AsRef<str>,
{
    let name_section = module.name_section.get_or_insert_with(NameSection::default);
    let module_name = name_section.module_name.clone();

    let func_count = export_names.len();
    module
        .name_section
        .as_mut()
        .unwrap()
        .function_names
        .reserve(func_count);
    module.function_section.reserve(func_count);
    module.code_section.reserve(func_count);

    for (index, export_name) in export_names.iter().enumerate() {
        let export_key = export_name.as_ref();
        let Some(mut host_func) = name_to_host_func.get(export_key).cloned() else {
            return Err(HostModuleError::new(format!(
                "func[{module_name}.{export_key}] missing"
            )));
        };
        if host_func.name.is_empty() {
            host_func.name = export_key.to_string();
        }
        if host_func.export_name.is_empty() {
            host_func.export_name = export_key.to_string();
        }
        if host_func.code.host_func.is_none() {
            return Err(HostModuleError::new(format!(
                "func[{module_name}.{export_key}] has no host implementation"
            )));
        }
        if host_func.param_names.len() > 0
            && host_func.param_names.len() != host_func.param_types.len()
        {
            return Err(HostModuleError::new(format!(
                "func[{module_name}.{export_key}] has {} params, but {} params names",
                host_func.param_types.len(),
                host_func.param_names.len()
            )));
        }
        if host_func.result_names.len() > 0
            && host_func.result_names.len() != host_func.result_types.len()
        {
            return Err(HostModuleError::new(format!(
                "func[{module_name}.{export_key}] has {} results, but {} results names",
                host_func.result_types.len(),
                host_func.result_names.len()
            )));
        }

        let type_index = maybe_add_type(
            module,
            &host_func.param_types,
            &host_func.result_types,
            multi_value_enabled,
        )
        .map_err(|err| {
            HostModuleError::new(format!("func[{module_name}.{}] {err}", host_func.name))
        })?;

        module.function_section.push(type_index);
        module.code_section.push(host_func.code);

        let func_index = index as Index;
        module.export_section.push(Export {
            ty: ExternType::FUNC,
            name: host_func.export_name,
            index: func_index,
        });
        module
            .name_section
            .as_mut()
            .unwrap()
            .function_names
            .push(NameAssoc {
                index: func_index,
                name: host_func.name,
            });

        if !host_func.param_names.is_empty() {
            module
                .name_section
                .as_mut()
                .unwrap()
                .local_names
                .push(NameMapAssoc {
                    index: func_index,
                    name_map: host_func
                        .param_names
                        .into_iter()
                        .enumerate()
                        .map(|(index, name)| NameAssoc {
                            index: index as Index,
                            name,
                        })
                        .collect(),
                });
        }

        if !host_func.result_names.is_empty() {
            module
                .name_section
                .as_mut()
                .unwrap()
                .result_names
                .push(NameMapAssoc {
                    index: func_index,
                    name_map: host_func
                        .result_names
                        .into_iter()
                        .enumerate()
                        .map(|(index, name)| NameAssoc {
                            index: index as Index,
                            name,
                        })
                        .collect(),
                });
        }
    }

    Ok(())
}

fn maybe_add_type(
    module: &mut Module,
    params: &[ValueType],
    results: &[ValueType],
    multi_value_enabled: bool,
) -> Result<Index, HostModuleError> {
    if results.len() > 1 && !multi_value_enabled {
        return Err(HostModuleError::new(
            "multiple result types invalid as feature \"multi-value\" is disabled",
        ));
    }

    for (index, functype) in module.type_section.iter().enumerate() {
        if functype.equals_signature(params, results) {
            return Ok(index as Index);
        }
    }

    let type_index = module.section_element_count(SectionId::TYPE);
    let mut functype = FunctionType::default();
    functype.params = params.to_vec();
    functype.results = results.to_vec();
    module.type_section.push(functype);
    Ok(type_index)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{new_host_module, HostFunc};
    use crate::host_func::stack_host_func;
    use crate::module::ValueType;

    #[test]
    fn new_host_module_builds_module_metadata() {
        let mut funcs = BTreeMap::new();
        funcs.insert(
            "adder".to_string(),
            HostFunc {
                export_name: "adder".to_string(),
                name: "sum".to_string(),
                param_types: vec![ValueType::I32, ValueType::I32],
                param_names: vec!["lhs".to_string(), "rhs".to_string()],
                result_types: vec![ValueType::I32],
                result_names: vec!["result".to_string()],
                code: crate::module::Code {
                    body_kind: crate::module::CodeBody::Host,
                    host_func: Some(stack_host_func(|stack| {
                        stack[0] = stack[0].wrapping_add(stack[1]);
                        Ok(())
                    })),
                    ..crate::module::Code::default()
                },
            },
        );

        let mut module = new_host_module("env", &["adder"], &funcs, false).unwrap();

        assert!(module.is_host_module);
        assert_eq!("env", module.name_section.as_ref().unwrap().module_name);
        assert_eq!(1, module.function_section.len());
        assert_eq!(1, module.export_section.len());
        assert_ne!([0; 32], module.id);

        module.build_function_definitions();
        let definition = module.function_definition(0);
        assert_eq!("sum", definition.name());
        assert_eq!("env.sum", definition.debug_name());
        assert_eq!(
            vec!["lhs".to_string(), "rhs".to_string()],
            definition.param_names
        );
        assert_eq!(vec!["result".to_string()], definition.result_names);
    }

    #[test]
    fn new_host_module_reuses_function_types() {
        let func = HostFunc::new(
            "a",
            "",
            vec![ValueType::I32],
            vec![ValueType::I32],
            stack_host_func(|_| Ok(())),
        );
        let mut funcs = BTreeMap::new();
        funcs.insert("a".to_string(), func.clone());
        funcs.insert(
            "b".to_string(),
            func.with_host_func(stack_host_func(|_| Ok(()))),
        );

        let module = new_host_module("env", &["a", "b"], &funcs, false).unwrap();
        assert_eq!(1, module.type_section.len());
        assert_eq!(vec![0, 0], module.function_section);
    }

    #[test]
    fn new_host_module_rejects_invalid_signatures() {
        let mut funcs = BTreeMap::new();
        funcs.insert(
            "multi".to_string(),
            HostFunc::new(
                "multi",
                "",
                vec![],
                vec![ValueType::I32, ValueType::I64],
                stack_host_func(|_| Ok(())),
            ),
        );

        let err = new_host_module("env", &["multi"], &funcs, false).unwrap_err();
        assert_eq!(
            "func[env.multi] multiple result types invalid as feature \"multi-value\" is disabled",
            err.to_string()
        );
    }
}
