#![doc = "Function-body validation helpers."]

use std::error::Error;
use std::fmt::{Display, Formatter};

use razero_features::CoreFeatures;

use crate::instruction::*;
use crate::leb128;
use crate::module::{Code, FunctionType, GlobalType, RefType, Table, ValueType};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionValidationError {
    EmptyBody,
    MissingEnd,
    InvalidInstruction(&'static str),
    InvalidMemoryAlignment,
    ZeroByteExpected(&'static str),
    InvalidTypeIndex {
        opcode_name: &'static str,
        type_index: u32,
    },
    UnknownGlobalIndex(u32),
    ImmutableGlobal(u32),
    TableIndexRequiresReferenceTypes(u32),
    UnknownTableIndex(u32),
    TableNotFuncref {
        actual: RefType,
        opcode_name: &'static str,
    },
    TypeMismatch,
}

impl Display for FunctionValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyBody => f.write_str("function body cannot be empty"),
            Self::MissingEnd => f.write_str("expr not end with OpcodeEnd"),
            Self::InvalidInstruction(context) => {
                write!(f, "invalid instruction immediate: {context}")
            }
            Self::InvalidMemoryAlignment => f.write_str("invalid memory alignment"),
            Self::ZeroByteExpected(context) => write!(f, "zero byte expected for {context}"),
            Self::InvalidTypeIndex {
                opcode_name,
                type_index,
            } => {
                write!(f, "invalid type index at {opcode_name}: {type_index}")
            }
            Self::UnknownGlobalIndex(global_index) => {
                write!(f, "unknown global index: {global_index}")
            }
            Self::ImmutableGlobal(global_index) => write!(f, "global {global_index} is immutable"),
            Self::TableIndexRequiresReferenceTypes(table_index) => write!(
                f,
                "table index must be zero but was {table_index}: feature \"reference-types\" is disabled"
            ),
            Self::UnknownTableIndex(table_index) => {
                write!(f, "unknown table index: {table_index}")
            }
            Self::TableNotFuncref {
                actual,
                opcode_name,
            } => {
                write!(
                    f,
                    "table is not funcref type but was {} for {opcode_name}",
                    actual.name()
                )
            }
            Self::TypeMismatch => f.write_str("type mismatch"),
        }
    }
}

impl Error for FunctionValidationError {}

pub fn validate_wasm_function(
    code: &Code,
    enabled_features: CoreFeatures,
    function_types: &[FunctionType],
    functions: &[u32],
    tables: &[Table],
    globals: &[GlobalType],
    func_type: &FunctionType,
) -> Result<(), FunctionValidationError> {
    validate_wasm_function_with_context(
        code,
        enabled_features,
        function_types,
        functions,
        tables,
        globals,
        &[],
        &[],
        false,
        None,
        func_type,
    )
}

pub fn validate_wasm_function_with_context(
    code: &Code,
    enabled_features: CoreFeatures,
    function_types: &[FunctionType],
    functions: &[u32],
    tables: &[Table],
    globals: &[GlobalType],
    element_types: &[RefType],
    declared_function_indexes: &[bool],
    has_memory: bool,
    data_count: Option<u32>,
    func_type: &FunctionType,
) -> Result<(), FunctionValidationError> {
    scan_wasm_function(
        code,
        IndirectCallValidation::Validate {
            enabled_features,
            function_types,
            functions,
            tables,
            globals,
            element_types,
            declared_function_indexes,
            has_memory,
            data_count,
        },
        func_type,
    )
    .map(|_| ())
}

pub fn wasm_function_uses_memory(
    code: &Code,
    enabled_features: CoreFeatures,
    function_types: &[FunctionType],
    functions: &[u32],
    tables: &[Table],
    globals: &[GlobalType],
    func_type: &FunctionType,
) -> Result<bool, FunctionValidationError> {
    wasm_function_uses_memory_with_context(
        code,
        enabled_features,
        function_types,
        functions,
        tables,
        globals,
        &[],
        &[],
        false,
        None,
        func_type,
    )
}

pub fn wasm_function_uses_memory_with_context(
    code: &Code,
    enabled_features: CoreFeatures,
    function_types: &[FunctionType],
    functions: &[u32],
    tables: &[Table],
    globals: &[GlobalType],
    element_types: &[RefType],
    declared_function_indexes: &[bool],
    has_memory: bool,
    data_count: Option<u32>,
    func_type: &FunctionType,
) -> Result<bool, FunctionValidationError> {
    scan_wasm_function(
        code,
        IndirectCallValidation::Validate {
            enabled_features,
            function_types,
            functions,
            tables,
            globals,
            element_types,
            declared_function_indexes,
            has_memory,
            data_count,
        },
        func_type,
    )
}

#[derive(Clone, Copy)]
enum IndirectCallValidation<'a> {
    Ignore,
    Validate {
        enabled_features: CoreFeatures,
        function_types: &'a [FunctionType],
        functions: &'a [u32],
        tables: &'a [Table],
        globals: &'a [GlobalType],
        element_types: &'a [RefType],
        declared_function_indexes: &'a [bool],
        has_memory: bool,
        data_count: Option<u32>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlKind {
    Function,
    Block,
    Loop,
    If { seen_else: bool },
}

#[derive(Clone, Debug)]
struct ControlFrame {
    kind: ControlKind,
    params: Vec<ValueType>,
    results: Vec<ValueType>,
    height: usize,
    unreachable: bool,
}

const VALUE_TYPE_UNKNOWN: ValueType = ValueType(0xff);

fn scan_wasm_function(
    code: &Code,
    indirect_call_validation: IndirectCallValidation<'_>,
    func_type: &FunctionType,
) -> Result<bool, FunctionValidationError> {
    if code.is_host_function() {
        return Ok(false);
    }
    let body = code.body.as_slice();
    match body.last().copied() {
        None => return Err(FunctionValidationError::EmptyBody),
        Some(OPCODE_END) => {}
        Some(_) => return Err(FunctionValidationError::MissingEnd),
    }

    let mut pc = 0;
    let mut uses_memory = false;
    let mut control_stack = vec![ControlFrame {
        kind: ControlKind::Function,
        params: Vec::new(),
        results: func_type.results.clone(),
        height: 0,
        unreachable: false,
    }];
    let mut value_stack = Vec::new();
    while pc < body.len() {
        let opcode = body[pc];
        pc += 1;
        if opcode == OPCODE_VEC_PREFIX {
            let IndirectCallValidation::Validate {
                enabled_features,
                has_memory,
                ..
            } = indirect_call_validation
            else {
                return Err(FunctionValidationError::InvalidInstruction("simd opcode"));
            };
            let (read, memory_here) = validate_vec_opcode(
                &body[pc..],
                enabled_features,
                has_memory,
                &mut value_stack,
                &control_stack,
            )?;
            uses_memory |= memory_here;
            pc += read;
            continue;
        }
        let mut pushed_control = None::<FunctionType>;
        let (immediate_len, memory_here) = match opcode {
            OPCODE_BLOCK | OPCODE_LOOP | OPCODE_IF => {
                let (block_type, read) = read_block_type(&body[pc..], &indirect_call_validation)?;
                pushed_control = Some(block_type);
                (read, false)
            }
            OPCODE_ELSE => {
                let Some(control) = control_stack.last_mut() else {
                    return Err(FunctionValidationError::InvalidInstruction("else"));
                };
                require_results(&mut value_stack, control)?;
                value_stack.truncate(control.height);
                value_stack.extend(control.params.iter().copied());
                control.unreachable = false;
                match &mut control.kind {
                    ControlKind::If { seen_else } if !*seen_else => *seen_else = true,
                    _ => return Err(FunctionValidationError::InvalidInstruction("else")),
                }
                (0, false)
            }
            OPCODE_END => {
                let Some(control) = control_stack.pop() else {
                    return Err(FunctionValidationError::InvalidInstruction("end"));
                };
                if matches!(control.kind, ControlKind::If { seen_else: false })
                    && control.params != control.results
                {
                    return Err(FunctionValidationError::TypeMismatch);
                }
                if control.kind == ControlKind::Function && pc < body.len() {
                    return Err(FunctionValidationError::InvalidInstruction("end"));
                }
                require_results(&mut value_stack, &control)?;
                if let Some(parent) = control_stack.last() {
                    value_stack.truncate(control.height);
                    value_stack.extend(control.results.iter().copied());
                    if parent.unreachable {
                        value_stack.truncate(parent.height);
                    }
                }
                (0, false)
            }
            OPCODE_BR | OPCODE_BR_IF | OPCODE_CALL | OPCODE_TAIL_CALL_RETURN_CALL => {
                (read_u32_len(&body[pc..], instruction_name(opcode))?, false)
            }
            OPCODE_BR_TABLE => {
                let (count, mut read) = read_u32(&body[pc..], "br_table")?;
                for _ in 0..count {
                    read += read_u32_len(&body[pc + read..], "br_table target")?;
                }
                (
                    read + read_u32_len(&body[pc + read..], "br_table default")?,
                    false,
                )
            }
            OPCODE_CALL_INDIRECT | OPCODE_TAIL_CALL_RETURN_CALL_INDIRECT => {
                let opcode_name = indirect_call_instruction_name(opcode);
                let (type_index, type_len) = read_u32(&body[pc..], opcode_name)?;
                let (table_index, table_len) =
                    read_u32(&body[pc + type_len..], "call_indirect table index")?;
                if let IndirectCallValidation::Validate {
                    enabled_features,
                    function_types,
                    tables,
                    ..
                } = indirect_call_validation
                {
                    if type_index as usize >= function_types.len() {
                        return Err(FunctionValidationError::InvalidTypeIndex {
                            opcode_name,
                            type_index,
                        });
                    }
                    if table_index != 0 {
                        enabled_features
                            .require_enabled(CoreFeatures::REFERENCE_TYPES)
                            .map_err(|_| {
                                FunctionValidationError::TableIndexRequiresReferenceTypes(
                                    table_index,
                                )
                            })?;
                    }
                    let table = tables
                        .get(table_index as usize)
                        .ok_or(FunctionValidationError::UnknownTableIndex(table_index))?;
                    if table.ty != RefType::FUNCREF {
                        return Err(FunctionValidationError::TableNotFuncref {
                            actual: table.ty,
                            opcode_name,
                        });
                    }
                }
                (type_len + table_len, false)
            }
            OPCODE_LOCAL_GET | OPCODE_LOCAL_SET | OPCODE_LOCAL_TEE | OPCODE_TABLE_GET
            | OPCODE_TABLE_SET | OPCODE_REF_FUNC => {
                (read_u32_len(&body[pc..], instruction_name(opcode))?, false)
            }
            OPCODE_GLOBAL_GET | OPCODE_GLOBAL_SET => {
                let (global_index, immediate_len) =
                    read_u32(&body[pc..], instruction_name(opcode))?;
                if let IndirectCallValidation::Validate { globals, .. } = indirect_call_validation {
                    let Some(global) = globals.get(global_index as usize) else {
                        return Err(FunctionValidationError::UnknownGlobalIndex(global_index));
                    };
                    if opcode == OPCODE_GLOBAL_SET && !global.mutable {
                        return Err(FunctionValidationError::ImmutableGlobal(global_index));
                    }
                }
                (immediate_len, false)
            }
            OPCODE_TYPED_SELECT => {
                let (count, read) = read_u32(&body[pc..], "typed select")?;
                (
                    read + read_fixed_len(
                        &body[pc + read..],
                        count as usize,
                        "typed select types",
                    )?,
                    false,
                )
            }
            OPCODE_I32_LOAD => (read_memarg_len(&body[pc..], 2)?, true),
            OPCODE_I64_LOAD => (read_memarg_len(&body[pc..], 3)?, true),
            OPCODE_F32_LOAD => (read_memarg_len(&body[pc..], 2)?, true),
            OPCODE_F64_LOAD => (read_memarg_len(&body[pc..], 3)?, true),
            OPCODE_I32_LOAD8_S | OPCODE_I32_LOAD8_U | OPCODE_I64_LOAD8_S | OPCODE_I64_LOAD8_U => {
                (read_memarg_len(&body[pc..], 0)?, true)
            }
            OPCODE_I32_LOAD16_S | OPCODE_I32_LOAD16_U | OPCODE_I64_LOAD16_S
            | OPCODE_I64_LOAD16_U => (read_memarg_len(&body[pc..], 1)?, true),
            OPCODE_I64_LOAD32_S | OPCODE_I64_LOAD32_U => (read_memarg_len(&body[pc..], 2)?, true),
            OPCODE_I32_STORE | OPCODE_F32_STORE => (read_memarg_len(&body[pc..], 2)?, true),
            OPCODE_I64_STORE | OPCODE_F64_STORE => (read_memarg_len(&body[pc..], 3)?, true),
            OPCODE_I32_STORE8 | OPCODE_I64_STORE8 => (read_memarg_len(&body[pc..], 0)?, true),
            OPCODE_I32_STORE16 | OPCODE_I64_STORE16 => (read_memarg_len(&body[pc..], 1)?, true),
            OPCODE_I64_STORE32 => (read_memarg_len(&body[pc..], 2)?, true),
            OPCODE_MEMORY_SIZE | OPCODE_MEMORY_GROW => {
                (read_zero_byte(&body[pc..], instruction_name(opcode))?, true)
            }
            OPCODE_I32_CONST => (read_i32_len(&body[pc..], instruction_name(opcode))?, false),
            OPCODE_I64_CONST => (read_i64_len(&body[pc..], instruction_name(opcode))?, false),
            OPCODE_F32_CONST => (
                read_fixed_len(&body[pc..], 4, instruction_name(opcode))?,
                false,
            ),
            OPCODE_F64_CONST => (
                read_fixed_len(&body[pc..], 8, instruction_name(opcode))?,
                false,
            ),
            OPCODE_REF_NULL => (
                read_fixed_len(&body[pc..], 1, instruction_name(opcode))?,
                false,
            ),
            OPCODE_MISC_PREFIX => read_misc_len(&body[pc..])?,
            OPCODE_ATOMIC_PREFIX => read_atomic_len(&body[pc..])?,
            _ => (0, false),
        };
        uses_memory |= memory_here;
        pc += immediate_len;
        match opcode {
            OPCODE_LOCAL_GET => {
                let (local_index, _) =
                    read_u32(&body[pc - immediate_len..pc], instruction_name(opcode))?;
                value_stack.push(resolve_local_type(code, func_type, local_index)?);
            }
            OPCODE_LOCAL_SET => {
                let (local_index, _) =
                    read_u32(&body[pc - immediate_len..pc], instruction_name(opcode))?;
                let local_type = resolve_local_type(code, func_type, local_index)?;
                pop_expected(&mut value_stack, &control_stack, local_type)?;
            }
            OPCODE_LOCAL_TEE => {
                let (local_index, _) =
                    read_u32(&body[pc - immediate_len..pc], instruction_name(opcode))?;
                let local_type = resolve_local_type(code, func_type, local_index)?;
                pop_expected(&mut value_stack, &control_stack, local_type)?;
                value_stack.push(local_type);
            }
            OPCODE_GLOBAL_GET => {
                let (global_index, _) =
                    read_u32(&body[pc - immediate_len..pc], instruction_name(opcode))?;
                if let IndirectCallValidation::Validate { globals, .. } = indirect_call_validation {
                    let global = globals
                        .get(global_index as usize)
                        .ok_or(FunctionValidationError::UnknownGlobalIndex(global_index))?;
                    value_stack.push(global.val_type);
                }
            }
            OPCODE_GLOBAL_SET => {
                let (global_index, _) =
                    read_u32(&body[pc - immediate_len..pc], instruction_name(opcode))?;
                if let IndirectCallValidation::Validate { globals, .. } = indirect_call_validation {
                    let global = globals
                        .get(global_index as usize)
                        .ok_or(FunctionValidationError::UnknownGlobalIndex(global_index))?;
                    pop_expected(&mut value_stack, &control_stack, global.val_type)?;
                }
            }
            OPCODE_TABLE_GET => {
                let (table_index, _) =
                    read_u32(&body[pc - immediate_len..pc], instruction_name(opcode))?;
                if let IndirectCallValidation::Validate { tables, .. } = indirect_call_validation {
                    let table = tables
                        .get(table_index as usize)
                        .ok_or(FunctionValidationError::UnknownTableIndex(table_index))?;
                    pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                    value_stack.push(ref_type_to_value_type(table.ty)?);
                }
            }
            OPCODE_TABLE_SET => {
                let (table_index, _) =
                    read_u32(&body[pc - immediate_len..pc], instruction_name(opcode))?;
                if let IndirectCallValidation::Validate { tables, .. } = indirect_call_validation {
                    let table = tables
                        .get(table_index as usize)
                        .ok_or(FunctionValidationError::UnknownTableIndex(table_index))?;
                    pop_expected(
                        &mut value_stack,
                        &control_stack,
                        ref_type_to_value_type(table.ty)?,
                    )?;
                    pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                }
            }
            OPCODE_I32_LOAD | OPCODE_I32_LOAD8_S | OPCODE_I32_LOAD8_U | OPCODE_I32_LOAD16_S
            | OPCODE_I32_LOAD16_U => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                value_stack.push(ValueType::I32);
            }
            OPCODE_I64_LOAD | OPCODE_I64_LOAD8_S | OPCODE_I64_LOAD8_U | OPCODE_I64_LOAD16_S
            | OPCODE_I64_LOAD16_U | OPCODE_I64_LOAD32_S | OPCODE_I64_LOAD32_U => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                value_stack.push(ValueType::I64);
            }
            OPCODE_F32_LOAD => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                value_stack.push(ValueType::F32);
            }
            OPCODE_F64_LOAD => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                value_stack.push(ValueType::F64);
            }
            OPCODE_I32_STORE | OPCODE_I32_STORE8 | OPCODE_I32_STORE16 => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
            }
            OPCODE_I64_STORE | OPCODE_I64_STORE8 | OPCODE_I64_STORE16 | OPCODE_I64_STORE32 => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I64)?;
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
            }
            OPCODE_F32_STORE => {
                pop_expected(&mut value_stack, &control_stack, ValueType::F32)?;
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
            }
            OPCODE_F64_STORE => {
                pop_expected(&mut value_stack, &control_stack, ValueType::F64)?;
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
            }
            OPCODE_MEMORY_SIZE => value_stack.push(ValueType::I32),
            OPCODE_MEMORY_GROW => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                value_stack.push(ValueType::I32);
            }
            OPCODE_I32_EQZ => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                value_stack.push(ValueType::I32);
            }
            OPCODE_I32_EQ | OPCODE_I32_NE | OPCODE_I32_LT_S | OPCODE_I32_LT_U | OPCODE_I32_GT_S
            | OPCODE_I32_GT_U | OPCODE_I32_LE_S | OPCODE_I32_LE_U | OPCODE_I32_GE_S
            | OPCODE_I32_GE_U => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                value_stack.push(ValueType::I32);
            }
            OPCODE_I32_CLZ | OPCODE_I32_CTZ | OPCODE_I32_POPCNT => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                value_stack.push(ValueType::I32);
            }
            OPCODE_I64_EQZ => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I64)?;
                value_stack.push(ValueType::I32);
            }
            OPCODE_I64_EQ | OPCODE_I64_NE | OPCODE_I64_LT_S | OPCODE_I64_LT_U | OPCODE_I64_GT_S
            | OPCODE_I64_GT_U | OPCODE_I64_LE_S | OPCODE_I64_LE_U | OPCODE_I64_GE_S
            | OPCODE_I64_GE_U => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I64)?;
                pop_expected(&mut value_stack, &control_stack, ValueType::I64)?;
                value_stack.push(ValueType::I32);
            }
            OPCODE_F32_EQ | OPCODE_F32_NE | OPCODE_F32_LT | OPCODE_F32_GT | OPCODE_F32_LE
            | OPCODE_F32_GE => {
                pop_expected(&mut value_stack, &control_stack, ValueType::F32)?;
                pop_expected(&mut value_stack, &control_stack, ValueType::F32)?;
                value_stack.push(ValueType::I32);
            }
            OPCODE_F64_EQ | OPCODE_F64_NE | OPCODE_F64_LT | OPCODE_F64_GT | OPCODE_F64_LE
            | OPCODE_F64_GE => {
                pop_expected(&mut value_stack, &control_stack, ValueType::F64)?;
                pop_expected(&mut value_stack, &control_stack, ValueType::F64)?;
                value_stack.push(ValueType::I32);
            }
            OPCODE_I64_CLZ | OPCODE_I64_CTZ | OPCODE_I64_POPCNT => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I64)?;
                value_stack.push(ValueType::I64);
            }
            OPCODE_F32_ABS | OPCODE_F32_NEG | OPCODE_F32_CEIL | OPCODE_F32_FLOOR
            | OPCODE_F32_TRUNC | OPCODE_F32_NEAREST | OPCODE_F32_SQRT => {
                pop_expected(&mut value_stack, &control_stack, ValueType::F32)?;
                value_stack.push(ValueType::F32);
            }
            OPCODE_F64_ABS | OPCODE_F64_NEG | OPCODE_F64_CEIL | OPCODE_F64_FLOOR
            | OPCODE_F64_TRUNC | OPCODE_F64_NEAREST | OPCODE_F64_SQRT => {
                pop_expected(&mut value_stack, &control_stack, ValueType::F64)?;
                value_stack.push(ValueType::F64);
            }
            OPCODE_I32_ADD | OPCODE_I32_SUB | OPCODE_I32_MUL | OPCODE_I32_DIV_S
            | OPCODE_I32_DIV_U | OPCODE_I32_REM_S | OPCODE_I32_REM_U | OPCODE_I32_AND
            | OPCODE_I32_OR | OPCODE_I32_XOR | OPCODE_I32_SHL | OPCODE_I32_SHR_S
            | OPCODE_I32_SHR_U | OPCODE_I32_ROTL | OPCODE_I32_ROTR => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                value_stack.push(ValueType::I32);
            }
            OPCODE_I64_ADD | OPCODE_I64_SUB | OPCODE_I64_MUL | OPCODE_I64_DIV_S
            | OPCODE_I64_DIV_U | OPCODE_I64_REM_S | OPCODE_I64_REM_U | OPCODE_I64_AND
            | OPCODE_I64_OR | OPCODE_I64_XOR | OPCODE_I64_SHL | OPCODE_I64_SHR_S
            | OPCODE_I64_SHR_U | OPCODE_I64_ROTL | OPCODE_I64_ROTR => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I64)?;
                pop_expected(&mut value_stack, &control_stack, ValueType::I64)?;
                value_stack.push(ValueType::I64);
            }
            OPCODE_F32_ADD | OPCODE_F32_SUB | OPCODE_F32_MUL | OPCODE_F32_DIV | OPCODE_F32_MIN
            | OPCODE_F32_MAX | OPCODE_F32_COPYSIGN => {
                pop_expected(&mut value_stack, &control_stack, ValueType::F32)?;
                pop_expected(&mut value_stack, &control_stack, ValueType::F32)?;
                value_stack.push(ValueType::F32);
            }
            OPCODE_F64_ADD | OPCODE_F64_SUB | OPCODE_F64_MUL | OPCODE_F64_DIV | OPCODE_F64_MIN
            | OPCODE_F64_MAX | OPCODE_F64_COPYSIGN => {
                pop_expected(&mut value_stack, &control_stack, ValueType::F64)?;
                pop_expected(&mut value_stack, &control_stack, ValueType::F64)?;
                value_stack.push(ValueType::F64);
            }
            OPCODE_I32_WRAP_I64
            | OPCODE_I32_TRUNC_F32_S
            | OPCODE_I32_TRUNC_F32_U
            | OPCODE_I32_TRUNC_F64_S
            | OPCODE_I32_TRUNC_F64_U
            | OPCODE_I32_REINTERPRET_F32 => {
                let input = if opcode == OPCODE_I32_WRAP_I64 {
                    ValueType::I64
                } else if matches!(
                    opcode,
                    OPCODE_I32_TRUNC_F32_S | OPCODE_I32_TRUNC_F32_U | OPCODE_I32_REINTERPRET_F32
                ) {
                    ValueType::F32
                } else {
                    ValueType::F64
                };
                pop_expected(&mut value_stack, &control_stack, input)?;
                value_stack.push(ValueType::I32);
            }
            OPCODE_I64_EXTEND_I32_S
            | OPCODE_I64_EXTEND_I32_U
            | OPCODE_I64_TRUNC_F32_S
            | OPCODE_I64_TRUNC_F32_U
            | OPCODE_I64_TRUNC_F64_S
            | OPCODE_I64_TRUNC_F64_U
            | OPCODE_I64_REINTERPRET_F64 => {
                let input = if matches!(opcode, OPCODE_I64_EXTEND_I32_S | OPCODE_I64_EXTEND_I32_U) {
                    ValueType::I32
                } else if matches!(opcode, OPCODE_I64_TRUNC_F32_S | OPCODE_I64_TRUNC_F32_U) {
                    ValueType::F32
                } else {
                    ValueType::F64
                };
                pop_expected(&mut value_stack, &control_stack, input)?;
                value_stack.push(ValueType::I64);
            }
            OPCODE_F32_CONVERT_I32_S | OPCODE_F32_CONVERT_I32_U | OPCODE_F32_REINTERPRET_I32 => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                value_stack.push(ValueType::F32);
            }
            OPCODE_F32_CONVERT_I64_S | OPCODE_F32_CONVERT_I64_U | OPCODE_F32_DEMOTE_F64 => {
                let input = if opcode == OPCODE_F32_DEMOTE_F64 {
                    ValueType::F64
                } else {
                    ValueType::I64
                };
                pop_expected(&mut value_stack, &control_stack, input)?;
                value_stack.push(ValueType::F32);
            }
            OPCODE_F64_CONVERT_I32_S | OPCODE_F64_CONVERT_I32_U => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                value_stack.push(ValueType::F64);
            }
            OPCODE_F64_CONVERT_I64_S
            | OPCODE_F64_CONVERT_I64_U
            | OPCODE_F64_PROMOTE_F32
            | OPCODE_F64_REINTERPRET_I64 => {
                let input = if opcode == OPCODE_F64_PROMOTE_F32 {
                    ValueType::F32
                } else {
                    ValueType::I64
                };
                pop_expected(&mut value_stack, &control_stack, input)?;
                value_stack.push(ValueType::F64);
            }
            OPCODE_CALL | OPCODE_TAIL_CALL_RETURN_CALL => {
                let (func_index, _) =
                    read_u32(&body[pc - immediate_len..pc], instruction_name(opcode))?;
                if let IndirectCallValidation::Validate {
                    function_types,
                    functions,
                    ..
                } = indirect_call_validation
                {
                    let type_index = *functions.get(func_index as usize).ok_or(
                        FunctionValidationError::InvalidInstruction("call function index"),
                    )?;
                    let call_type = function_types.get(type_index as usize).ok_or(
                        FunctionValidationError::InvalidInstruction("call type index"),
                    )?;
                    for param in call_type.params.iter().rev() {
                        pop_expected(&mut value_stack, &control_stack, *param)?;
                    }
                    if opcode == OPCODE_TAIL_CALL_RETURN_CALL {
                        let function = control_stack
                            .first()
                            .ok_or(FunctionValidationError::InvalidInstruction("return_call"))?;
                        if call_type.results != function.results {
                            return Err(FunctionValidationError::TypeMismatch);
                        }
                        mark_unreachable(&mut value_stack, &mut control_stack);
                    } else {
                        value_stack.extend(call_type.results.iter().copied());
                    }
                }
            }
            OPCODE_CALL_INDIRECT | OPCODE_TAIL_CALL_RETURN_CALL_INDIRECT => {
                let opcode_name = indirect_call_instruction_name(opcode);
                let (type_index, type_len) = read_u32(&body[pc - immediate_len..pc], opcode_name)?;
                let (_, table_len) = read_u32(
                    &body[pc - immediate_len + type_len..pc],
                    "call_indirect table index",
                )?;
                debug_assert_eq!(type_len + table_len, immediate_len);
                if let IndirectCallValidation::Validate { function_types, .. } =
                    indirect_call_validation
                {
                    let call_type = function_types.get(type_index as usize).ok_or(
                        FunctionValidationError::InvalidTypeIndex {
                            opcode_name,
                            type_index,
                        },
                    )?;
                    pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                    for param in call_type.params.iter().rev() {
                        pop_expected(&mut value_stack, &control_stack, *param)?;
                    }
                    if opcode == OPCODE_TAIL_CALL_RETURN_CALL_INDIRECT {
                        let function = control_stack
                            .first()
                            .ok_or(FunctionValidationError::InvalidInstruction(opcode_name))?;
                        if call_type.results != function.results {
                            return Err(FunctionValidationError::TypeMismatch);
                        }
                        mark_unreachable(&mut value_stack, &mut control_stack);
                    } else {
                        value_stack.extend(call_type.results.iter().copied());
                    }
                }
            }
            OPCODE_BR => {
                let (label_index, _) =
                    read_u32(&body[pc - immediate_len..pc], instruction_name(opcode))?;
                require_branch_target(&value_stack, &control_stack, label_index)?;
                mark_unreachable(&mut value_stack, &mut control_stack);
            }
            OPCODE_BR_IF => {
                let (label_index, _) =
                    read_u32(&body[pc - immediate_len..pc], instruction_name(opcode))?;
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                let target_types = branch_target_types(&control_stack, label_index)?.to_vec();
                for value_type in target_types.iter().rev() {
                    pop_expected(&mut value_stack, &control_stack, *value_type)?;
                }
                value_stack.extend(target_types);
            }
            OPCODE_BR_TABLE => {
                let (label_indexes, _) = read_label_table(&body[pc - immediate_len..pc])?;
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                let Some((&default_label, table_labels)) = label_indexes.split_last() else {
                    return Err(FunctionValidationError::InvalidInstruction("br_table"));
                };
                let mut default_types =
                    branch_target_types(&control_stack, default_label)?.to_vec();
                let reference_types_enabled = matches!(
                    indirect_call_validation,
                    IndirectCallValidation::Validate {
                        enabled_features,
                        ..
                    } if enabled_features.is_enabled(CoreFeatures::REFERENCE_TYPES)
                );
                if reference_types_enabled {
                    for offset in 0..default_types.len() {
                        let index = default_types.len() - 1 - offset;
                        let expected = default_types[index];
                        let actual = pop_any(&mut value_stack, &control_stack)?;
                        if actual == VALUE_TYPE_UNKNOWN {
                            default_types[index] = VALUE_TYPE_UNKNOWN;
                        } else if actual != expected {
                            return Err(FunctionValidationError::TypeMismatch);
                        }
                    }
                } else {
                    for value_type in default_types.iter().rev() {
                        pop_expected(&mut value_stack, &control_stack, *value_type)?;
                    }
                }
                for &label_index in table_labels {
                    let table_types = branch_target_types(&control_stack, label_index)?;
                    if table_types.len() != default_types.len() {
                        return Err(FunctionValidationError::TypeMismatch);
                    }
                    for (default_type, table_type) in default_types.iter().zip(table_types.iter()) {
                        if *default_type != VALUE_TYPE_UNKNOWN && *default_type != *table_type {
                            return Err(FunctionValidationError::TypeMismatch);
                        }
                    }
                }
                mark_unreachable(&mut value_stack, &mut control_stack);
            }
            OPCODE_BLOCK => {
                let block_type = pushed_control.clone().unwrap_or_default();
                for param in block_type.params.iter().rev() {
                    pop_expected(&mut value_stack, &control_stack, *param)?;
                }
                let height = value_stack.len();
                value_stack.extend(block_type.params.iter().copied());
                control_stack.push(ControlFrame {
                    kind: ControlKind::Block,
                    params: block_type.params,
                    results: block_type.results,
                    height,
                    unreachable: false,
                });
            }
            OPCODE_LOOP => {
                let block_type = pushed_control.clone().unwrap_or_default();
                for param in block_type.params.iter().rev() {
                    pop_expected(&mut value_stack, &control_stack, *param)?;
                }
                let height = value_stack.len();
                value_stack.extend(block_type.params.iter().copied());
                control_stack.push(ControlFrame {
                    kind: ControlKind::Loop,
                    params: block_type.params,
                    results: block_type.results,
                    height,
                    unreachable: false,
                });
            }
            OPCODE_IF => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                let block_type = pushed_control.clone().unwrap_or_default();
                for param in block_type.params.iter().rev() {
                    pop_expected(&mut value_stack, &control_stack, *param)?;
                }
                let height = value_stack.len();
                value_stack.extend(block_type.params.iter().copied());
                control_stack.push(ControlFrame {
                    kind: ControlKind::If { seen_else: false },
                    params: block_type.params,
                    results: block_type.results,
                    height,
                    unreachable: false,
                });
            }
            OPCODE_I32_CONST => value_stack.push(ValueType::I32),
            OPCODE_I64_CONST => value_stack.push(ValueType::I64),
            OPCODE_F32_CONST => value_stack.push(ValueType::F32),
            OPCODE_F64_CONST => value_stack.push(ValueType::F64),
            OPCODE_REF_NULL => {
                let ref_type = *body
                    .get(pc - immediate_len)
                    .ok_or(FunctionValidationError::InvalidInstruction("ref.null"))?;
                let value_type = match RefType(ref_type) {
                    RefType::FUNCREF => ValueType::FUNCREF,
                    RefType::EXTERNREF => ValueType::EXTERNREF,
                    _ => return Err(FunctionValidationError::InvalidInstruction("ref.null")),
                };
                value_stack.push(value_type);
            }
            OPCODE_REF_FUNC => {
                value_stack.push(ValueType::FUNCREF);
            }
            OPCODE_REF_IS_NULL => {
                let value = pop_any(&mut value_stack, &control_stack)?;
                match value {
                    ValueType::FUNCREF | ValueType::EXTERNREF | VALUE_TYPE_UNKNOWN => {
                        value_stack.push(ValueType::I32);
                    }
                    _ => return Err(FunctionValidationError::TypeMismatch),
                }
            }
            OPCODE_NOP => {}
            OPCODE_DROP => {
                pop_any(&mut value_stack, &control_stack)?;
            }
            OPCODE_SELECT | OPCODE_TYPED_SELECT => {
                pop_expected(&mut value_stack, &control_stack, ValueType::I32)?;
                let rhs = pop_any(&mut value_stack, &control_stack)?;
                let lhs = pop_any(&mut value_stack, &control_stack)?;
                if lhs != rhs && lhs != VALUE_TYPE_UNKNOWN && rhs != VALUE_TYPE_UNKNOWN {
                    return Err(FunctionValidationError::TypeMismatch);
                }
                let selected = if lhs == VALUE_TYPE_UNKNOWN { rhs } else { lhs };
                if opcode == OPCODE_SELECT
                    && matches!(selected, ValueType::FUNCREF | ValueType::EXTERNREF)
                {
                    return Err(FunctionValidationError::TypeMismatch);
                }
                value_stack.push(selected);
            }
            OPCODE_UNREACHABLE => mark_unreachable(&mut value_stack, &mut control_stack),
            OPCODE_RETURN => {
                let function = control_stack
                    .first()
                    .ok_or(FunctionValidationError::InvalidInstruction("return"))?
                    .clone();
                require_top_types_polymorphic(&value_stack, &control_stack, &function.results)?;
                mark_unreachable(&mut value_stack, &mut control_stack);
            }
            OPCODE_MISC_PREFIX => {
                validate_misc_opcode(
                    &body[pc - immediate_len..pc],
                    indirect_call_validation,
                    &mut value_stack,
                    &control_stack,
                )?;
            }
            OPCODE_ATOMIC_PREFIX => {
                validate_atomic_opcode(
                    &body[pc - immediate_len..pc],
                    indirect_call_validation,
                    &mut value_stack,
                    &control_stack,
                )?;
            }
            _ => {}
        }
    }
    if !control_stack.is_empty() {
        return Err(FunctionValidationError::MissingEnd);
    }
    Ok(uses_memory)
}

fn pop_any(
    value_stack: &mut Vec<ValueType>,
    control_stack: &[ControlFrame],
) -> Result<ValueType, FunctionValidationError> {
    let frame = control_stack
        .last()
        .ok_or(FunctionValidationError::InvalidInstruction("stack"))?;
    if value_stack.len() == frame.height {
        if frame.unreachable {
            return Ok(VALUE_TYPE_UNKNOWN);
        }
        return Err(FunctionValidationError::TypeMismatch);
    }
    value_stack
        .pop()
        .ok_or(FunctionValidationError::TypeMismatch)
}

fn pop_expected(
    value_stack: &mut Vec<ValueType>,
    control_stack: &[ControlFrame],
    expected: ValueType,
) -> Result<(), FunctionValidationError> {
    let actual = pop_any(value_stack, control_stack)?;
    if actual != expected && actual != VALUE_TYPE_UNKNOWN {
        return Err(FunctionValidationError::TypeMismatch);
    }
    Ok(())
}

fn require_results(
    value_stack: &mut Vec<ValueType>,
    frame: &ControlFrame,
) -> Result<(), FunctionValidationError> {
    if value_stack.len() < frame.height + frame.results.len() {
        if frame.unreachable {
            return Ok(());
        }
        return Err(FunctionValidationError::TypeMismatch);
    }
    for (index, expected) in frame.results.iter().enumerate() {
        let actual = value_stack[frame.height + index];
        if actual != *expected && actual != VALUE_TYPE_UNKNOWN {
            return Err(FunctionValidationError::TypeMismatch);
        }
    }
    if value_stack.len() != frame.height + frame.results.len() {
        return Err(FunctionValidationError::TypeMismatch);
    }
    Ok(())
}

fn mark_unreachable(value_stack: &mut Vec<ValueType>, control_stack: &mut [ControlFrame]) {
    if let Some(frame) = control_stack.last_mut() {
        value_stack.truncate(frame.height);
        frame.unreachable = true;
    }
}

fn resolve_local_type(
    code: &Code,
    func_type: &FunctionType,
    local_index: u32,
) -> Result<ValueType, FunctionValidationError> {
    let index = local_index as usize;
    if index < func_type.params.len() {
        return Ok(func_type.params[index]);
    }
    code.local_types
        .get(index - func_type.params.len())
        .copied()
        .ok_or(FunctionValidationError::InvalidInstruction("local index"))
}

fn require_branch_target(
    value_stack: &[ValueType],
    control_stack: &[ControlFrame],
    label_index: u32,
) -> Result<(), FunctionValidationError> {
    let label_index = label_index as usize;
    let frame_index = control_stack
        .len()
        .checked_sub(label_index + 1)
        .ok_or(FunctionValidationError::InvalidInstruction("branch label"))?;
    let frame = &control_stack[frame_index];
    let expected = branch_types_for_frame(frame);
    require_top_types_polymorphic(value_stack, control_stack, expected)
}

fn branch_target_types<'a>(
    control_stack: &'a [ControlFrame],
    label_index: u32,
) -> Result<&'a [ValueType], FunctionValidationError> {
    let label_index = label_index as usize;
    let frame_index = control_stack
        .len()
        .checked_sub(label_index + 1)
        .ok_or(FunctionValidationError::InvalidInstruction("branch label"))?;
    Ok(branch_types_for_frame(&control_stack[frame_index]))
}

fn branch_types_for_frame(frame: &ControlFrame) -> &[ValueType] {
    if matches!(frame.kind, ControlKind::Loop) {
        frame.params.as_slice()
    } else {
        frame.results.as_slice()
    }
}

fn require_top_types_polymorphic(
    value_stack: &[ValueType],
    control_stack: &[ControlFrame],
    expected: &[ValueType],
) -> Result<(), FunctionValidationError> {
    let mut stack = value_stack.to_vec();
    for value_type in expected.iter().rev() {
        pop_expected(&mut stack, control_stack, *value_type)?;
    }
    Ok(())
}

fn read_label_table(bytes: &[u8]) -> Result<(Vec<u32>, usize), FunctionValidationError> {
    let (count, mut read) = read_u32(bytes, "br_table")?;
    let mut labels = Vec::with_capacity(count as usize + 1);
    for _ in 0..count {
        let (label_index, len) = read_u32(&bytes[read..], "br_table target")?;
        labels.push(label_index);
        read += len;
    }
    let (default_label, len) = read_u32(&bytes[read..], "br_table default")?;
    labels.push(default_label);
    read += len;
    Ok((labels, read))
}

fn indirect_call_instruction_name(opcode: Opcode) -> &'static str {
    if opcode == OPCODE_TAIL_CALL_RETURN_CALL_INDIRECT {
        tail_call_instruction_name(OPCODE_TAIL_CALL_RETURN_CALL_INDIRECT)
    } else {
        instruction_name(opcode)
    }
}

fn read_fixed_len(
    bytes: &[u8],
    len: usize,
    context: &'static str,
) -> Result<usize, FunctionValidationError> {
    if bytes.len() < len {
        return Err(FunctionValidationError::InvalidInstruction(context));
    }
    Ok(len)
}

fn read_zero_byte(bytes: &[u8], context: &'static str) -> Result<usize, FunctionValidationError> {
    match bytes.first().copied() {
        Some(0x00) => Ok(1),
        Some(_) | None => Err(FunctionValidationError::ZeroByteExpected(context)),
    }
}

fn read_u32(bytes: &[u8], context: &'static str) -> Result<(u32, usize), FunctionValidationError> {
    leb128::decode_u32(bytes).map_err(|_| FunctionValidationError::InvalidInstruction(context))
}

fn read_u32_len(bytes: &[u8], context: &'static str) -> Result<usize, FunctionValidationError> {
    read_u32(bytes, context).map(|(_, read)| read)
}

fn read_i32_len(bytes: &[u8], context: &'static str) -> Result<usize, FunctionValidationError> {
    leb128::decode_i32(bytes)
        .map(|(_, read)| read)
        .map_err(|_| FunctionValidationError::InvalidInstruction(context))
}

fn read_i64_len(bytes: &[u8], context: &'static str) -> Result<usize, FunctionValidationError> {
    leb128::decode_i64(bytes)
        .map(|(_, read)| read)
        .map_err(|_| FunctionValidationError::InvalidInstruction(context))
}

fn read_block_type(
    bytes: &[u8],
    indirect_call_validation: &IndirectCallValidation<'_>,
) -> Result<(FunctionType, usize), FunctionValidationError> {
    let (block_type, read) = leb128::decode_i33_as_i64(bytes)
        .map_err(|_| FunctionValidationError::InvalidInstruction("block"))?;
    let block_type = match block_type {
        -64 => FunctionType::default(),
        -1 => FunctionType {
            results: vec![ValueType::I32],
            ..FunctionType::default()
        },
        -2 => FunctionType {
            results: vec![ValueType::I64],
            ..FunctionType::default()
        },
        -3 => FunctionType {
            results: vec![ValueType::F32],
            ..FunctionType::default()
        },
        -4 => FunctionType {
            results: vec![ValueType::F64],
            ..FunctionType::default()
        },
        -5 => FunctionType {
            results: vec![ValueType::V128],
            ..FunctionType::default()
        },
        -16 => FunctionType {
            results: vec![ValueType::FUNCREF],
            ..FunctionType::default()
        },
        -17 => FunctionType {
            results: vec![ValueType::EXTERNREF],
            ..FunctionType::default()
        },
        raw => {
            let IndirectCallValidation::Validate {
                enabled_features,
                function_types,
                ..
            } = indirect_call_validation
            else {
                return Err(FunctionValidationError::InvalidInstruction("block"));
            };
            enabled_features
                .require_enabled(CoreFeatures::MULTI_VALUE)
                .map_err(|_| FunctionValidationError::InvalidInstruction("block"))?;
            function_types
                .get(raw as usize)
                .cloned()
                .ok_or(FunctionValidationError::InvalidInstruction("block"))?
        }
    };
    Ok((block_type, read))
}

fn read_memarg_len(
    bytes: &[u8],
    max_align_exponent: u32,
) -> Result<usize, FunctionValidationError> {
    let (align, align_read) = read_u32(bytes, "memory alignment")?;
    if align >= 32 || align > max_align_exponent {
        return Err(FunctionValidationError::InvalidMemoryAlignment);
    }
    let offset_read = read_u32_len(&bytes[align_read..], "memory offset")?;
    Ok(align_read + offset_read)
}

fn read_misc_len(bytes: &[u8]) -> Result<(usize, bool), FunctionValidationError> {
    let (misc_opcode, read) = read_u32(bytes, "misc opcode")?;
    let (extra, uses_memory) = match misc_opcode as u8 {
        OPCODE_MISC_I32_TRUNC_SAT_F32_S
        | OPCODE_MISC_I32_TRUNC_SAT_F32_U
        | OPCODE_MISC_I32_TRUNC_SAT_F64_S
        | OPCODE_MISC_I32_TRUNC_SAT_F64_U
        | OPCODE_MISC_I64_TRUNC_SAT_F32_S
        | OPCODE_MISC_I64_TRUNC_SAT_F32_U
        | OPCODE_MISC_I64_TRUNC_SAT_F64_S
        | OPCODE_MISC_I64_TRUNC_SAT_F64_U => (0, false),
        OPCODE_MISC_MEMORY_INIT => {
            let data_len = read_u32_len(&bytes[read..], "memory.init data index")?;
            (
                data_len + read_zero_byte(&bytes[read + data_len..], "memory.init memory index")?,
                true,
            )
        }
        OPCODE_MISC_DATA_DROP => (read_u32_len(&bytes[read..], "data.drop data index")?, false),
        OPCODE_MISC_MEMORY_COPY => {
            let dst_len = read_zero_byte(&bytes[read..], "memory.copy destination memory index")?;
            (
                dst_len
                    + read_zero_byte(&bytes[read + dst_len..], "memory.copy source memory index")?,
                true,
            )
        }
        OPCODE_MISC_MEMORY_FILL => (
            read_zero_byte(&bytes[read..], "memory.fill memory index")?,
            true,
        ),
        OPCODE_MISC_TABLE_INIT => {
            let elem_len = read_u32_len(&bytes[read..], "table.init element index")?;
            (
                elem_len + read_u32_len(&bytes[read + elem_len..], "table.init table index")?,
                false,
            )
        }
        OPCODE_MISC_ELEM_DROP => (
            read_u32_len(&bytes[read..], "elem.drop element index")?,
            false,
        ),
        OPCODE_MISC_TABLE_COPY => {
            let dst_len = read_u32_len(&bytes[read..], "table.copy destination table index")?;
            (
                dst_len + read_u32_len(&bytes[read + dst_len..], "table.copy source table index")?,
                false,
            )
        }
        OPCODE_MISC_TABLE_GROW | OPCODE_MISC_TABLE_SIZE | OPCODE_MISC_TABLE_FILL => {
            (read_u32_len(&bytes[read..], "table index")?, false)
        }
        _ => (0, false),
    };
    Ok((read + extra, uses_memory))
}

fn read_atomic_len(bytes: &[u8]) -> Result<(usize, bool), FunctionValidationError> {
    let (atomic_opcode, read) = read_u32(bytes, "atomic opcode")?;
    let (extra, uses_memory) = match atomic_opcode as u8 {
        OPCODE_ATOMIC_MEMORY_NOTIFY | OPCODE_ATOMIC_MEMORY_WAIT32 => {
            (read_memarg_len(&bytes[read..], 2)?, true)
        }
        OPCODE_ATOMIC_MEMORY_WAIT64 => (read_memarg_len(&bytes[read..], 3)?, true),
        OPCODE_ATOMIC_FENCE => (read_zero_byte(&bytes[read..], "atomic.fence")?, false),
        OPCODE_ATOMIC_I32_LOAD
        | OPCODE_ATOMIC_I32_STORE
        | OPCODE_ATOMIC_I32_RMW_ADD
        | OPCODE_ATOMIC_I32_RMW_SUB
        | OPCODE_ATOMIC_I32_RMW_AND
        | OPCODE_ATOMIC_I32_RMW_OR
        | OPCODE_ATOMIC_I32_RMW_XOR
        | OPCODE_ATOMIC_I32_RMW_XCHG
        | OPCODE_ATOMIC_I32_RMW_CMPXCHG => (read_memarg_len(&bytes[read..], 2)?, true),
        OPCODE_ATOMIC_I64_LOAD
        | OPCODE_ATOMIC_I64_STORE
        | OPCODE_ATOMIC_I64_RMW_ADD
        | OPCODE_ATOMIC_I64_RMW_SUB
        | OPCODE_ATOMIC_I64_RMW_AND
        | OPCODE_ATOMIC_I64_RMW_OR
        | OPCODE_ATOMIC_I64_RMW_XOR
        | OPCODE_ATOMIC_I64_RMW_XCHG
        | OPCODE_ATOMIC_I64_RMW_CMPXCHG => (read_memarg_len(&bytes[read..], 3)?, true),
        OPCODE_ATOMIC_I32_LOAD8_U
        | OPCODE_ATOMIC_I32_STORE8
        | OPCODE_ATOMIC_I32_RMW8_ADD_U
        | OPCODE_ATOMIC_I32_RMW8_SUB_U
        | OPCODE_ATOMIC_I32_RMW8_AND_U
        | OPCODE_ATOMIC_I32_RMW8_OR_U
        | OPCODE_ATOMIC_I32_RMW8_XOR_U
        | OPCODE_ATOMIC_I32_RMW8_XCHG_U
        | OPCODE_ATOMIC_I32_RMW8_CMPXCHG_U
        | OPCODE_ATOMIC_I64_LOAD8_U
        | OPCODE_ATOMIC_I64_STORE8
        | OPCODE_ATOMIC_I64_RMW8_ADD_U
        | OPCODE_ATOMIC_I64_RMW8_SUB_U
        | OPCODE_ATOMIC_I64_RMW8_AND_U
        | OPCODE_ATOMIC_I64_RMW8_OR_U
        | OPCODE_ATOMIC_I64_RMW8_XOR_U
        | OPCODE_ATOMIC_I64_RMW8_XCHG_U
        | OPCODE_ATOMIC_I64_RMW8_CMPXCHG_U => (read_memarg_len(&bytes[read..], 0)?, true),
        OPCODE_ATOMIC_I32_LOAD16_U
        | OPCODE_ATOMIC_I32_STORE16
        | OPCODE_ATOMIC_I32_RMW16_ADD_U
        | OPCODE_ATOMIC_I32_RMW16_SUB_U
        | OPCODE_ATOMIC_I32_RMW16_AND_U
        | OPCODE_ATOMIC_I32_RMW16_OR_U
        | OPCODE_ATOMIC_I32_RMW16_XOR_U
        | OPCODE_ATOMIC_I32_RMW16_XCHG_U
        | OPCODE_ATOMIC_I32_RMW16_CMPXCHG_U
        | OPCODE_ATOMIC_I64_LOAD16_U
        | OPCODE_ATOMIC_I64_STORE16
        | OPCODE_ATOMIC_I64_RMW16_ADD_U
        | OPCODE_ATOMIC_I64_RMW16_SUB_U
        | OPCODE_ATOMIC_I64_RMW16_AND_U
        | OPCODE_ATOMIC_I64_RMW16_OR_U
        | OPCODE_ATOMIC_I64_RMW16_XOR_U
        | OPCODE_ATOMIC_I64_RMW16_XCHG_U
        | OPCODE_ATOMIC_I64_RMW16_CMPXCHG_U => (read_memarg_len(&bytes[read..], 1)?, true),
        OPCODE_ATOMIC_I64_LOAD32_U
        | OPCODE_ATOMIC_I64_STORE32
        | OPCODE_ATOMIC_I64_RMW32_ADD_U
        | OPCODE_ATOMIC_I64_RMW32_SUB_U
        | OPCODE_ATOMIC_I64_RMW32_AND_U
        | OPCODE_ATOMIC_I64_RMW32_OR_U
        | OPCODE_ATOMIC_I64_RMW32_XOR_U
        | OPCODE_ATOMIC_I64_RMW32_XCHG_U
        | OPCODE_ATOMIC_I64_RMW32_CMPXCHG_U => (read_memarg_len(&bytes[read..], 2)?, true),
        _ => (0, false),
    };
    Ok((read + extra, uses_memory))
}

fn read_memarg(bytes: &[u8]) -> Result<(u32, usize), FunctionValidationError> {
    let (align, align_read) = read_u32(bytes, "memory alignment")?;
    let offset_read = read_u32_len(&bytes[align_read..], "memory offset")?;
    Ok((align, align_read + offset_read))
}

fn read_memarg_align(bytes: &[u8]) -> Result<u32, FunctionValidationError> {
    let (align, _) = read_u32(bytes, "memory alignment")?;
    Ok(align)
}

fn vec_extract_lane_attr(opcode: OpcodeVec) -> Option<(u8, ValueType)> {
    Some(match opcode {
        OPCODE_VEC_I8X16_EXTRACT_LANE_S | OPCODE_VEC_I8X16_EXTRACT_LANE_U => (16, ValueType::I32),
        OPCODE_VEC_I16X8_EXTRACT_LANE_S | OPCODE_VEC_I16X8_EXTRACT_LANE_U => (8, ValueType::I32),
        OPCODE_VEC_I32X4_EXTRACT_LANE => (4, ValueType::I32),
        OPCODE_VEC_I64X2_EXTRACT_LANE => (2, ValueType::I64),
        OPCODE_VEC_F32X4_EXTRACT_LANE => (4, ValueType::F32),
        OPCODE_VEC_F64X2_EXTRACT_LANE => (2, ValueType::F64),
        _ => return None,
    })
}

fn vec_replace_lane_attr(opcode: OpcodeVec) -> Option<(u8, ValueType)> {
    Some(match opcode {
        OPCODE_VEC_I8X16_REPLACE_LANE => (16, ValueType::I32),
        OPCODE_VEC_I16X8_REPLACE_LANE => (8, ValueType::I32),
        OPCODE_VEC_I32X4_REPLACE_LANE => (4, ValueType::I32),
        OPCODE_VEC_I64X2_REPLACE_LANE => (2, ValueType::I64),
        OPCODE_VEC_F32X4_REPLACE_LANE => (4, ValueType::F32),
        OPCODE_VEC_F64X2_REPLACE_LANE => (2, ValueType::F64),
        _ => return None,
    })
}

fn vec_load_lane_attr(opcode: OpcodeVec) -> Option<(u32, u8)> {
    Some(match opcode {
        OPCODE_VEC_V128_LOAD64_LANE => (64 / 8, 128 / 64),
        OPCODE_VEC_V128_LOAD32_LANE => (32 / 8, 128 / 32),
        OPCODE_VEC_V128_LOAD16_LANE => (16 / 8, 128 / 16),
        OPCODE_VEC_V128_LOAD8_LANE => (1, 128 / 8),
        _ => return None,
    })
}

fn vec_store_lane_attr(opcode: OpcodeVec) -> Option<(u32, u8)> {
    Some(match opcode {
        OPCODE_VEC_V128_STORE64_LANE => (64 / 8, 128 / 64),
        OPCODE_VEC_V128_STORE32_LANE => (32 / 8, 128 / 32),
        OPCODE_VEC_V128_STORE16_LANE => (16 / 8, 128 / 16),
        OPCODE_VEC_V128_STORE8_LANE => (1, 128 / 8),
        _ => return None,
    })
}

fn vec_load_max_align(opcode: OpcodeVec) -> Option<u32> {
    Some(match opcode {
        OPCODE_VEC_V128_LOAD => 128 / 8,
        OPCODE_VEC_V128_LOAD8X8S
        | OPCODE_VEC_V128_LOAD8X8U
        | OPCODE_VEC_V128_LOAD16X4S
        | OPCODE_VEC_V128_LOAD16X4U
        | OPCODE_VEC_V128_LOAD32X2S
        | OPCODE_VEC_V128_LOAD32X2U => 64 / 8,
        OPCODE_VEC_V128_LOAD8_SPLAT => 1,
        OPCODE_VEC_V128_LOAD16_SPLAT => 16 / 8,
        OPCODE_VEC_V128_LOAD32_SPLAT | OPCODE_VEC_V128_LOAD32ZERO => 32 / 8,
        OPCODE_VEC_V128_LOAD64_SPLAT | OPCODE_VEC_V128_LOAD64ZERO => 64 / 8,
        _ => return None,
    })
}

fn vec_splat_input_type(opcode: OpcodeVec) -> Option<ValueType> {
    Some(match opcode {
        OPCODE_VEC_I8X16_SPLAT => ValueType::I32,
        OPCODE_VEC_I16X8_SPLAT => ValueType::I32,
        OPCODE_VEC_I32X4_SPLAT => ValueType::I32,
        OPCODE_VEC_I64X2_SPLAT => ValueType::I64,
        OPCODE_VEC_F32X4_SPLAT => ValueType::F32,
        OPCODE_VEC_F64X2_SPLAT => ValueType::F64,
        _ => return None,
    })
}

fn validate_vec_opcode(
    bytes: &[u8],
    enabled_features: CoreFeatures,
    has_memory: bool,
    value_stack: &mut Vec<ValueType>,
    control_stack: &[ControlFrame],
) -> Result<(usize, bool), FunctionValidationError> {
    let Some(&vec_opcode) = bytes.first() else {
        return Err(FunctionValidationError::MissingEnd);
    };
    enabled_features
        .require_enabled(CoreFeatures::SIMD)
        .map_err(|_| {
            FunctionValidationError::InvalidInstruction(vector_instruction_name(vec_opcode))
        })?;

    match vec_opcode {
        OPCODE_VEC_V128_CONST => {
            read_fixed_len(&bytes[1..], 16, vector_instruction_name(vec_opcode))?;
            value_stack.push(ValueType::V128);
            Ok((17, false))
        }
        OPCODE_VEC_V128_ANY_TRUE
        | OPCODE_VEC_I8X16_ALL_TRUE
        | OPCODE_VEC_I16X8_ALL_TRUE
        | OPCODE_VEC_I32X4_ALL_TRUE
        | OPCODE_VEC_I64X2_ALL_TRUE
        | OPCODE_VEC_I8X16_BIT_MASK
        | OPCODE_VEC_I16X8_BIT_MASK
        | OPCODE_VEC_I32X4_BIT_MASK
        | OPCODE_VEC_I64X2_BIT_MASK => {
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            value_stack.push(ValueType::I32);
            Ok((1, false))
        }
        OPCODE_VEC_V128_LOAD
        | OPCODE_VEC_V128_LOAD8X8S
        | OPCODE_VEC_V128_LOAD8X8U
        | OPCODE_VEC_V128_LOAD16X4S
        | OPCODE_VEC_V128_LOAD16X4U
        | OPCODE_VEC_V128_LOAD32X2S
        | OPCODE_VEC_V128_LOAD32X2U
        | OPCODE_VEC_V128_LOAD8_SPLAT
        | OPCODE_VEC_V128_LOAD16_SPLAT
        | OPCODE_VEC_V128_LOAD32_SPLAT
        | OPCODE_VEC_V128_LOAD64_SPLAT
        | OPCODE_VEC_V128_LOAD32ZERO
        | OPCODE_VEC_V128_LOAD64ZERO => {
            if !has_memory {
                return Err(FunctionValidationError::InvalidInstruction(
                    vector_instruction_name(vec_opcode),
                ));
            }
            let (align, read) = read_memarg(&bytes[1..])?;
            let max_align = vec_load_max_align(vec_opcode).unwrap();
            if align >= 32 || (1_u32 << align) > max_align {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::V128);
            Ok((1 + read, true))
        }
        OPCODE_VEC_V128_STORE => {
            if !has_memory {
                return Err(FunctionValidationError::InvalidInstruction(
                    vector_instruction_name(vec_opcode),
                ));
            }
            let (align, read) = read_memarg(&bytes[1..])?;
            if align >= 32 || (1_u32 << align) > 128 / 8 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            Ok((1 + read, true))
        }
        opcode if vec_load_lane_attr(opcode).is_some() => {
            if !has_memory {
                return Err(FunctionValidationError::InvalidInstruction(
                    vector_instruction_name(vec_opcode),
                ));
            }
            let (align_max, lane_ceil) = vec_load_lane_attr(opcode).unwrap();
            let (align, read) = read_memarg(&bytes[1..])?;
            if align >= 32 || (1_u32 << align) > align_max {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            let lane = *bytes
                .get(1 + read)
                .ok_or(FunctionValidationError::InvalidInstruction(
                    vector_instruction_name(vec_opcode),
                ))?;
            if lane >= lane_ceil {
                return Err(FunctionValidationError::InvalidInstruction(
                    vector_instruction_name(vec_opcode),
                ));
            }
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::V128);
            Ok((1 + read + 1, true))
        }
        opcode if vec_store_lane_attr(opcode).is_some() => {
            if !has_memory {
                return Err(FunctionValidationError::InvalidInstruction(
                    vector_instruction_name(vec_opcode),
                ));
            }
            let (align_max, lane_ceil) = vec_store_lane_attr(opcode).unwrap();
            let (align, read) = read_memarg(&bytes[1..])?;
            if align >= 32 || (1_u32 << align) > align_max {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            let lane = *bytes
                .get(1 + read)
                .ok_or(FunctionValidationError::InvalidInstruction(
                    vector_instruction_name(vec_opcode),
                ))?;
            if lane >= lane_ceil {
                return Err(FunctionValidationError::InvalidInstruction(
                    vector_instruction_name(vec_opcode),
                ));
            }
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            Ok((1 + read + 1, true))
        }
        opcode if vec_extract_lane_attr(opcode).is_some() => {
            let (lane_ceil, result_type) = vec_extract_lane_attr(opcode).unwrap();
            let lane = *bytes
                .get(1)
                .ok_or(FunctionValidationError::InvalidInstruction(
                    vector_instruction_name(vec_opcode),
                ))?;
            if lane >= lane_ceil {
                return Err(FunctionValidationError::InvalidInstruction(
                    vector_instruction_name(vec_opcode),
                ));
            }
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            value_stack.push(result_type);
            Ok((2, false))
        }
        opcode if vec_replace_lane_attr(opcode).is_some() => {
            let (lane_ceil, param_type) = vec_replace_lane_attr(opcode).unwrap();
            let lane = *bytes
                .get(1)
                .ok_or(FunctionValidationError::InvalidInstruction(
                    vector_instruction_name(vec_opcode),
                ))?;
            if lane >= lane_ceil {
                return Err(FunctionValidationError::InvalidInstruction(
                    vector_instruction_name(vec_opcode),
                ));
            }
            pop_expected(value_stack, control_stack, param_type)?;
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            value_stack.push(ValueType::V128);
            Ok((2, false))
        }
        opcode if vec_splat_input_type(opcode).is_some() => {
            pop_expected(
                value_stack,
                control_stack,
                vec_splat_input_type(opcode).unwrap(),
            )?;
            value_stack.push(ValueType::V128);
            Ok((1, false))
        }
        OPCODE_VEC_I8X16_SWIZZLE
        | OPCODE_VEC_V128_AND
        | OPCODE_VEC_V128_OR
        | OPCODE_VEC_V128_XOR
        | OPCODE_VEC_V128_AND_NOT => {
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            value_stack.push(ValueType::V128);
            Ok((1, false))
        }
        OPCODE_VEC_V128_BITSELECT => {
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            value_stack.push(ValueType::V128);
            Ok((1, false))
        }
        OPCODE_VEC_V128_NOT => {
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            value_stack.push(ValueType::V128);
            Ok((1, false))
        }
        OPCODE_VEC_V128I8X16_SHUFFLE => {
            read_fixed_len(&bytes[1..], 16, vector_instruction_name(vec_opcode))?;
            for lane in &bytes[1..17] {
                if *lane >= 32 {
                    return Err(FunctionValidationError::InvalidInstruction(
                        vector_instruction_name(vec_opcode),
                    ));
                }
            }
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            value_stack.push(ValueType::V128);
            Ok((17, false))
        }
        OPCODE_VEC_I8X16_SHL
        | OPCODE_VEC_I8X16_SHR_S
        | OPCODE_VEC_I8X16_SHR_U
        | OPCODE_VEC_I16X8_SHL
        | OPCODE_VEC_I16X8_SHR_S
        | OPCODE_VEC_I16X8_SHR_U
        | OPCODE_VEC_I32X4_SHL
        | OPCODE_VEC_I32X4_SHR_S
        | OPCODE_VEC_I32X4_SHR_U
        | OPCODE_VEC_I64X2_SHL
        | OPCODE_VEC_I64X2_SHR_S
        | OPCODE_VEC_I64X2_SHR_U => {
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            value_stack.push(ValueType::V128);
            Ok((1, false))
        }
        OPCODE_VEC_I8X16_EQ
        | OPCODE_VEC_I8X16_NE
        | OPCODE_VEC_I8X16_LT_S
        | OPCODE_VEC_I8X16_LT_U
        | OPCODE_VEC_I8X16_GT_S
        | OPCODE_VEC_I8X16_GT_U
        | OPCODE_VEC_I8X16_LE_S
        | OPCODE_VEC_I8X16_LE_U
        | OPCODE_VEC_I8X16_GE_S
        | OPCODE_VEC_I8X16_GE_U
        | OPCODE_VEC_I16X8_EQ
        | OPCODE_VEC_I16X8_NE
        | OPCODE_VEC_I16X8_LT_S
        | OPCODE_VEC_I16X8_LT_U
        | OPCODE_VEC_I16X8_GT_S
        | OPCODE_VEC_I16X8_GT_U
        | OPCODE_VEC_I16X8_LE_S
        | OPCODE_VEC_I16X8_LE_U
        | OPCODE_VEC_I16X8_GE_S
        | OPCODE_VEC_I16X8_GE_U
        | OPCODE_VEC_I32X4_EQ
        | OPCODE_VEC_I32X4_NE
        | OPCODE_VEC_I32X4_LT_S
        | OPCODE_VEC_I32X4_LT_U
        | OPCODE_VEC_I32X4_GT_S
        | OPCODE_VEC_I32X4_GT_U
        | OPCODE_VEC_I32X4_LE_S
        | OPCODE_VEC_I32X4_LE_U
        | OPCODE_VEC_I32X4_GE_S
        | OPCODE_VEC_I32X4_GE_U
        | OPCODE_VEC_I64X2_EQ
        | OPCODE_VEC_I64X2_NE
        | OPCODE_VEC_I64X2_LT_S
        | OPCODE_VEC_I64X2_GT_S
        | OPCODE_VEC_I64X2_LE_S
        | OPCODE_VEC_I64X2_GE_S
        | OPCODE_VEC_F32X4_EQ
        | OPCODE_VEC_F32X4_NE
        | OPCODE_VEC_F32X4_LT
        | OPCODE_VEC_F32X4_GT
        | OPCODE_VEC_F32X4_LE
        | OPCODE_VEC_F32X4_GE
        | OPCODE_VEC_F64X2_EQ
        | OPCODE_VEC_F64X2_NE
        | OPCODE_VEC_F64X2_LT
        | OPCODE_VEC_F64X2_GT
        | OPCODE_VEC_F64X2_LE
        | OPCODE_VEC_F64X2_GE
        | OPCODE_VEC_I32X4_DOT_I16X8_S
        | OPCODE_VEC_I8X16_NARROW_I16X8_S
        | OPCODE_VEC_I8X16_NARROW_I16X8_U
        | OPCODE_VEC_I16X8_NARROW_I32X4_S
        | OPCODE_VEC_I16X8_NARROW_I32X4_U
        | OPCODE_VEC_I8X16_ADD
        | OPCODE_VEC_I8X16_ADD_SAT_S
        | OPCODE_VEC_I8X16_ADD_SAT_U
        | OPCODE_VEC_I8X16_SUB
        | OPCODE_VEC_I8X16_SUB_SAT_S
        | OPCODE_VEC_I8X16_SUB_SAT_U
        | OPCODE_VEC_I16X8_ADD
        | OPCODE_VEC_I16X8_ADD_SAT_S
        | OPCODE_VEC_I16X8_ADD_SAT_U
        | OPCODE_VEC_I16X8_SUB
        | OPCODE_VEC_I16X8_SUB_SAT_S
        | OPCODE_VEC_I16X8_SUB_SAT_U
        | OPCODE_VEC_I16X8_MUL
        | OPCODE_VEC_I32X4_ADD
        | OPCODE_VEC_I32X4_SUB
        | OPCODE_VEC_I32X4_MUL
        | OPCODE_VEC_I64X2_ADD
        | OPCODE_VEC_I64X2_SUB
        | OPCODE_VEC_I64X2_MUL
        | OPCODE_VEC_F32X4_ADD
        | OPCODE_VEC_F32X4_SUB
        | OPCODE_VEC_F32X4_MUL
        | OPCODE_VEC_F32X4_DIV
        | OPCODE_VEC_F64X2_ADD
        | OPCODE_VEC_F64X2_SUB
        | OPCODE_VEC_F64X2_MUL
        | OPCODE_VEC_F64X2_DIV
        | OPCODE_VEC_I8X16_MIN_S
        | OPCODE_VEC_I8X16_MIN_U
        | OPCODE_VEC_I8X16_MAX_S
        | OPCODE_VEC_I8X16_MAX_U
        | OPCODE_VEC_I8X16_AVGR_U
        | OPCODE_VEC_I16X8_MIN_S
        | OPCODE_VEC_I16X8_MIN_U
        | OPCODE_VEC_I16X8_MAX_S
        | OPCODE_VEC_I16X8_MAX_U
        | OPCODE_VEC_I16X8_AVGR_U
        | OPCODE_VEC_I32X4_MIN_S
        | OPCODE_VEC_I32X4_MIN_U
        | OPCODE_VEC_I32X4_MAX_S
        | OPCODE_VEC_I32X4_MAX_U
        | OPCODE_VEC_F32X4_MIN
        | OPCODE_VEC_F32X4_MAX
        | OPCODE_VEC_F64X2_MIN
        | OPCODE_VEC_F64X2_MAX
        | OPCODE_VEC_F32X4_PMIN
        | OPCODE_VEC_F32X4_PMAX
        | OPCODE_VEC_F64X2_PMIN
        | OPCODE_VEC_F64X2_PMAX
        | OPCODE_VEC_I16X8_Q15MULR_SAT_S
        | OPCODE_VEC_I16X8_EXT_MUL_LOW_I8X16_S
        | OPCODE_VEC_I16X8_EXT_MUL_HIGH_I8X16_S
        | OPCODE_VEC_I16X8_EXT_MUL_LOW_I8X16_U
        | OPCODE_VEC_I16X8_EXT_MUL_HIGH_I8X16_U
        | OPCODE_VEC_I32X4_EXT_MUL_LOW_I16X8_S
        | OPCODE_VEC_I32X4_EXT_MUL_HIGH_I16X8_S
        | OPCODE_VEC_I32X4_EXT_MUL_LOW_I16X8_U
        | OPCODE_VEC_I32X4_EXT_MUL_HIGH_I16X8_U
        | OPCODE_VEC_I64X2_EXT_MUL_LOW_I32X4_S
        | OPCODE_VEC_I64X2_EXT_MUL_HIGH_I32X4_S
        | OPCODE_VEC_I64X2_EXT_MUL_LOW_I32X4_U
        | OPCODE_VEC_I64X2_EXT_MUL_HIGH_I32X4_U => {
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            value_stack.push(ValueType::V128);
            Ok((1, false))
        }
        OPCODE_VEC_I8X16_NEG
        | OPCODE_VEC_I16X8_NEG
        | OPCODE_VEC_I32X4_NEG
        | OPCODE_VEC_I64X2_NEG
        | OPCODE_VEC_F32X4_NEG
        | OPCODE_VEC_F64X2_NEG
        | OPCODE_VEC_F32X4_SQRT
        | OPCODE_VEC_F64X2_SQRT
        | OPCODE_VEC_I8X16_ABS
        | OPCODE_VEC_I8X16_POPCNT
        | OPCODE_VEC_I16X8_ABS
        | OPCODE_VEC_I32X4_ABS
        | OPCODE_VEC_I64X2_ABS
        | OPCODE_VEC_F32X4_ABS
        | OPCODE_VEC_F64X2_ABS
        | OPCODE_VEC_F32X4_CEIL
        | OPCODE_VEC_F32X4_FLOOR
        | OPCODE_VEC_F32X4_TRUNC
        | OPCODE_VEC_F32X4_NEAREST
        | OPCODE_VEC_F64X2_CEIL
        | OPCODE_VEC_F64X2_FLOOR
        | OPCODE_VEC_F64X2_TRUNC
        | OPCODE_VEC_F64X2_NEAREST
        | OPCODE_VEC_I16X8_EXTEND_LOW_I8X16_S
        | OPCODE_VEC_I16X8_EXTEND_HIGH_I8X16_S
        | OPCODE_VEC_I16X8_EXTEND_LOW_I8X16_U
        | OPCODE_VEC_I16X8_EXTEND_HIGH_I8X16_U
        | OPCODE_VEC_I32X4_EXTEND_LOW_I16X8_S
        | OPCODE_VEC_I32X4_EXTEND_HIGH_I16X8_S
        | OPCODE_VEC_I32X4_EXTEND_LOW_I16X8_U
        | OPCODE_VEC_I32X4_EXTEND_HIGH_I16X8_U
        | OPCODE_VEC_I64X2_EXTEND_LOW_I32X4_S
        | OPCODE_VEC_I64X2_EXTEND_HIGH_I32X4_S
        | OPCODE_VEC_I64X2_EXTEND_LOW_I32X4_U
        | OPCODE_VEC_I64X2_EXTEND_HIGH_I32X4_U
        | OPCODE_VEC_I16X8_EXTADD_PAIRWISE_I8X16_S
        | OPCODE_VEC_I16X8_EXTADD_PAIRWISE_I8X16_U
        | OPCODE_VEC_I32X4_EXTADD_PAIRWISE_I16X8_S
        | OPCODE_VEC_I32X4_EXTADD_PAIRWISE_I16X8_U
        | OPCODE_VEC_F64X2_PROMOTE_LOW_F32X4_ZERO
        | OPCODE_VEC_F32X4_DEMOTE_F64X2_ZERO
        | OPCODE_VEC_F32X4_CONVERT_I32X4_S
        | OPCODE_VEC_F32X4_CONVERT_I32X4_U
        | OPCODE_VEC_F64X2_CONVERT_LOW_I32X4_S
        | OPCODE_VEC_F64X2_CONVERT_LOW_I32X4_U
        | OPCODE_VEC_I32X4_TRUNC_SAT_F32X4_S
        | OPCODE_VEC_I32X4_TRUNC_SAT_F32X4_U
        | OPCODE_VEC_I32X4_TRUNC_SAT_F64X2_S_ZERO
        | OPCODE_VEC_I32X4_TRUNC_SAT_F64X2_U_ZERO => {
            pop_expected(value_stack, control_stack, ValueType::V128)?;
            value_stack.push(ValueType::V128);
            Ok((1, false))
        }
        _ => Err(FunctionValidationError::InvalidInstruction(
            vector_instruction_name(vec_opcode),
        )),
    }
}

fn ref_type_to_value_type(ref_type: RefType) -> Result<ValueType, FunctionValidationError> {
    match ref_type {
        RefType::FUNCREF => Ok(ValueType::FUNCREF),
        RefType::EXTERNREF => Ok(ValueType::EXTERNREF),
        _ => Err(FunctionValidationError::TypeMismatch),
    }
}

fn validate_misc_opcode(
    bytes: &[u8],
    indirect_call_validation: IndirectCallValidation<'_>,
    value_stack: &mut Vec<ValueType>,
    control_stack: &[ControlFrame],
) -> Result<(), FunctionValidationError> {
    let (misc_opcode, read) = read_u32(bytes, "misc opcode")?;
    let IndirectCallValidation::Validate {
        enabled_features,
        has_memory,
        data_count,
        tables,
        element_types,
        ..
    } = indirect_call_validation
    else {
        return Err(FunctionValidationError::InvalidInstruction("misc opcode"));
    };
    match misc_opcode as u8 {
        OPCODE_MISC_I32_TRUNC_SAT_F32_S
        | OPCODE_MISC_I32_TRUNC_SAT_F32_U
        | OPCODE_MISC_I32_TRUNC_SAT_F64_S
        | OPCODE_MISC_I32_TRUNC_SAT_F64_U
        | OPCODE_MISC_I64_TRUNC_SAT_F32_S
        | OPCODE_MISC_I64_TRUNC_SAT_F32_U
        | OPCODE_MISC_I64_TRUNC_SAT_F64_S
        | OPCODE_MISC_I64_TRUNC_SAT_F64_U => {
            enabled_features
                .require_enabled(CoreFeatures::NON_TRAPPING_FLOAT_TO_INT_CONVERSION)
                .map_err(|_| FunctionValidationError::InvalidInstruction("trunc_sat"))?;
            let (input, output) = match misc_opcode as u8 {
                OPCODE_MISC_I32_TRUNC_SAT_F32_S | OPCODE_MISC_I32_TRUNC_SAT_F32_U => {
                    (ValueType::F32, ValueType::I32)
                }
                OPCODE_MISC_I32_TRUNC_SAT_F64_S | OPCODE_MISC_I32_TRUNC_SAT_F64_U => {
                    (ValueType::F64, ValueType::I32)
                }
                OPCODE_MISC_I64_TRUNC_SAT_F32_S | OPCODE_MISC_I64_TRUNC_SAT_F32_U => {
                    (ValueType::F32, ValueType::I64)
                }
                _ => (ValueType::F64, ValueType::I64),
            };
            pop_expected(value_stack, control_stack, input)?;
            value_stack.push(output);
        }
        OPCODE_MISC_MEMORY_INIT => {
            enabled_features
                .require_enabled(CoreFeatures::BULK_MEMORY_OPERATIONS)
                .map_err(|_| FunctionValidationError::InvalidInstruction("memory.init"))?;
            let (data_index, _) = read_u32(&bytes[read..], "memory.init data index")
                .map_err(|_| FunctionValidationError::InvalidInstruction("memory.init"))?;
            let Some(data_count) = data_count else {
                return Err(FunctionValidationError::InvalidInstruction("memory.init"));
            };
            if !has_memory || data_index >= data_count {
                return Err(FunctionValidationError::InvalidInstruction("memory.init"));
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
        }
        OPCODE_MISC_DATA_DROP => {
            enabled_features
                .require_enabled(CoreFeatures::BULK_MEMORY_OPERATIONS)
                .map_err(|_| FunctionValidationError::InvalidInstruction("data.drop"))?;
            let (data_index, _) = read_u32(&bytes[read..], "data.drop data index")
                .map_err(|_| FunctionValidationError::InvalidInstruction("data.drop"))?;
            let Some(data_count) = data_count else {
                return Err(FunctionValidationError::InvalidInstruction("data.drop"));
            };
            if data_index >= data_count {
                return Err(FunctionValidationError::InvalidInstruction("data.drop"));
            }
        }
        OPCODE_MISC_MEMORY_COPY | OPCODE_MISC_MEMORY_FILL => {
            let opcode_name = if misc_opcode as u8 == OPCODE_MISC_MEMORY_COPY {
                "memory.copy"
            } else {
                "memory.fill"
            };
            enabled_features
                .require_enabled(CoreFeatures::BULK_MEMORY_OPERATIONS)
                .map_err(|_| FunctionValidationError::InvalidInstruction(opcode_name))?;
            if !has_memory {
                return Err(FunctionValidationError::InvalidInstruction(opcode_name));
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
        }
        OPCODE_MISC_TABLE_INIT | OPCODE_MISC_TABLE_COPY => {
            enabled_features
                .require_enabled(CoreFeatures::BULK_MEMORY_OPERATIONS)
                .map_err(|_| FunctionValidationError::InvalidInstruction("table.init"))?;
            if misc_opcode as u8 == OPCODE_MISC_TABLE_INIT {
                let (element_index, element_len) =
                    read_u32(&bytes[read..], "table.init element index")?;
                let (table_index, _) =
                    read_u32(&bytes[read + element_len..], "table.init table index")?;
                let table = tables
                    .get(table_index as usize)
                    .ok_or(FunctionValidationError::UnknownTableIndex(table_index))?;
                let element_type = element_types
                    .get(element_index as usize)
                    .ok_or(FunctionValidationError::InvalidInstruction("table.init"))?;
                if *element_type != table.ty {
                    return Err(FunctionValidationError::TypeMismatch);
                }
            } else {
                let (dst_table_index, dst_len) =
                    read_u32(&bytes[read..], "table.copy destination table index")?;
                let dst_table = tables
                    .get(dst_table_index as usize)
                    .ok_or(FunctionValidationError::UnknownTableIndex(dst_table_index))?;
                let (src_table_index, _) =
                    read_u32(&bytes[read + dst_len..], "table.copy source table index")?;
                let src_table = tables
                    .get(src_table_index as usize)
                    .ok_or(FunctionValidationError::UnknownTableIndex(src_table_index))?;
                if src_table.ty != dst_table.ty {
                    return Err(FunctionValidationError::TypeMismatch);
                }
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
        }
        OPCODE_MISC_ELEM_DROP => {
            enabled_features
                .require_enabled(CoreFeatures::BULK_MEMORY_OPERATIONS)
                .map_err(|_| FunctionValidationError::InvalidInstruction("elem.drop"))?;
            let (element_index, _) = read_u32(&bytes[read..], "elem.drop element index")
                .map_err(|_| FunctionValidationError::InvalidInstruction("elem.drop"))?;
            if element_types.get(element_index as usize).is_none() {
                return Err(FunctionValidationError::InvalidInstruction("elem.drop"));
            }
        }
        OPCODE_MISC_TABLE_GROW | OPCODE_MISC_TABLE_SIZE | OPCODE_MISC_TABLE_FILL => {
            enabled_features
                .require_enabled(CoreFeatures::REFERENCE_TYPES)
                .map_err(|_| FunctionValidationError::InvalidInstruction("table instruction"))?;
            let (table_index, _) = read_u32(&bytes[read..], "table index")?;
            let table = tables
                .get(table_index as usize)
                .ok_or(FunctionValidationError::UnknownTableIndex(table_index))?;
            let ref_type = match table.ty {
                RefType::FUNCREF => ValueType::FUNCREF,
                RefType::EXTERNREF => ValueType::EXTERNREF,
                _ => return Err(FunctionValidationError::TypeMismatch),
            };
            match misc_opcode as u8 {
                OPCODE_MISC_TABLE_GROW => {
                    pop_expected(value_stack, control_stack, ValueType::I32)?;
                    pop_expected(value_stack, control_stack, ref_type)?;
                    value_stack.push(ValueType::I32);
                }
                OPCODE_MISC_TABLE_SIZE => value_stack.push(ValueType::I32),
                OPCODE_MISC_TABLE_FILL => {
                    pop_expected(value_stack, control_stack, ValueType::I32)?;
                    pop_expected(value_stack, control_stack, ref_type)?;
                    pop_expected(value_stack, control_stack, ValueType::I32)?;
                }
                _ => unreachable!(),
            }
        }
        _ => {
            let _ = enabled_features;
        }
    }
    Ok(())
}

fn validate_atomic_opcode(
    bytes: &[u8],
    indirect_call_validation: IndirectCallValidation<'_>,
    value_stack: &mut Vec<ValueType>,
    control_stack: &[ControlFrame],
) -> Result<(), FunctionValidationError> {
    let (atomic_opcode, read) = read_u32(bytes, "atomic opcode")?;
    let IndirectCallValidation::Validate {
        enabled_features,
        has_memory,
        ..
    } = indirect_call_validation
    else {
        return Err(FunctionValidationError::InvalidInstruction("atomic opcode"));
    };
    let atomic_opcode = atomic_opcode as u8;
    enabled_features
        .require_enabled(CoreFeatures::THREADS)
        .map_err(|_| FunctionValidationError::InvalidInstruction("atomic opcode"))?;
    if atomic_opcode == OPCODE_ATOMIC_FENCE {
        return Ok(());
    }
    if !has_memory {
        return Err(FunctionValidationError::InvalidInstruction("atomic memory"));
    }
    let align = read_memarg_align(&bytes[read..])?;
    match atomic_opcode {
        OPCODE_ATOMIC_MEMORY_NOTIFY => {
            if (1u32 << align) > 4 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I32);
        }
        OPCODE_ATOMIC_MEMORY_WAIT32 => {
            if (1u32 << align) > 4 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I64)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I32);
        }
        OPCODE_ATOMIC_MEMORY_WAIT64 => {
            if (1u32 << align) > 8 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I64)?;
            pop_expected(value_stack, control_stack, ValueType::I64)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I32);
        }
        OPCODE_ATOMIC_I32_LOAD => {
            if (1u32 << align) > 4 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I32);
        }
        OPCODE_ATOMIC_I64_LOAD => {
            if (1u32 << align) > 8 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I64);
        }
        OPCODE_ATOMIC_I32_LOAD8_U | OPCODE_ATOMIC_I64_LOAD8_U => {
            if (1u32 << align) != 1 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(if atomic_opcode == OPCODE_ATOMIC_I32_LOAD8_U {
                ValueType::I32
            } else {
                ValueType::I64
            });
        }
        OPCODE_ATOMIC_I32_LOAD16_U => {
            if (1u32 << align) != 2 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I32);
        }
        OPCODE_ATOMIC_I64_LOAD16_U => {
            if (1u32 << align) > 2 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I64);
        }
        OPCODE_ATOMIC_I64_LOAD32_U => {
            if (1u32 << align) > 4 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I64);
        }
        OPCODE_ATOMIC_I32_STORE | OPCODE_ATOMIC_I32_STORE8 | OPCODE_ATOMIC_I32_STORE16 => {
            let max_align = match atomic_opcode {
                OPCODE_ATOMIC_I32_STORE => 4,
                OPCODE_ATOMIC_I32_STORE8 => 1,
                _ => 2,
            };
            if (1u32 << align) > max_align {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
        }
        OPCODE_ATOMIC_I64_STORE
        | OPCODE_ATOMIC_I64_STORE8
        | OPCODE_ATOMIC_I64_STORE16
        | OPCODE_ATOMIC_I64_STORE32 => {
            let max_align = match atomic_opcode {
                OPCODE_ATOMIC_I64_STORE => 8,
                OPCODE_ATOMIC_I64_STORE8 => 1,
                OPCODE_ATOMIC_I64_STORE16 => 2,
                _ => 4,
            };
            if (1u32 << align) > max_align {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I64)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
        }
        OPCODE_ATOMIC_I32_RMW_ADD
        | OPCODE_ATOMIC_I32_RMW_SUB
        | OPCODE_ATOMIC_I32_RMW_AND
        | OPCODE_ATOMIC_I32_RMW_OR
        | OPCODE_ATOMIC_I32_RMW_XOR
        | OPCODE_ATOMIC_I32_RMW_XCHG => {
            if (1u32 << align) > 4 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I32);
        }
        OPCODE_ATOMIC_I32_RMW8_ADD_U
        | OPCODE_ATOMIC_I32_RMW8_SUB_U
        | OPCODE_ATOMIC_I32_RMW8_AND_U
        | OPCODE_ATOMIC_I32_RMW8_OR_U
        | OPCODE_ATOMIC_I32_RMW8_XOR_U
        | OPCODE_ATOMIC_I32_RMW8_XCHG_U => {
            if (1u32 << align) > 1 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I32);
        }
        OPCODE_ATOMIC_I32_RMW16_ADD_U
        | OPCODE_ATOMIC_I32_RMW16_SUB_U
        | OPCODE_ATOMIC_I32_RMW16_AND_U
        | OPCODE_ATOMIC_I32_RMW16_OR_U
        | OPCODE_ATOMIC_I32_RMW16_XOR_U
        | OPCODE_ATOMIC_I32_RMW16_XCHG_U => {
            if (1u32 << align) > 2 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I32);
        }
        OPCODE_ATOMIC_I64_RMW_ADD
        | OPCODE_ATOMIC_I64_RMW_SUB
        | OPCODE_ATOMIC_I64_RMW_AND
        | OPCODE_ATOMIC_I64_RMW_OR
        | OPCODE_ATOMIC_I64_RMW_XOR
        | OPCODE_ATOMIC_I64_RMW_XCHG => {
            if (1u32 << align) > 8 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I64)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I64);
        }
        OPCODE_ATOMIC_I64_RMW8_ADD_U
        | OPCODE_ATOMIC_I64_RMW8_SUB_U
        | OPCODE_ATOMIC_I64_RMW8_AND_U
        | OPCODE_ATOMIC_I64_RMW8_OR_U
        | OPCODE_ATOMIC_I64_RMW8_XOR_U
        | OPCODE_ATOMIC_I64_RMW8_XCHG_U => {
            if (1u32 << align) > 1 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I64)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I64);
        }
        OPCODE_ATOMIC_I64_RMW16_ADD_U
        | OPCODE_ATOMIC_I64_RMW16_SUB_U
        | OPCODE_ATOMIC_I64_RMW16_AND_U
        | OPCODE_ATOMIC_I64_RMW16_OR_U
        | OPCODE_ATOMIC_I64_RMW16_XOR_U
        | OPCODE_ATOMIC_I64_RMW16_XCHG_U => {
            if (1u32 << align) > 2 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I64)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I64);
        }
        OPCODE_ATOMIC_I64_RMW32_ADD_U
        | OPCODE_ATOMIC_I64_RMW32_SUB_U
        | OPCODE_ATOMIC_I64_RMW32_AND_U
        | OPCODE_ATOMIC_I64_RMW32_OR_U
        | OPCODE_ATOMIC_I64_RMW32_XOR_U
        | OPCODE_ATOMIC_I64_RMW32_XCHG_U => {
            if (1u32 << align) > 4 {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I64)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I64);
        }
        OPCODE_ATOMIC_I32_RMW_CMPXCHG
        | OPCODE_ATOMIC_I32_RMW8_CMPXCHG_U
        | OPCODE_ATOMIC_I32_RMW16_CMPXCHG_U => {
            let max_align = match atomic_opcode {
                OPCODE_ATOMIC_I32_RMW_CMPXCHG => 4,
                OPCODE_ATOMIC_I32_RMW8_CMPXCHG_U => 1,
                _ => 2,
            };
            if (1u32 << align) > max_align {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I32);
        }
        OPCODE_ATOMIC_I64_RMW_CMPXCHG
        | OPCODE_ATOMIC_I64_RMW8_CMPXCHG_U
        | OPCODE_ATOMIC_I64_RMW16_CMPXCHG_U
        | OPCODE_ATOMIC_I64_RMW32_CMPXCHG_U => {
            let max_align = match atomic_opcode {
                OPCODE_ATOMIC_I64_RMW_CMPXCHG => 8,
                OPCODE_ATOMIC_I64_RMW8_CMPXCHG_U => 1,
                OPCODE_ATOMIC_I64_RMW16_CMPXCHG_U => 2,
                _ => 4,
            };
            if (1u32 << align) > max_align {
                return Err(FunctionValidationError::InvalidMemoryAlignment);
            }
            pop_expected(value_stack, control_stack, ValueType::I64)?;
            pop_expected(value_stack, control_stack, ValueType::I64)?;
            pop_expected(value_stack, control_stack, ValueType::I32)?;
            value_stack.push(ValueType::I64);
        }
        _ => return Err(FunctionValidationError::InvalidInstruction("atomic opcode")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_wasm_function, FunctionValidationError};
    use crate::instruction::{
        instruction_name, OPCODE_BR, OPCODE_CALL_INDIRECT, OPCODE_DROP, OPCODE_ELSE, OPCODE_END,
        OPCODE_F64_CONST, OPCODE_GLOBAL_SET, OPCODE_I32_CONST, OPCODE_I32_LOAD16_U,
        OPCODE_I32_LOAD8_S, OPCODE_IF, OPCODE_MEMORY_GROW, OPCODE_MEMORY_SIZE, OPCODE_RETURN,
        OPCODE_TAIL_CALL_RETURN_CALL_INDIRECT, OPCODE_UNREACHABLE,
    };
    use crate::module::{Code, CodeBody, FunctionType, GlobalType, RefType, Table, ValueType};
    use razero_features::CoreFeatures;

    #[test]
    fn validates_host_and_wasm_bodies() {
        assert_eq!(
            Err(FunctionValidationError::EmptyBody),
            validate_wasm_function(
                &Code::default(),
                CoreFeatures::empty(),
                &[],
                &[],
                &[],
                &[],
                &FunctionType::default(),
            )
        );
        assert_eq!(
            Ok(()),
            validate_wasm_function(
                &Code {
                    body: vec![0x00, OPCODE_END],
                    ..Code::default()
                },
                CoreFeatures::empty(),
                &[],
                &[],
                &[],
                &[],
                &FunctionType::default(),
            )
        );
        assert_eq!(
            Ok(()),
            validate_wasm_function(
                &Code {
                    body_kind: CodeBody::Host,
                    ..Code::default()
                },
                CoreFeatures::empty(),
                &[],
                &[],
                &[],
                &[],
                &FunctionType::default(),
            )
        );
    }

    #[test]
    fn rejects_invalid_memory_alignment() {
        assert_eq!(
            Err(FunctionValidationError::InvalidMemoryAlignment),
            validate_wasm_function(
                &Code {
                    body: vec![
                        OPCODE_I32_CONST,
                        0x00,
                        OPCODE_I32_LOAD8_S,
                        0x01,
                        0x00,
                        OPCODE_DROP,
                        OPCODE_END,
                    ],
                    ..Code::default()
                },
                CoreFeatures::empty(),
                &[],
                &[],
                &[],
                &[],
                &FunctionType::default(),
            )
        );
    }

    #[test]
    fn accepts_natural_or_smaller_memory_alignment() {
        assert_eq!(
            Ok(()),
            validate_wasm_function(
                &Code {
                    body: vec![
                        OPCODE_I32_CONST,
                        0x00,
                        OPCODE_I32_LOAD16_U,
                        0x01,
                        0x00,
                        OPCODE_DROP,
                        OPCODE_END,
                    ],
                    ..Code::default()
                },
                CoreFeatures::empty(),
                &[],
                &[],
                &[],
                &[],
                &FunctionType::default(),
            )
        );
    }

    #[test]
    fn rejects_if_without_else_when_results_are_expected() {
        assert_eq!(
            Err(FunctionValidationError::TypeMismatch),
            validate_wasm_function(
                &Code {
                    body: vec![
                        0x00,
                        OPCODE_I32_CONST,
                        0x01,
                        OPCODE_IF,
                        0x7f,
                        OPCODE_I32_CONST,
                        0x00,
                        OPCODE_END,
                        OPCODE_END,
                    ],
                    ..Code::default()
                },
                CoreFeatures::empty(),
                &[],
                &[],
                &[],
                &[],
                &FunctionType {
                    results: vec![ValueType::I32],
                    ..FunctionType::default()
                },
            )
        );
    }

    #[test]
    fn accepts_if_with_else_when_both_branches_produce_results() {
        assert_eq!(
            Ok(()),
            validate_wasm_function(
                &Code {
                    body: vec![
                        0x00,
                        OPCODE_I32_CONST,
                        0x01,
                        OPCODE_IF,
                        0x7f,
                        OPCODE_I32_CONST,
                        0x00,
                        OPCODE_ELSE,
                        OPCODE_I32_CONST,
                        0x01,
                        OPCODE_END,
                        OPCODE_END,
                    ],
                    ..Code::default()
                },
                CoreFeatures::empty(),
                &[],
                &[],
                &[],
                &[],
                &FunctionType {
                    results: vec![ValueType::I32],
                    ..FunctionType::default()
                },
            )
        );
    }

    #[test]
    fn rejects_unknown_call_indirect_table_index() {
        assert_eq!(
            Err(FunctionValidationError::UnknownTableIndex(1)),
            validate_wasm_function(
                &Code {
                    body: vec![
                        OPCODE_I32_CONST,
                        0x00,
                        OPCODE_CALL_INDIRECT,
                        0x00,
                        0x01,
                        OPCODE_END
                    ],
                    ..Code::default()
                },
                CoreFeatures::V2,
                &[FunctionType::default()],
                &[],
                &[Table {
                    min: 1,
                    max: Some(1),
                    ty: RefType::FUNCREF,
                }],
                &[],
                &FunctionType::default(),
            )
        );
    }

    #[test]
    fn rejects_unknown_return_call_indirect_table_index() {
        assert_eq!(
            Err(FunctionValidationError::UnknownTableIndex(1)),
            validate_wasm_function(
                &Code {
                    body: vec![
                        OPCODE_I32_CONST,
                        0x00,
                        OPCODE_TAIL_CALL_RETURN_CALL_INDIRECT,
                        0x00,
                        0x01,
                        OPCODE_END,
                    ],
                    ..Code::default()
                },
                CoreFeatures::V2 | CoreFeatures::TAIL_CALL,
                &[FunctionType::default()],
                &[],
                &[Table {
                    min: 1,
                    max: Some(1),
                    ty: RefType::FUNCREF,
                }],
                &[],
                &FunctionType::default(),
            )
        );
    }

    #[test]
    fn rejects_unknown_branch_label_in_unreachable_code() {
        assert_eq!(
            Err(FunctionValidationError::InvalidInstruction("branch label")),
            validate_wasm_function(
                &Code {
                    body: vec![OPCODE_UNREACHABLE, OPCODE_BR, 0x01, OPCODE_END],
                    ..Code::default()
                },
                CoreFeatures::empty(),
                &[],
                &[],
                &[],
                &[],
                &FunctionType::default(),
            )
        );
    }

    #[test]
    fn rejects_unconsumed_value_after_unreachable() {
        assert_eq!(
            Err(FunctionValidationError::TypeMismatch),
            validate_wasm_function(
                &Code {
                    body: vec![OPCODE_UNREACHABLE, OPCODE_I32_CONST, 0x00, OPCODE_END],
                    ..Code::default()
                },
                CoreFeatures::empty(),
                &[],
                &[],
                &[],
                &[],
                &FunctionType::default(),
            )
        );
    }

    #[test]
    fn rejects_mismatched_return_type_in_unreachable_code() {
        assert_eq!(
            Err(FunctionValidationError::TypeMismatch),
            validate_wasm_function(
                &Code {
                    body: vec![
                        OPCODE_UNREACHABLE,
                        OPCODE_F64_CONST,
                        0x00,
                        0x00,
                        0x00,
                        0x00,
                        0x00,
                        0x00,
                        0xf0,
                        0x3f,
                        OPCODE_RETURN,
                        OPCODE_END,
                    ],
                    ..Code::default()
                },
                CoreFeatures::empty(),
                &[],
                &[],
                &[],
                &[],
                &FunctionType {
                    results: vec![ValueType::I32],
                    ..FunctionType::default()
                },
            )
        );
    }

    #[test]
    fn rejects_non_zero_memory_reserved_bytes() {
        for opcode in [OPCODE_MEMORY_SIZE, OPCODE_MEMORY_GROW] {
            assert_eq!(
                Err(FunctionValidationError::ZeroByteExpected(instruction_name(
                    opcode
                ))),
                validate_wasm_function(
                    &Code {
                        body: vec![opcode, 0x01, OPCODE_END],
                        ..Code::default()
                    },
                    CoreFeatures::empty(),
                    &[],
                    &[],
                    &[],
                    &[],
                    &FunctionType::default(),
                )
            );
            assert_eq!(
                Err(FunctionValidationError::ZeroByteExpected(instruction_name(
                    opcode
                ))),
                validate_wasm_function(
                    &Code {
                        body: vec![opcode, 0x80, 0x00, OPCODE_END],
                        ..Code::default()
                    },
                    CoreFeatures::empty(),
                    &[],
                    &[],
                    &[],
                    &[],
                    &FunctionType::default(),
                )
            );
        }
    }

    #[test]
    fn rejects_global_set_on_immutable_global() {
        assert_eq!(
            Err(FunctionValidationError::ImmutableGlobal(0)),
            validate_wasm_function(
                &Code {
                    body: vec![OPCODE_I32_CONST, 0x00, OPCODE_GLOBAL_SET, 0x00, OPCODE_END],
                    ..Code::default()
                },
                CoreFeatures::empty(),
                &[],
                &[],
                &[],
                &[GlobalType {
                    val_type: ValueType::I32,
                    mutable: false,
                }],
                &FunctionType::default(),
            )
        );
    }

    #[test]
    fn rejects_nested_body_missing_function_end() {
        assert_eq!(
            Err(FunctionValidationError::MissingEnd),
            validate_wasm_function(
                &Code {
                    body: vec![0x02, 0x40, OPCODE_END],
                    ..Code::default()
                },
                CoreFeatures::empty(),
                &[],
                &[],
                &[],
                &[],
                &FunctionType::default(),
            )
        );
    }
}
