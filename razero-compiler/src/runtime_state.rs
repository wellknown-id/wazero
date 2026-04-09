use std::collections::BTreeMap;

use razero_wasm::const_expr::{evaluate_const_expr, ConstExpr, ConstExprError};
use razero_wasm::instruction::{OPCODE_END, OPCODE_REF_FUNC, OPCODE_REF_NULL};
use razero_wasm::leb128;
use razero_wasm::memory::MEMORY_PAGE_SIZE;
use razero_wasm::module::{ElementMode, Index, RefType, ValueType};

use crate::aot::{AotCompiledMetadata, AotElementSegmentMetadata};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LinkedRuntimePlan {
    pub memory_bytes: Option<Vec<u8>>,
    pub globals: Vec<LinkedGlobalValue>,
    pub tables: Vec<LinkedTablePlan>,
    pub type_ids: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LinkedGlobalValue {
    pub value_lo: u64,
    pub value_hi: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LinkedTablePlan {
    pub elements: Vec<Option<Index>>,
}

pub(crate) fn build_linked_runtime_plan(
    metadata: &AotCompiledMetadata,
) -> Result<LinkedRuntimePlan, String> {
    validate_linked_runtime_metadata(metadata)?;

    let function_indexes = metadata
        .functions
        .iter()
        .map(|function| function.wasm_function_index)
        .collect::<Vec<_>>();
    let function_lookup = metadata
        .functions
        .iter()
        .enumerate()
        .map(|(slot, function)| (function.wasm_function_index, slot))
        .collect::<BTreeMap<_, _>>();

    let mut globals = Vec::with_capacity(metadata.globals.len());
    for (index, (global, init)) in metadata
        .globals
        .iter()
        .zip(metadata.global_initializers.iter())
        .enumerate()
    {
        let (values, value_type) = evaluate_integer_const_expr(
            &init.init_expression,
            metadata,
            &globals,
            &function_lookup,
        )?;
        if value_type != global.val_type {
            return Err(format!(
                "global[{index}] initializer type {} does not match declared type {}",
                value_type.name(),
                global.val_type.name()
            ));
        }
        let value_lo = values.first().copied().unwrap_or_default();
        let value_hi = values.get(1).copied().unwrap_or_default();
        globals.push(LinkedGlobalValue { value_lo, value_hi });
    }

    let memory_bytes = match &metadata.memory {
        Some(memory) => {
            let mut bytes = vec![0; memory.min.saturating_mul(MEMORY_PAGE_SIZE) as usize];
            for (index, segment) in metadata.data_segments.iter().enumerate() {
                if segment.passive {
                    return Err(format!(
                        "data[{index}] is passive; linked runtime packaging only supports active data segments"
                    ));
                }
                let offset = evaluate_offset(
                    &segment.offset_expression,
                    metadata,
                    &globals,
                    &function_lookup,
                )?;
                let end = offset
                    .checked_add(segment.init.len())
                    .ok_or_else(|| format!("data[{index}] offset overflows memory"))?;
                if end > bytes.len() {
                    return Err(format!(
                        "data[{index}] range {offset}..{end} exceeds memory length {}",
                        bytes.len()
                    ));
                }
                bytes[offset..end].copy_from_slice(&segment.init);
            }
            Some(bytes)
        }
        None => {
            if !metadata.data_segments.is_empty() {
                return Err(
                    "active data segments require a defined local memory in linked runtime packaging"
                        .to_string(),
                );
            }
            None
        }
    };

    let mut tables = metadata
        .tables
        .iter()
        .enumerate()
        .map(|(index, table)| {
            if table.ty != RefType::FUNCREF {
                return Err(format!(
                    "table[{index}] uses {}, only funcref tables are supported by linked runtime packaging",
                    table.ty.name()
                ));
            }
            Ok(LinkedTablePlan {
                elements: vec![None; table.min as usize],
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    for (index, segment) in metadata.element_segments.iter().enumerate() {
        validate_active_element_segment(index, segment)?;
        let offset = evaluate_offset(
            &segment.offset_expression,
            metadata,
            &globals,
            &function_lookup,
        )?;
        let table = tables
            .get_mut(segment.table_index as usize)
            .ok_or_else(|| {
                format!(
                    "element[{index}] references unknown table {}",
                    segment.table_index
                )
            })?;
        let end = offset
            .checked_add(segment.init_expressions.len())
            .ok_or_else(|| format!("element[{index}] offset overflows table"))?;
        if end > table.elements.len() {
            return Err(format!(
                "element[{index}] range {offset}..{end} exceeds table length {}",
                table.elements.len()
            ));
        }
        for (entry_index, expr) in segment.init_expressions.iter().enumerate() {
            table.elements[offset + entry_index] =
                evaluate_element_reference(index, entry_index, expr, &function_indexes)?;
        }
    }

    Ok(LinkedRuntimePlan {
        memory_bytes,
        globals,
        tables,
        type_ids: metadata
            .types
            .iter()
            .enumerate()
            .map(|(index, _)| index as u32)
            .collect(),
    })
}

pub(crate) fn validate_linked_runtime_metadata(
    metadata: &AotCompiledMetadata,
) -> Result<(), String> {
    if metadata.module_shape.is_host_module {
        return Err("host modules are not supported by linked runtime packaging".to_string());
    }
    if metadata.module_shape.import_function_count != 0
        || metadata.module_shape.import_global_count != 0
        || metadata.module_shape.import_memory_count != 0
        || metadata.module_shape.import_table_count != 0
    {
        return Err(
            "linked runtime packaging currently requires modules without imports".to_string(),
        );
    }
    if metadata.ensure_termination {
        return Err(
            "linked runtime packaging does not support runtime-injected termination helpers"
                .to_string(),
        );
    }
    if metadata
        .memory
        .as_ref()
        .is_some_and(|memory| memory.is_shared)
    {
        return Err(
            "linked runtime packaging does not support shared memories or atomics integration"
                .to_string(),
        );
    }
    if metadata.functions.is_empty() {
        return Err(
            "linked module metadata does not contain any compiled local functions".to_string(),
        );
    }
    if metadata.globals.len() != metadata.global_initializers.len() {
        return Err(format!(
            "linked runtime metadata has {} globals but {} global initializers",
            metadata.globals.len(),
            metadata.global_initializers.len()
        ));
    }
    if metadata.module_shape.local_table_count != metadata.tables.len() as u32 {
        return Err("linked runtime metadata has inconsistent table counts".to_string());
    }
    if metadata.module_shape.local_global_count != metadata.globals.len() as u32 {
        return Err("linked runtime metadata has inconsistent global counts".to_string());
    }
    if metadata.module_shape.data_segment_count != metadata.data_segments.len() as u32 {
        return Err("linked runtime metadata has inconsistent data segment counts".to_string());
    }
    if metadata.module_shape.element_segment_count != metadata.element_segments.len() as u32 {
        return Err("linked runtime metadata has inconsistent element segment counts".to_string());
    }
    if let Some(start_index) = metadata.start_function_index {
        let function = metadata
            .functions
            .iter()
            .find(|function| function.wasm_function_index == start_index)
            .ok_or_else(|| {
                format!("linked module metadata has no local start function {start_index}")
            })?;
        let ty = metadata
            .types
            .get(function.type_index as usize)
            .ok_or_else(|| {
                format!(
                    "linked module metadata is missing type {}",
                    function.type_index
                )
            })?;
        if !ty.params.is_empty() || !ty.results.is_empty() {
            return Err("start functions must use the () -> () signature".to_string());
        }
    }
    Ok(())
}

fn evaluate_offset(
    expr: &[u8],
    metadata: &AotCompiledMetadata,
    globals: &[LinkedGlobalValue],
    function_lookup: &BTreeMap<Index, usize>,
) -> Result<usize, String> {
    let (values, value_type) =
        evaluate_integer_const_expr(expr, metadata, globals, function_lookup)?;
    match value_type {
        ValueType::I32 | ValueType::I64 => Ok(values.first().copied().unwrap_or_default() as usize),
        _ => Err("offset expression must evaluate to i32/i64".to_string()),
    }
}

fn evaluate_integer_const_expr(
    expr: &[u8],
    metadata: &AotCompiledMetadata,
    globals: &[LinkedGlobalValue],
    function_lookup: &BTreeMap<Index, usize>,
) -> Result<(Vec<u64>, ValueType), String> {
    evaluate_const_expr(
        &ConstExpr::new(expr.to_vec()),
        |index| {
            let global = globals
                .get(index as usize)
                .ok_or_else(const_expr_resolver_error)?;
            let global_type = metadata
                .globals
                .get(index as usize)
                .ok_or_else(const_expr_resolver_error)?;
            Ok((global_type.val_type, global.value_lo, global.value_hi))
        },
        |index| {
            function_lookup
                .contains_key(&index)
                .then_some(None)
                .ok_or_else(const_expr_resolver_error)
        },
    )
    .map_err(|err| err.to_string())
}

fn const_expr_resolver_error() -> ConstExprError {
    evaluate_const_expr(
        &ConstExpr::new(Vec::new()),
        |_| unreachable!(),
        |_| unreachable!(),
    )
    .unwrap_err()
}

fn validate_active_element_segment(
    index: usize,
    segment: &AotElementSegmentMetadata,
) -> Result<(), String> {
    if segment.mode != ElementMode::Active {
        return Err(format!(
            "element[{index}] uses {:?}; linked runtime packaging only supports active element segments",
            segment.mode
        ));
    }
    if segment.ty != RefType::FUNCREF {
        return Err(format!(
            "element[{index}] uses {}; linked runtime packaging only supports funcref element segments",
            segment.ty.name()
        ));
    }
    Ok(())
}

fn evaluate_element_reference(
    segment_index: usize,
    init_index: usize,
    expr: &[u8],
    function_indexes: &[Index],
) -> Result<Option<Index>, String> {
    let Some(&opcode) = expr.first() else {
        return Err(format!(
            "element[{segment_index}].init[{init_index}] is empty"
        ));
    };
    match opcode {
        OPCODE_REF_NULL => {
            if expr.last().copied() != Some(OPCODE_END) {
                return Err(format!(
                    "element[{segment_index}].init[{init_index}] has an invalid ref.null encoding"
                ));
            }
            Ok(None)
        }
        OPCODE_REF_FUNC => {
            let (func_index, used) = leb128::load_u32(expr.get(1..).unwrap_or_default())
                .map_err(|err| {
                    format!(
                        "element[{segment_index}].init[{init_index}] ref.func index: {err}"
                    )
                })?;
            if expr.get(1 + used).copied() != Some(OPCODE_END) {
                return Err(format!(
                    "element[{segment_index}].init[{init_index}] has trailing bytes"
                ));
            }
            if !function_indexes.contains(&func_index) {
                return Err(format!(
                    "element[{segment_index}].init[{init_index}] references missing local function {func_index}"
                ));
            }
            Ok(Some(func_index))
        }
        _ => Err(format!(
            "element[{segment_index}].init[{init_index}] uses an unsupported initializer opcode 0x{opcode:02x}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::build_linked_runtime_plan;
    use crate::aot::{AotCompiledMetadata, AotFunctionMetadata};
    use crate::wazevoapi::ModuleContextOffsetData;
    use razero_features::CoreFeatures;
    use razero_wasm::module::{
        Code, ConstExpr, DataSegment, ElementMode, ElementSegment, FunctionType, Global,
        GlobalType, Memory, Module, RefType, Table, ValueType,
    };

    fn function_type(params: &[ValueType], results: &[ValueType]) -> FunctionType {
        let mut ty = FunctionType::default();
        ty.params = params.to_vec();
        ty.results = results.to_vec();
        ty.cache_num_in_u64();
        ty
    }

    #[test]
    fn linked_runtime_plan_applies_global_data_and_element_initializers() {
        let module = Module {
            type_section: vec![
                function_type(&[], &[]),
                function_type(&[], &[ValueType::I32]),
            ],
            function_section: vec![1, 0],
            table_section: vec![Table {
                min: 1,
                max: Some(1),
                ty: RefType::FUNCREF,
            }],
            memory_section: Some(Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                ..Memory::default()
            }),
            global_section: vec![Global {
                ty: GlobalType {
                    val_type: ValueType::I32,
                    mutable: false,
                },
                init: ConstExpr::from_i32(0),
            }],
            data_section: vec![DataSegment {
                offset_expression: ConstExpr::from_opcode(0x23, &[0]),
                init: vec![5],
                passive: false,
            }],
            element_section: vec![ElementSegment {
                offset_expr: ConstExpr::from_opcode(0x23, &[0]),
                table_index: 0,
                init: vec![ConstExpr::from_opcode(0xd2, &[0])],
                ty: RefType::FUNCREF,
                mode: ElementMode::Active,
            }],
            enabled_features: CoreFeatures::V2,
            ..Module::default()
        };
        let metadata = AotCompiledMetadata::new(
            &module,
            Vec::new(),
            vec![
                AotFunctionMetadata {
                    local_function_index: 0,
                    wasm_function_index: 0,
                    type_index: 1,
                    executable_offset: 0,
                    executable_len: 0,
                },
                AotFunctionMetadata {
                    local_function_index: 1,
                    wasm_function_index: 1,
                    type_index: 0,
                    executable_offset: 0,
                    executable_len: 0,
                },
            ],
            Vec::new(),
            ModuleContextOffsetData::default(),
            Vec::new(),
            false,
        );

        let plan = build_linked_runtime_plan(&metadata).unwrap();
        assert_eq!(plan.memory_bytes.unwrap()[0], 5);
        assert_eq!(plan.globals.len(), 1);
        assert_eq!(plan.tables[0].elements, vec![Some(0)]);
    }

    #[test]
    fn linked_runtime_plan_supports_multiple_globals_tables_and_segments() {
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            function_section: vec![0, 0],
            table_section: vec![
                Table {
                    min: 3,
                    max: Some(3),
                    ty: RefType::FUNCREF,
                },
                Table {
                    min: 1,
                    max: Some(1),
                    ty: RefType::FUNCREF,
                },
            ],
            memory_section: Some(Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                ..Memory::default()
            }),
            global_section: vec![
                Global {
                    ty: GlobalType {
                        val_type: ValueType::I32,
                        mutable: false,
                    },
                    init: ConstExpr::from_i32(1),
                },
                Global {
                    ty: GlobalType {
                        val_type: ValueType::I64,
                        mutable: false,
                    },
                    init: ConstExpr::from_i64(9),
                },
            ],
            code_section: vec![
                Code {
                    body: vec![0x0b],
                    ..Code::default()
                },
                Code {
                    body: vec![0x0b],
                    ..Code::default()
                },
            ],
            data_section: vec![
                DataSegment {
                    offset_expression: ConstExpr::from_opcode(0x23, &[0]),
                    init: vec![0xaa],
                    passive: false,
                },
                DataSegment {
                    offset_expression: ConstExpr::from_i32(3),
                    init: vec![0xbb, 0xcc],
                    passive: false,
                },
            ],
            element_section: vec![
                ElementSegment {
                    offset_expr: ConstExpr::from_opcode(0x23, &[0]),
                    table_index: 0,
                    init: vec![
                        ConstExpr::from_opcode(0xd2, &[1]),
                        ConstExpr::from_opcode(0xd0, &[0x70]),
                    ],
                    ty: RefType::FUNCREF,
                    mode: ElementMode::Active,
                },
                ElementSegment {
                    offset_expr: ConstExpr::from_i32(0),
                    table_index: 1,
                    init: vec![ConstExpr::from_opcode(0xd2, &[0])],
                    ty: RefType::FUNCREF,
                    mode: ElementMode::Active,
                },
            ],
            enabled_features: CoreFeatures::V2,
            ..Module::default()
        };
        let metadata = AotCompiledMetadata::new(
            &module,
            Vec::new(),
            vec![
                AotFunctionMetadata {
                    local_function_index: 0,
                    wasm_function_index: 0,
                    type_index: 0,
                    executable_offset: 0,
                    executable_len: 0,
                },
                AotFunctionMetadata {
                    local_function_index: 1,
                    wasm_function_index: 1,
                    type_index: 0,
                    executable_offset: 0,
                    executable_len: 0,
                },
            ],
            Vec::new(),
            ModuleContextOffsetData::default(),
            Vec::new(),
            false,
        );

        let plan = build_linked_runtime_plan(&metadata).unwrap();
        let memory = plan.memory_bytes.unwrap();
        assert_eq!(memory[1], 0xaa);
        assert_eq!(&memory[3..5], &[0xbb, 0xcc]);
        assert_eq!(plan.globals.len(), 2);
        assert_eq!(plan.globals[0].value_lo, 1);
        assert_eq!(plan.globals[1].value_lo, 9);
        assert_eq!(plan.tables[0].elements, vec![None, Some(1), None]);
        assert_eq!(plan.tables[1].elements, vec![Some(0)]);
    }
}
