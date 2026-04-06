#![doc = "Function type and function section decoding."]

use crate::decoder::Decoder;
use crate::errors::{DecodeError, DecodeResult};
use razero::CoreFeatures;
use razero_wasm::module::{FunctionType, ValueType};

pub(crate) fn decode_value_type(byte: u8) -> DecodeResult<ValueType> {
    match byte {
        0x7f => Ok(ValueType::I32),
        0x7e => Ok(ValueType::I64),
        0x7d => Ok(ValueType::F32),
        0x7c => Ok(ValueType::F64),
        0x7b => Ok(ValueType::V128),
        0x70 => Ok(ValueType::FUNCREF),
        0x6f => Ok(ValueType::EXTERNREF),
        other => Err(DecodeError::new(format!("invalid value type: {other}"))),
    }
}

pub(crate) fn decode_value_types(
    decoder: &mut Decoder<'_>,
    num: u32,
) -> DecodeResult<Vec<ValueType>> {
    if num == 0 {
        return Ok(Vec::new());
    }

    let bytes = decoder.read_bytes(num as usize)?;
    bytes.iter().map(|byte| decode_value_type(*byte)).collect()
}

fn require_feature(
    enabled_features: CoreFeatures,
    feature: CoreFeatures,
    feature_name: &str,
) -> DecodeResult<()> {
    if enabled_features.contains(feature) {
        Ok(())
    } else {
        Err(DecodeError::new(format!(
            "feature \"{feature_name}\" is disabled"
        )))
    }
}

pub fn decode_function_type(
    enabled_features: CoreFeatures,
    decoder: &mut Decoder<'_>,
    ret: &mut FunctionType,
) -> DecodeResult<()> {
    let byte = decoder
        .read_byte()
        .map_err(|err| DecodeError::new(format!("read leading byte: {}", err.message)))?;
    if byte != 0x60 {
        return Err(DecodeError::new(format!("invalid byte: {byte:#x} != 0x60")));
    }

    let param_count = decoder
        .read_var_u32("could not read parameter count")
        .map_err(|err| DecodeError::new(err.message))?;
    let param_types = decode_value_types(decoder, param_count).map_err(|err| {
        DecodeError::new(format!("could not read parameter types: {}", err.message))
    })?;

    let result_count = decoder
        .read_var_u32("could not read result count")
        .map_err(|err| DecodeError::new(err.message))?;
    if result_count > 1 {
        require_feature(enabled_features, CoreFeatures::MULTI_VALUE, "multi-value").map_err(
            |err| DecodeError::new(format!("multiple result types invalid as {}", err.message)),
        )?;
    }

    let result_types = decode_value_types(decoder, result_count)
        .map_err(|err| DecodeError::new(format!("could not read result types: {}", err.message)))?;

    ret.params = param_types;
    ret.results = result_types;
    let _ = ret.key();
    Ok(())
}

pub fn decode_function_section(decoder: &mut Decoder<'_>) -> DecodeResult<Vec<u32>> {
    let count = decoder
        .read_var_u32("get size of vector")
        .map_err(|err| DecodeError::new(err.message))?;

    let mut result = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let type_index = decoder
            .read_var_u32("get type index")
            .map_err(|err| DecodeError::new(err.message))?;
        result.push(type_index);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{decode_function_section, decode_function_type};
    use crate::decoder::Decoder;
    use razero::CoreFeatures;
    use razero_wasm::leb128;
    use razero_wasm::module::{FunctionType, ValueType};

    #[test]
    fn decodes_function_type() {
        let mut decoder = Decoder::new(&[
            0x60,
            0x02,
            ValueType::I32.0,
            ValueType::FUNCREF.0,
            0x02,
            ValueType::EXTERNREF.0,
            ValueType::I64.0,
        ]);
        let mut actual = FunctionType::default();

        decode_function_type(CoreFeatures::all(), &mut decoder, &mut actual).unwrap();

        assert_eq!(vec![ValueType::I32, ValueType::FUNCREF], actual.params);
        assert_eq!(vec![ValueType::EXTERNREF, ValueType::I64], actual.results);
        assert_eq!("i32funcref_externrefi64", actual.key());
    }

    #[test]
    fn rejects_multi_value_when_disabled() {
        let mut decoder = Decoder::new(&[0x60, 0x00, 0x02, ValueType::I32.0, ValueType::I64.0]);
        let err = decode_function_type(
            CoreFeatures::empty(),
            &mut decoder,
            &mut FunctionType::default(),
        )
        .unwrap_err();

        assert_eq!(
            "multiple result types invalid as feature \"multi-value\" is disabled",
            err.message
        );
    }

    #[test]
    fn rejects_invalid_parameter_type() {
        let mut decoder = Decoder::new(&[0x60, 0x01, 0x6e, 0x00]);
        let err = decode_function_type(
            CoreFeatures::all(),
            &mut decoder,
            &mut FunctionType::default(),
        )
        .unwrap_err();

        assert_eq!(
            "could not read parameter types: invalid value type: 110",
            err.message
        );
    }

    #[test]
    fn decodes_function_section() {
        let mut bytes = leb128::encode_u32(3);
        bytes.extend_from_slice(&leb128::encode_u32(1));
        bytes.extend_from_slice(&leb128::encode_u32(10));
        bytes.extend_from_slice(&leb128::encode_u32(127));

        let mut decoder = Decoder::new(&bytes);
        let actual = decode_function_section(&mut decoder).unwrap();

        assert_eq!(vec![1, 10, 127], actual);
        assert_eq!(0, decoder.remaining());
    }
}
