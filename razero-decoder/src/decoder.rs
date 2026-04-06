#![doc = "Core Wasm decoder cursor and module driver."]

use crate::errors::{DecodeError, DecodeResult};
use crate::header::{Header, WASM_HEADER_LEN};
use crate::section::{check_section_order, Section, SectionHeader};
use razero::CoreFeatures;
use razero_wasm::leb128;
use razero_wasm::module::{Module, SectionId};

#[derive(Debug, Clone, Copy)]
pub struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn read_header(&mut self) -> DecodeResult<Header> {
        let remaining = self.bytes.get(self.position..).unwrap_or_default();
        let header = Header::decode(remaining)?;
        self.position += WASM_HEADER_LEN;
        Ok(header)
    }

    pub fn read_optional_byte(&mut self) -> DecodeResult<Option<u8>> {
        if self.remaining() == 0 {
            return Ok(None);
        }
        self.read_byte().map(Some)
    }

    pub fn read_byte(&mut self) -> DecodeResult<u8> {
        let byte = *self
            .bytes
            .get(self.position)
            .ok_or_else(|| DecodeError::new("unexpected end of input"))?;
        self.position += 1;
        Ok(byte)
    }

    pub fn read_bytes(&mut self, len: usize) -> DecodeResult<&'a [u8]> {
        let end = self
            .position
            .checked_add(len)
            .ok_or_else(|| DecodeError::new("unexpected end of input"))?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| DecodeError::new("unexpected end of input"))?;
        self.position = end;
        Ok(bytes)
    }

    pub fn skip(&mut self, len: usize) -> DecodeResult<()> {
        self.read_bytes(len).map(|_| ())
    }

    pub fn read_var_u32(&mut self, context: &str) -> DecodeResult<u32> {
        let bytes = self.bytes.get(self.position..).unwrap_or_default();
        let (value, read) = leb128::decode_u32(bytes)
            .map_err(|err| DecodeError::new(format!("{context}: {err}")))?;
        self.position += read;
        Ok(value)
    }

    pub fn read_utf8(&mut self, context: &str) -> DecodeResult<(String, u32)> {
        let size_before = self.position;
        let size = self.read_var_u32(&format!("failed to read {context} size"))?;
        if size == 0 {
            return Ok((String::new(), (self.position - size_before) as u32));
        }

        let bytes = self.read_bytes(size as usize).map_err(|err| {
            DecodeError::new(format!("failed to read {context}: {}", err.message))
        })?;
        let value = std::str::from_utf8(bytes)
            .map_err(|_| DecodeError::new(format!("{context} is not valid UTF-8")))?;

        Ok((value.to_owned(), (self.position - size_before) as u32))
    }

    pub fn decode(&mut self) -> DecodeResult<()> {
        let start = self.position;
        let mut module = ModuleDecoder::new(
            self.bytes.get(self.position..).unwrap_or_default(),
            CoreFeatures::empty(),
        );
        module.read_header()?;
        while let Some(section) = module.next_section()? {
            section.finish()?;
        }
        self.position = start + module.into_decoder().position();
        Ok(())
    }
}

#[derive(Debug)]
pub struct ModuleDecoder<'a> {
    decoder: Decoder<'a>,
    enabled_features: CoreFeatures,
    module: Module,
    last_section_id: SectionId,
    header_read: bool,
}

impl<'a> ModuleDecoder<'a> {
    pub fn new(bytes: &'a [u8], enabled_features: CoreFeatures) -> Self {
        Self {
            decoder: Decoder::new(bytes),
            enabled_features,
            module: Module::default(),
            last_section_id: SectionId::CUSTOM,
            header_read: false,
        }
    }

    pub fn enabled_features(&self) -> CoreFeatures {
        self.enabled_features
    }

    pub fn require_feature(&self, feature: CoreFeatures, feature_name: &str) -> DecodeResult<()> {
        if self.enabled_features.contains(feature) {
            Ok(())
        } else {
            Err(DecodeError::new(format!(
                "feature \"{feature_name}\" is disabled"
            )))
        }
    }

    pub fn module(&self) -> &Module {
        &self.module
    }

    pub fn module_mut(&mut self) -> &mut Module {
        &mut self.module
    }

    pub fn read_header(&mut self) -> DecodeResult<Header> {
        let header = self.decoder.read_header()?;
        self.header_read = true;
        Ok(header)
    }

    pub fn next_section(&mut self) -> DecodeResult<Option<Section<'a>>> {
        if !self.header_read {
            self.read_header()?;
        }

        let Some(header) = SectionHeader::decode(&mut self.decoder)? else {
            return Ok(None);
        };

        self.last_section_id = check_section_order(header.id, self.last_section_id)
            .ok_or_else(|| DecodeError::new("invalid section order"))?;

        let bytes = self.decoder.read_bytes(header.size as usize)?;
        Ok(Some(Section::new(header, bytes)))
    }

    pub fn finish(self) -> DecodeResult<Module> {
        let function_count = self.module.section_element_count(SectionId::FUNCTION);
        let code_count = self.module.section_element_count(SectionId::CODE);
        if function_count != code_count {
            return Err(DecodeError::new(format!(
                "function and code section have inconsistent lengths: {function_count} != {code_count}"
            )));
        }
        Ok(self.module)
    }

    pub fn into_decoder(self) -> Decoder<'a> {
        self.decoder
    }
}

#[cfg(test)]
mod tests {
    use super::{Decoder, ModuleDecoder};
    use crate::header::{WASM_MAGIC, WASM_VERSION};
    use razero::CoreFeatures;
    use razero_wasm::module::SectionId;

    fn module_prefix() -> Vec<u8> {
        let mut bytes = Vec::from(WASM_MAGIC);
        bytes.extend_from_slice(&WASM_VERSION);
        bytes
    }

    #[test]
    fn decoder_reads_utf8_and_size() {
        let mut decoder = Decoder::new(&[0x03, b'f', b'o', b'o']);
        let (value, size) = decoder.read_utf8("custom section name").unwrap();

        assert_eq!("foo", value);
        assert_eq!(4, size);
        assert_eq!(0, decoder.remaining());
    }

    #[test]
    fn decoder_validates_module_envelope() {
        let mut bytes = module_prefix();
        bytes.extend_from_slice(&[SectionId::TYPE.0, 0x00, SectionId::FUNCTION.0, 0x00]);

        let mut decoder = Decoder::new(&bytes);
        decoder.decode().unwrap();

        assert_eq!(bytes.len(), decoder.position());
    }

    #[test]
    fn decoder_rejects_invalid_utf8() {
        let mut decoder = Decoder::new(&[0x01, 0xff]);
        let err = decoder.read_utf8("export name").unwrap_err();
        assert_eq!("export name is not valid UTF-8", err.message);
    }

    #[test]
    fn module_decoder_detects_invalid_section_order() {
        let mut bytes = module_prefix();
        bytes.extend_from_slice(&[
            SectionId::TYPE.0,
            0x00,
            SectionId::CODE.0,
            0x00,
            SectionId::START.0,
            0x00,
        ]);

        let mut decoder = ModuleDecoder::new(&bytes, CoreFeatures::empty());
        decoder.read_header().unwrap();
        decoder.next_section().unwrap().unwrap().finish().unwrap();
        decoder.next_section().unwrap().unwrap().finish().unwrap();

        let err = decoder.next_section().unwrap_err();
        assert_eq!("invalid section order", err.message);
    }

    #[test]
    fn custom_sections_do_not_change_order_tracking() {
        let mut bytes = module_prefix();
        bytes.extend_from_slice(&[
            SectionId::TYPE.0,
            0x00,
            SectionId::CUSTOM.0,
            0x02,
            0x01,
            b'x',
            SectionId::FUNCTION.0,
            0x00,
        ]);

        let mut decoder = ModuleDecoder::new(&bytes, CoreFeatures::empty());
        decoder.read_header().unwrap();
        decoder.next_section().unwrap().unwrap().finish().unwrap();
        let mut custom = decoder.next_section().unwrap().unwrap();
        let (name, size) = custom
            .decoder_mut()
            .read_utf8("custom section name")
            .unwrap();
        assert_eq!("x", name);
        assert_eq!(2, size);
        custom.finish().unwrap();
        decoder.next_section().unwrap().unwrap().finish().unwrap();
        assert!(decoder.next_section().unwrap().is_none());
    }

    #[test]
    fn section_finish_reports_partial_consumption() {
        let mut bytes = module_prefix();
        bytes.extend_from_slice(&[SectionId::CUSTOM.0, 0x03, 0x01, b'a', 0xff]);

        let mut decoder = ModuleDecoder::new(&bytes, CoreFeatures::empty());
        let mut section = decoder.next_section().unwrap().unwrap();
        let _ = section
            .decoder_mut()
            .read_utf8("custom section name")
            .unwrap();

        let err = section.finish().unwrap_err();
        assert_eq!(
            "invalid section length: expected to be 3 but got 2",
            err.message
        );
    }

    #[test]
    fn require_feature_matches_go_wording() {
        let bytes = module_prefix();
        let decoder = ModuleDecoder::new(&bytes, CoreFeatures::empty());
        let err = decoder
            .require_feature(CoreFeatures::REFERENCE_TYPES, "reference-types")
            .unwrap_err();
        assert_eq!("feature \"reference-types\" is disabled", err.message);
    }

    #[test]
    fn finish_checks_function_and_code_counts() {
        let bytes = module_prefix();
        let mut decoder = ModuleDecoder::new(&bytes, CoreFeatures::empty());
        decoder.module_mut().function_section.push(0);

        let err = decoder.finish().unwrap_err();
        assert_eq!(
            "function and code section have inconsistent lengths: 1 != 0",
            err.message
        );
    }
}
