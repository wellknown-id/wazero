#![doc = "Element section decoding."]

use crate::const_expr::{decode_const_expr_with_extended_const, read_var_u32_raw};
use crate::decoder::Decoder;
use crate::errors::{require_feature, DecodeError, DecodeResult};
use razero::CoreFeatures;
use razero_wasm::const_expr::ConstExpr;
use razero_wasm::module::{ElementMode, ElementSegment, RefType, MAXIMUM_FUNCTION_INDEX};

const ELEMENT_SEGMENT_PREFIX_LEGACY: u32 = 0;
const ELEMENT_SEGMENT_PREFIX_PASSIVE_FUNCREF_VALUE_VECTOR: u32 = 1;
const ELEMENT_SEGMENT_PREFIX_ACTIVE_FUNCREF_VALUE_VECTOR_WITH_TABLE_INDEX: u32 = 2;
const ELEMENT_SEGMENT_PREFIX_DECLARATIVE_FUNCREF_VALUE_VECTOR: u32 = 3;
const ELEMENT_SEGMENT_PREFIX_ACTIVE_FUNCREF_CONST_EXPR_VECTOR: u32 = 4;
const ELEMENT_SEGMENT_PREFIX_PASSIVE_CONST_EXPR_VECTOR: u32 = 5;
const ELEMENT_SEGMENT_PREFIX_ACTIVE_CONST_EXPR_VECTOR: u32 = 6;
const ELEMENT_SEGMENT_PREFIX_DECLARATIVE_CONST_EXPR_VECTOR: u32 = 7;

pub fn ensure_element_kind_funcref(decoder: &mut Decoder<'_>) -> DecodeResult<()> {
    let element_kind = decoder
        .read_byte()
        .map_err(|err| DecodeError::new(format!("read element prefix: {}", err.message)))?;
    if element_kind != 0x00 {
        return Err(DecodeError::new(format!(
            "element kind must be zero but was 0x{element_kind:x}"
        )));
    }
    Ok(())
}

pub fn decode_element_init_value_vector(decoder: &mut Decoder<'_>) -> DecodeResult<Vec<ConstExpr>> {
    let size = read_var_u32_raw(decoder, "get size of vector")?.0;
    let mut ret = Vec::with_capacity(size as usize);
    for _ in 0..size {
        let (function_index, raw) = read_var_u32_raw(decoder, "read function index")?;
        if function_index >= MAXIMUM_FUNCTION_INDEX {
            return Err(DecodeError::new(format!(
                "too large function index in Element init: {function_index}"
            )));
        }
        ret.push(ConstExpr::from_opcode(
            razero_wasm::instruction::OPCODE_REF_FUNC,
            &raw,
        ));
    }
    Ok(ret)
}

pub fn decode_element_const_expr_vector(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
) -> DecodeResult<Vec<ConstExpr>> {
    decode_element_const_expr_vector_with_extended_const(decoder, enabled_features, false)
}

pub fn decode_element_const_expr_vector_with_extended_const(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
    extended_const_enabled: bool,
) -> DecodeResult<Vec<ConstExpr>> {
    let size = read_var_u32_raw(decoder, "failed to get the size of constexpr vector")?.0;
    let mut ret = Vec::with_capacity(size as usize);
    for _ in 0..size {
        ret.push(decode_const_expr_with_extended_const(
            decoder,
            enabled_features,
            extended_const_enabled,
        )?);
    }
    Ok(ret)
}

pub fn decode_element_ref_type(decoder: &mut Decoder<'_>) -> DecodeResult<RefType> {
    let ref_type = decoder
        .read_byte()
        .map(RefType)
        .map_err(|err| DecodeError::new(format!("read element ref type: {}", err.message)))?;
    if ref_type != RefType::FUNCREF && ref_type != RefType::EXTERNREF {
        return Err(DecodeError::new(
            "ref type must be funcref or externref for element as of WebAssembly 2.0",
        ));
    }
    Ok(ref_type)
}

pub fn decode_element_segment(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
) -> DecodeResult<ElementSegment> {
    decode_element_segment_with_extended_const(decoder, enabled_features, false)
}

pub fn decode_element_segment_with_extended_const(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
    extended_const_enabled: bool,
) -> DecodeResult<ElementSegment> {
    let prefix = read_var_u32_raw(decoder, "read element prefix")?.0;
    if prefix != ELEMENT_SEGMENT_PREFIX_LEGACY {
        require_feature(
            enabled_features,
            CoreFeatures::BULK_MEMORY,
            "bulk-memory-operations",
        )
        .map_err(|err| {
            DecodeError::new(format!(
                "non-zero prefix for element segment is invalid as {}",
                err.message
            ))
        })?;
    }

    let mut ret = ElementSegment::default();
    match prefix {
        ELEMENT_SEGMENT_PREFIX_LEGACY => {
            ret.offset_expr = decode_const_expr_with_extended_const(
                decoder,
                enabled_features,
                extended_const_enabled,
            )
            .map_err(|err| DecodeError::new(format!("read expr for offset: {}", err.message)))?;
            ret.init = decode_element_init_value_vector(decoder)?;
            ret.mode = ElementMode::Active;
            ret.ty = RefType::FUNCREF;
        }
        ELEMENT_SEGMENT_PREFIX_PASSIVE_FUNCREF_VALUE_VECTOR => {
            ensure_element_kind_funcref(decoder)?;
            ret.init = decode_element_init_value_vector(decoder)?;
            ret.mode = ElementMode::Passive;
            ret.ty = RefType::FUNCREF;
        }
        ELEMENT_SEGMENT_PREFIX_ACTIVE_FUNCREF_VALUE_VECTOR_WITH_TABLE_INDEX => {
            ret.table_index = read_var_u32_raw(decoder, "get size of vector")?.0;
            if ret.table_index != 0 {
                require_feature(
                    enabled_features,
                    CoreFeatures::REFERENCE_TYPES,
                    "reference-types",
                )
                .map_err(|err| {
                    DecodeError::new(format!(
                        "table index must be zero but was {}: {}",
                        ret.table_index, err.message
                    ))
                })?;
            }
            ret.offset_expr = decode_const_expr_with_extended_const(
                decoder,
                enabled_features,
                extended_const_enabled,
            )
            .map_err(|err| DecodeError::new(format!("read expr for offset: {}", err.message)))?;
            ensure_element_kind_funcref(decoder)?;
            ret.init = decode_element_init_value_vector(decoder)?;
            ret.mode = ElementMode::Active;
            ret.ty = RefType::FUNCREF;
        }
        ELEMENT_SEGMENT_PREFIX_DECLARATIVE_FUNCREF_VALUE_VECTOR => {
            ensure_element_kind_funcref(decoder)?;
            ret.init = decode_element_init_value_vector(decoder)?;
            ret.mode = ElementMode::Declarative;
            ret.ty = RefType::FUNCREF;
        }
        ELEMENT_SEGMENT_PREFIX_ACTIVE_FUNCREF_CONST_EXPR_VECTOR => {
            ret.offset_expr = decode_const_expr_with_extended_const(
                decoder,
                enabled_features,
                extended_const_enabled,
            )
            .map_err(|err| DecodeError::new(format!("read expr for offset: {}", err.message)))?;
            ret.init = decode_element_const_expr_vector_with_extended_const(
                decoder,
                enabled_features,
                extended_const_enabled,
            )?;
            ret.mode = ElementMode::Active;
            ret.ty = RefType::FUNCREF;
        }
        ELEMENT_SEGMENT_PREFIX_PASSIVE_CONST_EXPR_VECTOR => {
            ret.ty = decode_element_ref_type(decoder)?;
            ret.init = decode_element_const_expr_vector_with_extended_const(
                decoder,
                enabled_features,
                extended_const_enabled,
            )?;
            ret.mode = ElementMode::Passive;
        }
        ELEMENT_SEGMENT_PREFIX_ACTIVE_CONST_EXPR_VECTOR => {
            ret.table_index = read_var_u32_raw(decoder, "get size of vector")?.0;
            if ret.table_index != 0 {
                require_feature(
                    enabled_features,
                    CoreFeatures::REFERENCE_TYPES,
                    "reference-types",
                )
                .map_err(|err| {
                    DecodeError::new(format!(
                        "table index must be zero but was {}: {}",
                        ret.table_index, err.message
                    ))
                })?;
            }
            ret.offset_expr = decode_const_expr_with_extended_const(
                decoder,
                enabled_features,
                extended_const_enabled,
            )
            .map_err(|err| DecodeError::new(format!("read expr for offset: {}", err.message)))?;
            ret.ty = decode_element_ref_type(decoder)?;
            ret.init = decode_element_const_expr_vector_with_extended_const(
                decoder,
                enabled_features,
                extended_const_enabled,
            )?;
            ret.mode = ElementMode::Active;
        }
        ELEMENT_SEGMENT_PREFIX_DECLARATIVE_CONST_EXPR_VECTOR => {
            ret.ty = decode_element_ref_type(decoder)?;
            ret.init = decode_element_const_expr_vector_with_extended_const(
                decoder,
                enabled_features,
                extended_const_enabled,
            )?;
            ret.mode = ElementMode::Declarative;
        }
        _ => {
            return Err(DecodeError::new(format!(
                "invalid element segment prefix: 0x{prefix:x}"
            )));
        }
    }
    Ok(ret)
}

#[cfg(test)]
mod tests {
    use super::{
        decode_element_const_expr_vector, decode_element_init_value_vector, decode_element_segment,
    };
    use crate::decoder::Decoder;
    use razero::CoreFeatures;
    use razero_wasm::const_expr::ConstExpr;
    use razero_wasm::instruction::{
        OPCODE_END, OPCODE_I32_CONST, OPCODE_REF_FUNC, OPCODE_REF_NULL,
    };
    use razero_wasm::leb128;
    use razero_wasm::module::{ElementMode, ElementSegment, RefType};

    #[test]
    fn decodes_element_init_value_vector() {
        let mut decoder = Decoder::new(&[2, 1, 2]);
        assert_eq!(
            vec![
                ConstExpr::from_opcode(OPCODE_REF_FUNC, &[1]),
                ConstExpr::from_opcode(OPCODE_REF_FUNC, &[2]),
            ],
            decode_element_init_value_vector(&mut decoder).unwrap()
        );
    }

    #[test]
    fn decodes_element_const_expr_vector() {
        let mut decoder = Decoder::new(&[
            2,
            OPCODE_REF_NULL,
            RefType::FUNCREF.0,
            OPCODE_END,
            OPCODE_REF_FUNC,
            100,
            OPCODE_END,
        ]);
        assert_eq!(
            vec![
                ConstExpr::from_opcode(OPCODE_REF_NULL, &[RefType::FUNCREF.0]),
                ConstExpr::from_opcode(OPCODE_REF_FUNC, &[100]),
            ],
            decode_element_const_expr_vector(&mut decoder, CoreFeatures::BULK_MEMORY).unwrap()
        );
    }

    #[test]
    fn decodes_active_const_expr_element_segment() {
        let mut decoder = Decoder::new(&[
            6,
            0,
            OPCODE_I32_CONST,
            0x80,
            1,
            OPCODE_END,
            RefType::FUNCREF.0,
            2,
            OPCODE_REF_NULL,
            RefType::FUNCREF.0,
            OPCODE_END,
            OPCODE_REF_FUNC,
            0x80,
            0x7f,
            OPCODE_END,
        ]);

        assert_eq!(
            ElementSegment {
                offset_expr: ConstExpr::new(vec![OPCODE_I32_CONST, 0x80, 1, OPCODE_END]),
                table_index: 0,
                init: vec![
                    ConstExpr::from_opcode(OPCODE_REF_NULL, &[RefType::FUNCREF.0]),
                    ConstExpr::from_opcode(OPCODE_REF_FUNC, &leb128::encode_u32(16256)),
                ],
                ty: RefType::FUNCREF,
                mode: ElementMode::Active,
            },
            decode_element_segment(&mut decoder, CoreFeatures::BULK_MEMORY).unwrap()
        );
    }

    #[test]
    fn rejects_non_zero_prefix_without_bulk_memory() {
        let mut decoder = Decoder::new(&[1]);
        let err = decode_element_segment(&mut decoder, CoreFeatures::empty()).unwrap_err();
        assert_eq!(
            "non-zero prefix for element segment is invalid as feature \"bulk-memory-operations\" is disabled",
            err.message
        );
    }

    #[test]
    fn rejects_large_function_index() {
        let mut decoder = Decoder::new(&[1, 0xff, 0xff, 0xff, 0xff, 0x0f]);
        let err = decode_element_init_value_vector(&mut decoder).unwrap_err();
        assert_eq!(
            "too large function index in Element init: 4294967295",
            err.message
        );
    }
}
