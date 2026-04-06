#![doc = "Value type decoding helpers."]

use crate::decoder::Decoder;
use crate::errors::{DecodeError, DecodeResult};
use razero_wasm::module::ValueType;

pub fn decode_value_types(decoder: &mut Decoder<'_>, num: u32) -> DecodeResult<Vec<ValueType>> {
    if num == 0 {
        return Ok(Vec::new());
    }

    let bytes = decoder.read_bytes(num as usize)?;
    let mut ret = Vec::with_capacity(num as usize);
    for &byte in bytes {
        let value_type = ValueType(byte);
        if !is_valid_value_type(value_type) {
            return Err(DecodeError::new(format!("invalid value type: {byte}")));
        }
        ret.push(value_type);
    }
    Ok(ret)
}

pub fn is_valid_value_type(value_type: ValueType) -> bool {
    matches!(
        value_type,
        ValueType::I32
            | ValueType::F32
            | ValueType::I64
            | ValueType::F64
            | ValueType::EXTERNREF
            | ValueType::FUNCREF
            | ValueType::V128
    )
}

#[cfg(test)]
mod tests {
    use super::decode_value_types;
    use crate::decoder::Decoder;
    use razero_wasm::module::ValueType;

    #[test]
    fn decodes_value_types() {
        let mut decoder = Decoder::new(&[
            ValueType::I32.0,
            ValueType::I64.0,
            ValueType::FUNCREF.0,
            0xff,
        ]);

        let decoded = decode_value_types(&mut decoder, 3).unwrap();
        assert_eq!(
            vec![ValueType::I32, ValueType::I64, ValueType::FUNCREF],
            decoded
        );
        assert_eq!(1, decoder.remaining());
    }

    #[test]
    fn rejects_invalid_value_type() {
        let mut decoder = Decoder::new(&[0x01]);
        let err = decode_value_types(&mut decoder, 1).unwrap_err();
        assert_eq!("invalid value type: 1", err.message);
    }
}
