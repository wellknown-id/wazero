#![doc = "Code section decoding."]

use crate::decoder::Decoder;
use crate::errors::{DecodeError, DecodeResult};
use crate::function::decode_value_type;
use razero_wasm::instruction::OPCODE_END;
use razero_wasm::module::Code;

pub fn decode_code(
    decoder: &mut Decoder<'_>,
    code_section_start: u64,
    ret: &mut Code,
) -> DecodeResult<()> {
    let size_start = decoder.position();
    let code_size = decoder
        .read_var_u32("get the size of code")
        .map_err(|err| DecodeError::new(err.message))?;
    let mut remaining = i64::from(code_size);

    let locals_size_start = decoder.position();
    let locals_group_count = decoder
        .read_var_u32("get the size locals")
        .map_err(|err| DecodeError::new(err.message))?;
    remaining -= (decoder.position() - locals_size_start) as i64;
    if remaining < 0 {
        return Err(DecodeError::new("unexpected end of input"));
    }

    let mut local_decls = Vec::with_capacity(locals_group_count as usize);
    let mut local_count_sum = 0_u64;
    for _ in 0..locals_group_count {
        let local_count_start = decoder.position();
        let local_count = decoder
            .read_var_u32("read n of locals")
            .map_err(|err| DecodeError::new(err.message))?;
        remaining -= (decoder.position() - local_count_start) as i64;
        if remaining < 0 {
            return Err(DecodeError::new("unexpected end of input"));
        }

        let local_type_byte = decoder
            .read_byte()
            .map_err(|err| DecodeError::new(format!("read type of local: {}", err.message)))?;
        remaining -= 1;
        if remaining < 0 {
            return Err(DecodeError::new("unexpected end of input"));
        }

        let local_type = decode_value_type(local_type_byte)
            .map_err(|_| DecodeError::new(format!("invalid local type: {local_type_byte:#x}")))?;
        local_count_sum += u64::from(local_count);
        if local_count_sum > u64::from(u32::MAX) {
            return Err(DecodeError::new(format!(
                "too many locals: {local_count_sum}"
            )));
        }
        local_decls.push((local_count, local_type));
    }

    let mut local_types = Vec::with_capacity(local_count_sum as usize);
    for (count, local_type) in local_decls {
        local_types.extend(std::iter::repeat_n(local_type, count as usize));
    }

    let body_offset_in_code_section = code_section_start - decoder.remaining() as u64;
    let body = decoder
        .read_bytes(remaining as usize)
        .map(|bytes| bytes.to_vec())
        .map_err(|err| DecodeError::new(format!("read body: {}", err.message)))?;
    if body.last().copied() != Some(OPCODE_END) {
        return Err(DecodeError::new("expr not end with OpcodeEnd"));
    }

    let _ = size_start;
    ret.local_types = local_types;
    ret.body = body;
    ret.body_offset_in_code_section = body_offset_in_code_section;
    Ok(())
}

pub fn decode_code_section(decoder: &mut Decoder<'_>) -> DecodeResult<Vec<Code>> {
    let code_section_start = decoder.remaining() as u64;
    let count = decoder
        .read_var_u32("get size of vector")
        .map_err(|err| DecodeError::new(err.message))?;

    let mut result = vec![Code::default(); count as usize];
    for (index, code) in result.iter_mut().enumerate() {
        decode_code(decoder, code_section_start, code).map_err(|err| {
            DecodeError::new(format!("read {}-th code segment: {}", index, err.message))
        })?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::decode_code_section;
    use crate::decoder::Decoder;
    use razero_wasm::instruction::OPCODE_END;
    use razero_wasm::leb128;
    use razero_wasm::module::{Code, ValueType};

    #[test]
    fn decodes_code_section_and_expands_locals() {
        let mut bytes = leb128::encode_u32(1);
        bytes.extend_from_slice(&[
            0x07,
            0x02,
            0x02,
            ValueType::I32.0,
            0x01,
            ValueType::I64.0,
            0x00,
            OPCODE_END,
        ]);

        let mut decoder = Decoder::new(&bytes);
        let actual = decode_code_section(&mut decoder).unwrap();

        assert_eq!(
            vec![Code {
                local_types: vec![ValueType::I32, ValueType::I32, ValueType::I64],
                body: vec![0x00, OPCODE_END],
                body_offset_in_code_section: 7,
                ..Code::default()
            }],
            actual
        );
        assert_eq!(0, decoder.remaining());
    }

    #[test]
    fn rejects_invalid_local_type() {
        let mut bytes = leb128::encode_u32(1);
        bytes.extend_from_slice(&[0x03, 0x01, 0x01, 0x6e]);

        let mut decoder = Decoder::new(&bytes);
        let err = decode_code_section(&mut decoder).unwrap_err();

        assert_eq!(
            "read 0-th code segment: invalid local type: 0x6e",
            err.message
        );
    }

    #[test]
    fn rejects_body_without_end_opcode() {
        let mut bytes = leb128::encode_u32(1);
        bytes.extend_from_slice(&[0x02, 0x00, 0x00, 0x00]);

        let mut decoder = Decoder::new(&bytes);
        let err = decode_code_section(&mut decoder).unwrap_err();

        assert_eq!(
            "read 0-th code segment: expr not end with OpcodeEnd",
            err.message
        );
    }
}
