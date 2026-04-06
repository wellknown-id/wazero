#![doc = "Global section decoding."]

use crate::const_expr::decode_const_expr_with_extended_const;
use crate::decoder::Decoder;
use crate::errors::{DecodeError, DecodeResult, ERR_INVALID_BYTE};
use crate::value::decode_value_types;
use razero::CoreFeatures;
use razero_wasm::module::{Global, GlobalType};

pub fn decode_global(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
) -> DecodeResult<Global> {
    decode_global_with_extended_const(decoder, enabled_features, false)
}

pub fn decode_global_with_extended_const(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
    extended_const_enabled: bool,
) -> DecodeResult<Global> {
    Ok(Global {
        ty: decode_global_type(decoder)?,
        init: decode_const_expr_with_extended_const(
            decoder,
            enabled_features,
            extended_const_enabled,
        )?,
    })
}

pub fn decode_global_type(decoder: &mut Decoder<'_>) -> DecodeResult<GlobalType> {
    let mut value_types = decode_value_types(decoder, 1)
        .map_err(|err| DecodeError::new(format!("read value type: {}", err.message)))?;
    let mutability = decoder
        .read_byte()
        .map_err(|err| DecodeError::new(format!("read mutablity: {}", err.message)))?;

    let mutable = match mutability {
        0x00 => false,
        0x01 => true,
        _ => {
            return Err(DecodeError::new(format!(
                "{ERR_INVALID_BYTE} for mutability: {mutability:#x} != 0x00 or 0x01"
            )));
        }
    };

    Ok(GlobalType {
        val_type: value_types.remove(0),
        mutable,
    })
}

#[cfg(test)]
mod tests {
    use super::{decode_global, decode_global_type};
    use crate::decoder::Decoder;
    use razero::CoreFeatures;
    use razero_wasm::const_expr::ConstExpr;
    use razero_wasm::instruction::{OPCODE_END, OPCODE_I32_CONST};
    use razero_wasm::module::{Global, GlobalType, ValueType};

    #[test]
    fn decodes_global_type() {
        let mut decoder = Decoder::new(&[ValueType::I64.0, 0x01]);
        assert_eq!(
            GlobalType {
                val_type: ValueType::I64,
                mutable: true,
            },
            decode_global_type(&mut decoder).unwrap()
        );
    }

    #[test]
    fn decodes_global() {
        let mut decoder =
            Decoder::new(&[ValueType::I32.0, 0x00, OPCODE_I32_CONST, 0x01, OPCODE_END]);
        assert_eq!(
            Global {
                ty: GlobalType {
                    val_type: ValueType::I32,
                    mutable: false,
                },
                init: ConstExpr::new(vec![OPCODE_I32_CONST, 0x01, OPCODE_END]),
            },
            decode_global(&mut decoder, CoreFeatures::empty()).unwrap()
        );
    }

    #[test]
    fn rejects_invalid_mutability() {
        let mut decoder = Decoder::new(&[ValueType::I32.0, 0x02]);
        let err = decode_global_type(&mut decoder).unwrap_err();
        assert_eq!(
            "invalid byte for mutability: 0x2 != 0x00 or 0x01",
            err.message
        );
    }
}
