#![doc = "Constant expression decoding."]

use crate::decoder::Decoder;
use crate::errors::{require_feature, DecodeError, DecodeResult, ERR_INVALID_BYTE};
use razero_features::CoreFeatures;
use razero_wasm::const_expr::ConstExpr;
use razero_wasm::instruction::{
    instruction_name, OPCODE_END, OPCODE_F32_CONST, OPCODE_F64_CONST, OPCODE_GLOBAL_GET,
    OPCODE_I32_ADD, OPCODE_I32_CONST, OPCODE_I32_MUL, OPCODE_I32_SUB, OPCODE_I64_ADD,
    OPCODE_I64_CONST, OPCODE_I64_MUL, OPCODE_I64_SUB, OPCODE_REF_FUNC, OPCODE_REF_NULL,
    OPCODE_VEC_PREFIX, OPCODE_VEC_V128_CONST,
};
use razero_wasm::leb128;
use razero_wasm::module::RefType;

pub fn decode_const_expr(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
) -> DecodeResult<ConstExpr> {
    decode_const_expr_with_extended_const(decoder, enabled_features, false)
}

pub fn decode_const_expr_with_extended_const(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
    extended_const_enabled: bool,
) -> DecodeResult<ConstExpr> {
    let mut data = Vec::new();

    loop {
        let opcode = decoder.read_byte().map_err(|err| {
            DecodeError::new(format!("read const expression opcode: {}", err.message))
        })?;
        data.push(opcode);

        match opcode {
            OPCODE_I32_CONST => {
                data.extend(read_var_i32_raw(decoder, "read value")?.1);
            }
            OPCODE_I64_CONST => {
                data.extend(read_var_i64_raw(decoder, "read value")?.1);
            }
            OPCODE_I32_ADD | OPCODE_I32_SUB | OPCODE_I32_MUL | OPCODE_I64_ADD | OPCODE_I64_SUB
            | OPCODE_I64_MUL => {
                if !extended_const_enabled {
                    return Err(DecodeError::new(format!(
                        "{} is not supported in a constant expression as feature \"extended-const\" is disabled",
                        instruction_name(opcode)
                    )));
                }
            }
            OPCODE_F32_CONST => {
                let bytes = decoder.read_bytes(4).map_err(|err| {
                    DecodeError::new(format!("read f32 constant: {}", err.message))
                })?;
                data.extend_from_slice(bytes);
            }
            OPCODE_F64_CONST => {
                let bytes = decoder.read_bytes(8).map_err(|err| {
                    DecodeError::new(format!("read f64 constant: {}", err.message))
                })?;
                data.extend_from_slice(bytes);
            }
            OPCODE_GLOBAL_GET => {
                data.extend(read_var_u32_raw(decoder, "read value")?.1);
            }
            OPCODE_REF_NULL => {
                require_feature(
                    enabled_features,
                    CoreFeatures::BULK_MEMORY,
                    "bulk-memory-operations",
                )
                .map_err(|err| {
                    DecodeError::new(format!("ref.null is not supported as {}", err.message))
                })?;
                let ref_type = decoder.read_byte().map_err(|err| {
                    DecodeError::new(format!("read reference type for ref.null: {}", err.message))
                })?;
                if ref_type != RefType::FUNCREF.0 && ref_type != RefType::EXTERNREF.0 {
                    return Err(DecodeError::new(format!(
                        "invalid type for ref.null: 0x{ref_type:x}"
                    )));
                }
                data.push(ref_type);
            }
            OPCODE_REF_FUNC => {
                require_feature(
                    enabled_features,
                    CoreFeatures::BULK_MEMORY,
                    "bulk-memory-operations",
                )
                .map_err(|err| {
                    DecodeError::new(format!("ref.func is not supported as {}", err.message))
                })?;
                data.extend(read_var_u32_raw(decoder, "read value")?.1);
            }
            OPCODE_VEC_PREFIX => {
                require_feature(enabled_features, CoreFeatures::SIMD, "simd").map_err(|err| {
                    DecodeError::new(format!(
                        "vector instructions are not supported as {}",
                        err.message
                    ))
                })?;
                let suffix = decoder.read_byte().map_err(|err| {
                    DecodeError::new(format!(
                        "read vector instruction opcode suffix: {}",
                        err.message
                    ))
                })?;
                data.push(suffix);

                if suffix != OPCODE_VEC_V128_CONST {
                    return Err(DecodeError::new(format!(
                        "invalid vector opcode for const expression: 0x{suffix:x}"
                    )));
                }

                let remaining = decoder.remaining();
                if remaining < 16 {
                    return Err(DecodeError::new(format!(
                        "read vector const instruction immediates: needs 16 bytes but was {remaining} bytes"
                    )));
                }
                data.extend_from_slice(decoder.read_bytes(16).unwrap());
            }
            OPCODE_END => return Ok(ConstExpr::new(data)),
            _ => {
                return Err(DecodeError::new(format!(
                    "{ERR_INVALID_BYTE} for const expression op code: 0x{opcode:x}"
                )));
            }
        }
    }
}

pub(crate) fn read_var_u32_raw(
    decoder: &mut Decoder<'_>,
    context: &str,
) -> DecodeResult<(u32, Vec<u8>)> {
    read_leb128_raw(decoder, context, leb128::decode_u32)
}

pub(crate) fn read_var_i32_raw(
    decoder: &mut Decoder<'_>,
    context: &str,
) -> DecodeResult<(i32, Vec<u8>)> {
    read_leb128_raw(decoder, context, leb128::decode_i32)
}

pub(crate) fn read_var_i64_raw(
    decoder: &mut Decoder<'_>,
    context: &str,
) -> DecodeResult<(i64, Vec<u8>)> {
    read_leb128_raw(decoder, context, leb128::decode_i64)
}

fn read_leb128_raw<T, F>(
    decoder: &mut Decoder<'_>,
    context: &str,
    decode: F,
) -> DecodeResult<(T, Vec<u8>)>
where
    F: Fn(&[u8]) -> Result<(T, usize), leb128::Leb128Error>,
{
    let mut raw = Vec::with_capacity(5);
    loop {
        let byte = decoder
            .read_byte()
            .map_err(|err| DecodeError::new(format!("{context}: {}", err.message)))?;
        raw.push(byte);
        if byte < 0x80 || raw.len() == 10 {
            break;
        }
    }

    let (value, _) = decode(&raw).map_err(|err| DecodeError::new(format!("{context}: {err}")))?;
    Ok((value, raw))
}

#[cfg(test)]
mod tests {
    use super::{decode_const_expr, decode_const_expr_with_extended_const};
    use crate::decoder::Decoder;
    use razero_features::CoreFeatures;
    use razero_wasm::const_expr::ConstExpr;
    use razero_wasm::instruction::{
        OPCODE_END, OPCODE_I32_ADD, OPCODE_I32_CONST, OPCODE_REF_FUNC, OPCODE_REF_NULL,
        OPCODE_VEC_PREFIX, OPCODE_VEC_V128_CONST,
    };

    #[test]
    fn decodes_ref_func_const_expr() {
        let mut decoder = Decoder::new(&[OPCODE_REF_FUNC, 0x80, 0x00, OPCODE_END]);
        assert_eq!(
            ConstExpr::new(vec![OPCODE_REF_FUNC, 0x80, 0x00, OPCODE_END]),
            decode_const_expr(&mut decoder, CoreFeatures::BULK_MEMORY).unwrap()
        );
    }

    #[test]
    fn decodes_v128_const_expr() {
        let mut bytes = vec![OPCODE_VEC_PREFIX, OPCODE_VEC_V128_CONST];
        bytes.extend_from_slice(&[1; 16]);
        bytes.push(OPCODE_END);

        let mut decoder = Decoder::new(&bytes);
        assert_eq!(
            ConstExpr::new(bytes.clone()),
            decode_const_expr(&mut decoder, CoreFeatures::SIMD).unwrap()
        );
    }

    #[test]
    fn rejects_extended_const_when_disabled() {
        let mut decoder = Decoder::new(&[
            OPCODE_I32_CONST,
            0x01,
            OPCODE_I32_CONST,
            0x01,
            OPCODE_I32_ADD,
            OPCODE_END,
        ]);
        let err = decode_const_expr(&mut decoder, CoreFeatures::empty()).unwrap_err();
        assert_eq!(
            "i32.add is not supported in a constant expression as feature \"extended-const\" is disabled",
            err.message
        );
    }

    #[test]
    fn allows_extended_const_when_enabled() {
        let bytes = vec![
            OPCODE_I32_CONST,
            0x01,
            OPCODE_I32_CONST,
            0x01,
            OPCODE_I32_ADD,
            OPCODE_END,
        ];
        let mut decoder = Decoder::new(&bytes);
        assert_eq!(
            ConstExpr::new(bytes.clone()),
            decode_const_expr_with_extended_const(&mut decoder, CoreFeatures::empty(), true)
                .unwrap()
        );
    }

    #[test]
    fn rejects_invalid_ref_null_type() {
        let mut decoder = Decoder::new(&[OPCODE_REF_NULL, 0xff, OPCODE_END]);
        let err = decode_const_expr(&mut decoder, CoreFeatures::BULK_MEMORY).unwrap_err();
        assert_eq!("invalid type for ref.null: 0xff", err.message);
    }
}
