#![doc = "Constant expression encoding and evaluation."]

use std::error::Error;
use std::fmt::{Display, Formatter};

use crate::instruction::{
    OPCODE_END, OPCODE_F32_CONST, OPCODE_F64_CONST, OPCODE_GLOBAL_GET, OPCODE_I32_ADD,
    OPCODE_I32_CONST, OPCODE_I32_MUL, OPCODE_I32_SUB, OPCODE_I64_ADD, OPCODE_I64_CONST,
    OPCODE_I64_MUL, OPCODE_I64_SUB, OPCODE_REF_FUNC, OPCODE_REF_NULL, OPCODE_VEC_PREFIX,
    OPCODE_VEC_V128_CONST,
};
use crate::leb128;
use crate::module::{Index, ValueType};
use crate::table::Reference;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConstExpr {
    pub data: Vec<u8>,
}

impl ConstExpr {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    pub fn from_opcode(opcode: u8, op_data: &[u8]) -> Self {
        let mut data = Vec::with_capacity(op_data.len() + 3);
        if opcode == OPCODE_VEC_V128_CONST {
            data.push(OPCODE_VEC_PREFIX);
        }
        data.push(opcode);
        data.extend_from_slice(op_data);
        data.push(OPCODE_END);
        Self { data }
    }

    pub fn from_i32(value: i32) -> Self {
        Self::from_opcode(OPCODE_I32_CONST, &leb128::encode_i32(value))
    }

    pub fn from_i64(value: i64) -> Self {
        Self::from_opcode(OPCODE_I64_CONST, &leb128::encode_i64(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstExprError {
    message: String,
}

impl ConstExprError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ConstExprError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ConstExprError {}

pub fn evaluate_const_expr<GR, FR>(
    expr: &ConstExpr,
    mut global_resolver: GR,
    mut func_ref_resolver: FR,
) -> Result<(Vec<u64>, ValueType), ConstExprError>
where
    GR: FnMut(Index) -> Result<(ValueType, u64, u64), ConstExprError>,
    FR: FnMut(Index) -> Result<Reference, ConstExprError>,
{
    let mut stack = Vec::new();
    let mut type_stack = Vec::new();
    let data = &expr.data;
    let mut pc = 0usize;

    loop {
        let Some(&opcode) = data.get(pc) else {
            return Err(ConstExprError::new("unexpected end of constant expression"));
        };
        pc += 1;

        match opcode {
            OPCODE_I32_CONST => {
                let (value, read) = leb128::load_i32(&data[pc..])
                    .map_err(|err| ConstExprError::new(format!("read i32: {err}")))?;
                pc += read;
                stack.push(u64::from(value as u32));
                type_stack.push(ValueType::I32);
            }
            OPCODE_I64_CONST => {
                let (value, read) = leb128::load_i64(&data[pc..])
                    .map_err(|err| ConstExprError::new(format!("read i64: {err}")))?;
                pc += read;
                stack.push(value as u64);
                type_stack.push(ValueType::I64);
            }
            OPCODE_F32_CONST => {
                let bytes = data.get(pc..pc + 4).ok_or_else(|| {
                    ConstExprError::new("read f32: unexpected end of constant expression")
                })?;
                pc += 4;
                stack.push(u64::from(u32::from_le_bytes(bytes.try_into().unwrap())));
                type_stack.push(ValueType::F32);
            }
            OPCODE_F64_CONST => {
                let bytes = data.get(pc..pc + 8).ok_or_else(|| {
                    ConstExprError::new("read f64: unexpected end of constant expression")
                })?;
                pc += 8;
                stack.push(u64::from_le_bytes(bytes.try_into().unwrap()));
                type_stack.push(ValueType::F64);
            }
            OPCODE_GLOBAL_GET => {
                let (index, read) = leb128::load_u32(&data[pc..])
                    .map_err(|err| ConstExprError::new(format!("read index of global: {err}")))?;
                pc += read;
                let (value_type, lo, hi) = global_resolver(index)?;
                match value_type {
                    ValueType::V128 => stack.extend([lo, hi]),
                    _ => stack.push(lo),
                }
                type_stack.push(value_type);
            }
            OPCODE_REF_NULL => {
                let Some(&raw_value_type) = data.get(pc) else {
                    return Err(ConstExprError::new(
                        "read reference type for ref.null: unexpected end of constant expression",
                    ));
                };
                pc += 1;

                let value_type = ValueType(raw_value_type);
                if value_type != ValueType::FUNCREF && value_type != ValueType::EXTERNREF {
                    return Err(ConstExprError::new(format!(
                        "invalid type for ref.null: 0x{:x}",
                        raw_value_type
                    )));
                }
                stack.push(0);
                type_stack.push(value_type);
            }
            OPCODE_REF_FUNC => {
                let (index, read) = leb128::load_u32(&data[pc..])
                    .map_err(|err| ConstExprError::new(format!("read i32: {err}")))?;
                pc += read;
                stack.push(func_ref_resolver(index)?.unwrap_or_default() as u64);
                type_stack.push(ValueType::FUNCREF);
            }
            OPCODE_VEC_PREFIX => {
                let Some(&vector_opcode) = data.get(pc) else {
                    return Err(ConstExprError::new(
                        "invalid vector opcode for const expression: 0xfd",
                    ));
                };
                if vector_opcode != OPCODE_VEC_V128_CONST {
                    return Err(ConstExprError::new(format!(
                        "invalid vector opcode for const expression: 0x{:x}",
                        opcode
                    )));
                }
                pc += 1;

                let bytes = data.get(pc..pc + 16).ok_or_else(|| {
                    ConstExprError::new("v128.const needs 16 bytes but was fewer")
                })?;
                let lo = u64::from_le_bytes(bytes[..8].try_into().unwrap());
                let hi = u64::from_le_bytes(bytes[8..].try_into().unwrap());
                pc += 16;

                stack.extend([lo, hi]);
                type_stack.push(ValueType::V128);
            }
            OPCODE_I32_ADD => {
                evaluate_binary_i32(&mut stack, &mut type_stack, "i32.add", |lhs, rhs| {
                    lhs.wrapping_add(rhs)
                })?;
            }
            OPCODE_I32_SUB => {
                evaluate_binary_i32(&mut stack, &mut type_stack, "i32.sub", |lhs, rhs| {
                    lhs.wrapping_sub(rhs)
                })?;
            }
            OPCODE_I32_MUL => {
                evaluate_binary_i32(&mut stack, &mut type_stack, "i32.mul", |lhs, rhs| {
                    lhs.wrapping_mul(rhs)
                })?;
            }
            OPCODE_I64_ADD => {
                evaluate_binary_i64(&mut stack, &mut type_stack, "i64.add", |lhs, rhs| {
                    lhs.wrapping_add(rhs)
                })?;
            }
            OPCODE_I64_SUB => {
                evaluate_binary_i64(&mut stack, &mut type_stack, "i64.sub", |lhs, rhs| {
                    lhs.wrapping_sub(rhs)
                })?;
            }
            OPCODE_I64_MUL => {
                evaluate_binary_i64(&mut stack, &mut type_stack, "i64.mul", |lhs, rhs| {
                    lhs.wrapping_mul(rhs)
                })?;
            }
            OPCODE_END => {
                if type_stack.len() != 1 {
                    return Err(ConstExprError::new(
                        "stack has more than one value at end of constant expression",
                    ));
                }
                return Ok((stack, type_stack[0]));
            }
            _ => {
                return Err(ConstExprError::new(format!(
                    "invalid opcode for const expression: 0x{:x}",
                    opcode
                )))
            }
        }
    }
}

fn evaluate_binary_i32<F>(
    stack: &mut Vec<u64>,
    type_stack: &mut Vec<ValueType>,
    name: &str,
    op: F,
) -> Result<(), ConstExprError>
where
    F: FnOnce(u32, u32) -> u32,
{
    pop_binary_types(type_stack, ValueType::I32, name)?;
    let rhs = stack.pop().unwrap() as u32;
    let lhs = stack.pop().unwrap() as u32;
    stack.push(u64::from(op(lhs, rhs)));
    type_stack.push(ValueType::I32);
    Ok(())
}

fn evaluate_binary_i64<F>(
    stack: &mut Vec<u64>,
    type_stack: &mut Vec<ValueType>,
    name: &str,
    op: F,
) -> Result<(), ConstExprError>
where
    F: FnOnce(u64, u64) -> u64,
{
    pop_binary_types(type_stack, ValueType::I64, name)?;
    let rhs = stack.pop().unwrap();
    let lhs = stack.pop().unwrap();
    stack.push(op(lhs, rhs));
    type_stack.push(ValueType::I64);
    Ok(())
}

fn pop_binary_types(
    type_stack: &mut Vec<ValueType>,
    expected: ValueType,
    op_name: &str,
) -> Result<(), ConstExprError> {
    if type_stack.len() < 2 {
        return Err(ConstExprError::new(format!("stack underflow on {op_name}")));
    }
    let rhs = type_stack.pop().unwrap();
    let lhs = type_stack.pop().unwrap();
    if lhs != expected || rhs != expected {
        return Err(ConstExprError::new(format!(
            "type mismatch on {op_name}: {}, {}",
            lhs.name(),
            rhs.name()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{evaluate_const_expr, ConstExpr, ConstExprError};
    use crate::instruction::{
        OPCODE_END, OPCODE_GLOBAL_GET, OPCODE_I32_ADD, OPCODE_I32_CONST, OPCODE_REF_FUNC,
        OPCODE_REF_NULL, OPCODE_VEC_V128_CONST,
    };
    use crate::leb128;
    use crate::module::ValueType;

    #[test]
    fn constructors_match_go_encoding_shape() {
        assert_eq!(
            vec![OPCODE_I32_CONST, 0x01, 0x0b],
            ConstExpr::from_i32(1).data
        );
        assert_eq!(
            vec![0xfd, OPCODE_VEC_V128_CONST, 1, 2, 0x0b],
            ConstExpr::from_opcode(OPCODE_VEC_V128_CONST, &[1, 2]).data
        );
    }

    #[test]
    fn evaluate_handles_constants_globals_refs_and_arithmetic() {
        let mut data = Vec::new();
        data.push(OPCODE_I32_CONST);
        data.extend_from_slice(&leb128::encode_i32(2));
        data.push(OPCODE_I32_CONST);
        data.extend_from_slice(&leb128::encode_i32(3));
        data.push(OPCODE_I32_ADD);
        data.push(OPCODE_END);

        let (values, value_type) = evaluate_const_expr(
            &ConstExpr::new(data),
            |_| unreachable!(),
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(ValueType::I32, value_type);
        assert_eq!(vec![5], values);

        let (values, value_type) = evaluate_const_expr(
            &ConstExpr::from_opcode(OPCODE_GLOBAL_GET, &[0]),
            |index| {
                assert_eq!(0, index);
                Ok((ValueType::V128, 10, 20))
            },
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(ValueType::V128, value_type);
        assert_eq!(vec![10, 20], values);

        let (values, value_type) = evaluate_const_expr(
            &ConstExpr::from_opcode(OPCODE_REF_FUNC, &[5]),
            |_| unreachable!(),
            |index| Ok(Some(index + 100)),
        )
        .unwrap();
        assert_eq!(ValueType::FUNCREF, value_type);
        assert_eq!(vec![105], values);

        let (values, value_type) = evaluate_const_expr(
            &ConstExpr::from_opcode(OPCODE_REF_NULL, &[ValueType::EXTERNREF.0]),
            |_| unreachable!(),
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(ValueType::EXTERNREF, value_type);
        assert_eq!(vec![0], values);
    }

    #[test]
    fn evaluate_reports_invalid_ref_null_type() {
        let err = evaluate_const_expr(
            &ConstExpr::from_opcode(OPCODE_REF_NULL, &[0xff]),
            |_| unreachable!(),
            |_| unreachable!(),
        )
        .unwrap_err();

        assert_eq!("invalid type for ref.null: 0xff", err.to_string());
    }

    #[test]
    fn evaluate_reports_type_mismatch() {
        let mut data = Vec::new();
        data.push(OPCODE_I32_CONST);
        data.extend_from_slice(&leb128::encode_i32(1));
        data.push(OPCODE_REF_NULL);
        data.push(ValueType::FUNCREF.0);
        data.push(OPCODE_I32_ADD);
        data.push(OPCODE_END);

        let err = evaluate_const_expr(
            &ConstExpr::new(data),
            |_| unreachable!(),
            |_| unreachable!(),
        )
        .unwrap_err();
        assert_eq!("type mismatch on i32.add: i32, funcref", err.to_string());
    }

    #[test]
    fn evaluate_preserves_resolver_errors() {
        let err = evaluate_const_expr(
            &ConstExpr::from_opcode(OPCODE_GLOBAL_GET, &[0]),
            |_| Err(ConstExprError::new("boom")),
            |_| unreachable!(),
        )
        .unwrap_err();

        assert_eq!("boom", err.to_string());
    }
}
