#![doc = "Wasm section envelope parsing."]

use crate::decoder::Decoder;
use crate::errors::{DecodeError, DecodeResult};
use razero_wasm::module::SectionId;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SectionHeader {
    pub id: SectionId,
    pub size: u32,
}

impl SectionHeader {
    pub fn decode(decoder: &mut Decoder<'_>) -> DecodeResult<Option<Self>> {
        let Some(id) = decoder.read_optional_byte()? else {
            return Ok(None);
        };

        let size = decoder.read_var_u32("get size of section")?;
        Ok(Some(Self {
            id: SectionId(id),
            size,
        }))
    }

    pub fn name(self) -> &'static str {
        self.id.name()
    }
}

#[derive(Debug, Clone)]
pub struct Section<'a> {
    pub header: SectionHeader,
    decoder: Decoder<'a>,
}

impl<'a> Section<'a> {
    pub(crate) fn new(header: SectionHeader, bytes: &'a [u8]) -> Self {
        Self {
            header,
            decoder: Decoder::new(bytes),
        }
    }

    pub fn decoder(&self) -> &Decoder<'a> {
        &self.decoder
    }

    pub fn decoder_mut(&mut self) -> &mut Decoder<'a> {
        &mut self.decoder
    }

    pub fn bytes_consumed(&self) -> usize {
        self.header.size as usize - self.decoder.remaining()
    }

    pub fn finish(self) -> DecodeResult<()> {
        let consumed = self.bytes_consumed() as u32;
        if consumed != self.header.size {
            return Err(DecodeError::new(format!(
                "invalid section length: expected to be {} but got {}",
                self.header.size, consumed
            )));
        }
        Ok(())
    }
}

pub fn check_section_order(current: SectionId, previous: SectionId) -> Option<SectionId> {
    if current == SectionId::CUSTOM {
        return Some(previous);
    }

    if current > SectionId::DATA_COUNT {
        return None;
    }
    if current == SectionId::DATA_COUNT {
        return (previous <= SectionId::ELEMENT).then_some(current);
    }
    if previous == SectionId::DATA_COUNT {
        return (current >= SectionId::CODE).then_some(current);
    }

    (current > previous).then_some(current)
}

#[cfg(test)]
mod tests {
    use super::{check_section_order, SectionHeader};
    use crate::decoder::Decoder;
    use razero_wasm::module::SectionId;

    #[test]
    fn decodes_section_header() {
        let mut decoder = Decoder::new(&[SectionId::TYPE.0, 0x03, 0xaa, 0xbb, 0xcc]);
        let header = SectionHeader::decode(&mut decoder)
            .unwrap()
            .expect("section header");

        assert_eq!(SectionId::TYPE, header.id);
        assert_eq!(3, header.size);
        assert_eq!(3, decoder.remaining());
    }

    #[test]
    fn allows_custom_sections_anywhere() {
        assert_eq!(
            Some(SectionId::TYPE),
            check_section_order(SectionId::CUSTOM, SectionId::TYPE)
        );
        assert_eq!(
            Some(SectionId::TYPE),
            check_section_order(SectionId::TYPE, SectionId::CUSTOM)
        );
    }

    #[test]
    fn enforces_data_count_special_case() {
        assert_eq!(
            Some(SectionId::DATA_COUNT),
            check_section_order(SectionId::DATA_COUNT, SectionId::ELEMENT)
        );
        assert_eq!(
            Some(SectionId::CODE),
            check_section_order(SectionId::CODE, SectionId::DATA_COUNT)
        );
        assert_eq!(
            None,
            check_section_order(SectionId::DATA_COUNT, SectionId::CODE)
        );
        assert_eq!(
            None,
            check_section_order(SectionId::START, SectionId::DATA_COUNT)
        );
    }

    #[test]
    fn rejects_unknown_or_duplicate_non_custom_sections() {
        assert_eq!(None, check_section_order(SectionId(13), SectionId::CUSTOM));
        assert_eq!(None, check_section_order(SectionId::TYPE, SectionId::TYPE));
    }
}
