#![doc = "Data section decoding."]

use crate::const_expr::{decode_const_expr_with_extended_const, read_var_u32_raw};
use crate::decoder::Decoder;
use crate::errors::{require_feature, DecodeError, DecodeResult};
use razero::CoreFeatures;
use razero_wasm::module::DataSegment;

const DATA_SEGMENT_PREFIX_ACTIVE: u32 = 0x0;
const DATA_SEGMENT_PREFIX_PASSIVE: u32 = 0x1;
const DATA_SEGMENT_PREFIX_ACTIVE_WITH_MEMORY_INDEX: u32 = 0x2;

pub fn decode_data_segment(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
) -> DecodeResult<DataSegment> {
    decode_data_segment_with_extended_const(decoder, enabled_features, false)
}

pub fn decode_data_segment_with_extended_const(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
    extended_const_enabled: bool,
) -> DecodeResult<DataSegment> {
    let prefix = read_var_u32_raw(decoder, "read data segment prefix")?.0;
    if prefix != DATA_SEGMENT_PREFIX_ACTIVE {
        require_feature(
            enabled_features,
            CoreFeatures::BULK_MEMORY,
            "bulk-memory-operations",
        )
        .map_err(|err| {
            DecodeError::new(format!(
                "non-zero prefix for data segment is invalid as {}",
                err.message
            ))
        })?;
    }

    let mut ret = DataSegment::default();
    match prefix {
        DATA_SEGMENT_PREFIX_ACTIVE | DATA_SEGMENT_PREFIX_ACTIVE_WITH_MEMORY_INDEX => {
            if prefix == DATA_SEGMENT_PREFIX_ACTIVE_WITH_MEMORY_INDEX {
                let memory_index = read_var_u32_raw(decoder, "read memory index")?.0;
                if memory_index != 0 {
                    return Err(DecodeError::new(format!(
                        "memory index must be zero but was {memory_index}"
                    )));
                }
            }
            ret.offset_expression = decode_const_expr_with_extended_const(
                decoder,
                enabled_features,
                extended_const_enabled,
            )
            .map_err(|err| DecodeError::new(format!("read offset expression: {}", err.message)))?;
        }
        DATA_SEGMENT_PREFIX_PASSIVE => ret.passive = true,
        _ => {
            return Err(DecodeError::new(format!(
                "invalid data segment prefix: 0x{prefix:x}"
            )));
        }
    }

    let size = read_var_u32_raw(decoder, "get the size of vector")?.0;
    ret.init = decoder
        .read_bytes(size as usize)
        .map(|bytes| bytes.to_vec())
        .map_err(|err| DecodeError::new(format!("read bytes for init: {}", err.message)))?;
    Ok(ret)
}

#[cfg(test)]
mod tests {
    use super::decode_data_segment;
    use crate::decoder::Decoder;
    use razero::CoreFeatures;
    use razero_wasm::const_expr::ConstExpr;
    use razero_wasm::instruction::{OPCODE_END, OPCODE_I32_CONST};
    use razero_wasm::module::DataSegment;

    #[test]
    fn decodes_active_data_segment() {
        let mut decoder = Decoder::new(&[0x0, OPCODE_I32_CONST, 0x1, OPCODE_END, 0x2, 0xf, 0xe]);
        assert_eq!(
            DataSegment {
                offset_expression: ConstExpr::new(vec![OPCODE_I32_CONST, 0x1, OPCODE_END]),
                init: vec![0xf, 0xe],
                passive: false,
            },
            decode_data_segment(&mut decoder, CoreFeatures::BULK_MEMORY).unwrap()
        );
    }

    #[test]
    fn decodes_passive_data_segment() {
        let mut decoder = Decoder::new(&[0x1, 0x2, 0xf, 0xf]);
        assert_eq!(
            DataSegment {
                offset_expression: ConstExpr::default(),
                init: vec![0xf, 0xf],
                passive: true,
            },
            decode_data_segment(&mut decoder, CoreFeatures::BULK_MEMORY).unwrap()
        );
    }

    #[test]
    fn rejects_non_zero_prefix_without_bulk_memory() {
        let mut decoder = Decoder::new(&[0x2, 0x0, OPCODE_I32_CONST, 0x1, OPCODE_END, 0x0]);
        let err = decode_data_segment(&mut decoder, CoreFeatures::empty()).unwrap_err();
        assert_eq!(
            "non-zero prefix for data segment is invalid as feature \"bulk-memory-operations\" is disabled",
            err.message
        );
    }

    #[test]
    fn rejects_non_zero_memory_index() {
        let mut decoder = Decoder::new(&[0x2, 0x1, OPCODE_I32_CONST, 0x1, OPCODE_END, 0x0]);
        let err = decode_data_segment(&mut decoder, CoreFeatures::BULK_MEMORY).unwrap_err();
        assert_eq!("memory index must be zero but was 1", err.message);
    }
}
