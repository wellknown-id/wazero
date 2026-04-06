#![doc = "Custom section decoding."]

use crate::decoder::Decoder;
use crate::errors::DecodeResult;
use razero_wasm::module::CustomSection;

pub fn decode_custom_section(
    decoder: &mut Decoder<'_>,
    name: String,
    limit: u64,
) -> DecodeResult<CustomSection> {
    Ok(CustomSection {
        name,
        data: decoder.read_bytes(limit as usize)?.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::decode_custom_section;
    use crate::decoder::Decoder;
    use razero_wasm::module::CustomSection;

    #[test]
    fn decodes_custom_section_payload() {
        let mut decoder = Decoder::new(&[1, 2, 3]);
        assert_eq!(
            CustomSection {
                name: "producers".to_string(),
                data: vec![1, 2, 3],
            },
            decode_custom_section(&mut decoder, "producers".to_string(), 3).unwrap()
        );
    }
}
