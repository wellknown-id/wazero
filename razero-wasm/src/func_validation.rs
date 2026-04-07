#![doc = "Function-body validation helpers."]

use std::error::Error;
use std::fmt::{Display, Formatter};

use razero_features::CoreFeatures;

use crate::instruction::*;
use crate::leb128;
use crate::module::{Code, RefType, Table};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionValidationError {
    EmptyBody,
    MissingEnd,
    InvalidInstruction(&'static str),
    InvalidMemoryAlignment,
    InvalidTypeIndex {
        opcode_name: &'static str,
        type_index: u32,
    },
    TableIndexRequiresReferenceTypes(u32),
    UnknownTableIndex(u32),
    TableNotFuncref {
        actual: RefType,
        opcode_name: &'static str,
    },
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
            Self::InvalidTypeIndex {
                opcode_name,
                type_index,
            } => {
                write!(f, "invalid type index at {opcode_name}: {type_index}")
            }
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
        }
    }
}

impl Error for FunctionValidationError {}

pub fn validate_wasm_function(
    code: &Code,
    enabled_features: CoreFeatures,
    type_count: u32,
    tables: &[Table],
) -> Result<(), FunctionValidationError> {
    scan_wasm_function(
        code,
        IndirectCallValidation::Validate {
            enabled_features,
            type_count,
            tables,
        },
    )
    .map(|_| ())
}

pub fn wasm_function_uses_memory(code: &Code) -> Result<bool, FunctionValidationError> {
    scan_wasm_function(code, IndirectCallValidation::Ignore)
}

#[derive(Clone, Copy)]
enum IndirectCallValidation<'a> {
    Ignore,
    Validate {
        enabled_features: CoreFeatures,
        type_count: u32,
        tables: &'a [Table],
    },
}

fn scan_wasm_function(
    code: &Code,
    indirect_call_validation: IndirectCallValidation<'_>,
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
    while pc < body.len() {
        let opcode = body[pc];
        pc += 1;
        let (immediate_len, memory_here) = match opcode {
            OPCODE_BLOCK | OPCODE_LOOP | OPCODE_IF => {
                (read_i33_len(&body[pc..], "block")?, false)
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
                    type_count,
                    tables,
                } = indirect_call_validation
                {
                    if type_index >= type_count {
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
            OPCODE_LOCAL_GET | OPCODE_LOCAL_SET | OPCODE_LOCAL_TEE | OPCODE_GLOBAL_GET
            | OPCODE_GLOBAL_SET | OPCODE_TABLE_GET | OPCODE_TABLE_SET | OPCODE_REF_FUNC => {
                (read_u32_len(&body[pc..], instruction_name(opcode))?, false)
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
            OPCODE_MEMORY_SIZE | OPCODE_MEMORY_GROW => (
                read_fixed_len(&body[pc..], 1, instruction_name(opcode))?,
                true,
            ),
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
            OPCODE_VEC_PREFIX => break,
            _ => (0, false),
        };
        uses_memory |= memory_here;
        pc += immediate_len;
    }
    Ok(uses_memory)
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

fn read_i33_len(bytes: &[u8], context: &'static str) -> Result<usize, FunctionValidationError> {
    leb128::decode_i33_as_i64(bytes)
        .map(|(_, read)| read)
        .map_err(|_| FunctionValidationError::InvalidInstruction(context))
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
                data_len + read_u32_len(&bytes[read + data_len..], "memory.init memory index")?,
                true,
            )
        }
        OPCODE_MISC_DATA_DROP => (read_u32_len(&bytes[read..], "data.drop data index")?, false),
        OPCODE_MISC_MEMORY_COPY => {
            let dst_len = read_u32_len(&bytes[read..], "memory.copy destination memory index")?;
            (
                dst_len
                    + read_u32_len(&bytes[read + dst_len..], "memory.copy source memory index")?,
                true,
            )
        }
        OPCODE_MISC_MEMORY_FILL => (
            read_u32_len(&bytes[read..], "memory.fill memory index")?,
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
        OPCODE_ATOMIC_FENCE => (read_fixed_len(&bytes[read..], 1, "atomic.fence")?, false),
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

#[cfg(test)]
mod tests {
    use super::{validate_wasm_function, FunctionValidationError};
    use crate::instruction::{
        OPCODE_CALL_INDIRECT, OPCODE_DROP, OPCODE_END, OPCODE_I32_CONST, OPCODE_I32_LOAD16_U,
        OPCODE_I32_LOAD8_S, OPCODE_TAIL_CALL_RETURN_CALL_INDIRECT,
    };
    use crate::module::{Code, CodeBody, RefType, Table};
    use razero_features::CoreFeatures;

    #[test]
    fn validates_host_and_wasm_bodies() {
        assert_eq!(
            Err(FunctionValidationError::EmptyBody),
            validate_wasm_function(&Code::default(), CoreFeatures::empty(), 0, &[])
        );
        assert_eq!(
            Ok(()),
            validate_wasm_function(&Code {
                body: vec![0x00, OPCODE_END],
                ..Code::default()
            }, CoreFeatures::empty(), 0, &[])
        );
        assert_eq!(
            Ok(()),
            validate_wasm_function(&Code {
                body_kind: CodeBody::Host,
                ..Code::default()
            }, CoreFeatures::empty(), 0, &[])
        );
    }

    #[test]
    fn rejects_invalid_memory_alignment() {
        assert_eq!(
            Err(FunctionValidationError::InvalidMemoryAlignment),
            validate_wasm_function(&Code {
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
            }, CoreFeatures::empty(), 0, &[])
        );
    }

    #[test]
    fn accepts_natural_or_smaller_memory_alignment() {
        assert_eq!(
            Ok(()),
            validate_wasm_function(&Code {
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
            }, CoreFeatures::empty(), 0, &[])
        );
    }

    #[test]
    fn rejects_unknown_call_indirect_table_index() {
        assert_eq!(
            Err(FunctionValidationError::UnknownTableIndex(1)),
            validate_wasm_function(
                &Code {
                    body: vec![OPCODE_I32_CONST, 0x00, OPCODE_CALL_INDIRECT, 0x00, 0x01, OPCODE_END],
                    ..Code::default()
                },
                CoreFeatures::V2,
                1,
                &[Table {
                    min: 1,
                    max: Some(1),
                    ty: RefType::FUNCREF,
                }],
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
                1,
                &[Table {
                    min: 1,
                    max: Some(1),
                    ty: RefType::FUNCREF,
                }],
            )
        );
    }
}
