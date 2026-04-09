#![doc = "Core Wasm decoder cursor and module driver."]

use crate::code::decode_code_section;
use crate::custom::decode_custom_section;
use crate::data::decode_data_segment_with_extended_const;
use crate::element::decode_element_segment_with_extended_const;
use crate::errors::{DecodeError, DecodeResult};
use crate::export::decode_export_section;
use crate::function::{decode_function_section, decode_function_type};
use crate::global::decode_global_with_extended_const;
use crate::header::{Header, WASM_HEADER_LEN};
use crate::import::decode_import_section;
use crate::memory::{decode_memory_section, MemorySizer};
use crate::names::decode_name_section;
use crate::section::{check_section_order, Section, SectionHeader};
use crate::table::decode_table_section;
use razero_features::CoreFeatures;
use razero_wasm::leb128;
use razero_wasm::module::{
    DataSegment, ElementSegment, FunctionType, Global, Module, SectionId, MEMORY_LIMIT_PAGES,
};

pub fn decode_module(bytes: &[u8], enabled_features: CoreFeatures) -> DecodeResult<Module> {
    ModuleDecoder::new(bytes, enabled_features).decode_module()
}

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
        self.decode_module(CoreFeatures::empty()).map(|_| ())
    }

    pub fn decode_module(&mut self, enabled_features: CoreFeatures) -> DecodeResult<Module> {
        let start = self.position;
        let decoder = self.bytes.get(self.position..).unwrap_or_default();
        let module = ModuleDecoder::new(decoder, enabled_features).decode_module()?;
        self.position = start + decoder.len();
        Ok(module)
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
        self.module
            .validate(self.enabled_features, MEMORY_LIMIT_PAGES)
            .map_err(|err| DecodeError::new(err.to_string()))?;
        Ok(self.module)
    }

    pub fn decode_module(mut self) -> DecodeResult<Module> {
        self.read_header()?;
        while self.decode_next_section()? {}
        self.finish()
    }

    pub fn decode_next_section(&mut self) -> DecodeResult<bool> {
        let Some(mut section) = self.next_section()? else {
            return Ok(false);
        };

        let section_name = section.header.name();
        self.decode_section(&mut section)
            .and_then(|_| section.finish())
            .map_err(|err| DecodeError::new(format!("section {section_name}: {}", err.message)))?;
        Ok(true)
    }

    pub fn into_decoder(self) -> Decoder<'a> {
        self.decoder
    }

    fn decode_section(&mut self, section: &mut Section<'a>) -> DecodeResult<()> {
        match section.header.id {
            SectionId::CUSTOM => self.decode_custom_section(section),
            SectionId::TYPE => {
                self.module.type_section =
                    decode_type_section(self.enabled_features, section.decoder_mut())?;
                Ok(())
            }
            SectionId::IMPORT => {
                let imports = decode_import_section(
                    section.decoder_mut(),
                    MemorySizer::default(),
                    self.enabled_features,
                )?;
                self.module.import_section = imports.imports;
                self.module.import_per_module = imports.per_module;
                self.module.import_function_count = imports.func_count;
                self.module.import_global_count = imports.global_count;
                self.module.import_memory_count = imports.memory_count;
                self.module.import_table_count = imports.table_count;
                Ok(())
            }
            SectionId::FUNCTION => {
                self.module.function_section = decode_function_section(section.decoder_mut())?;
                Ok(())
            }
            SectionId::TABLE => {
                self.module.table_section =
                    decode_table_section(section.decoder_mut(), self.enabled_features)?;
                Ok(())
            }
            SectionId::MEMORY => {
                self.module.memory_section = decode_memory_section(
                    section.decoder_mut(),
                    self.enabled_features,
                    MemorySizer::default(),
                )?;
                Ok(())
            }
            SectionId::GLOBAL => {
                self.module.global_section =
                    decode_global_section(section.decoder_mut(), self.enabled_features)?;
                Ok(())
            }
            SectionId::EXPORT => {
                let (exports, export_map) = decode_export_section(section.decoder_mut())?;
                self.module.export_section = exports;
                self.module.exports = export_map;
                Ok(())
            }
            SectionId::START => {
                self.module.start_section = Some(decode_start_section(section.decoder_mut())?);
                Ok(())
            }
            SectionId::ELEMENT => {
                self.module.element_section =
                    decode_element_section(section.decoder_mut(), self.enabled_features)?;
                Ok(())
            }
            SectionId::CODE => {
                self.module.code_section = decode_code_section(section.decoder_mut())?;
                Ok(())
            }
            SectionId::DATA => {
                self.module.data_section =
                    decode_data_section(section.decoder_mut(), self.enabled_features)?;
                Ok(())
            }
            SectionId::DATA_COUNT => {
                self.require_feature(CoreFeatures::BULK_MEMORY, "bulk-memory-operations")
                    .map_err(|err| {
                        DecodeError::new(format!(
                            "data count section not supported as {}",
                            err.message
                        ))
                    })?;
                self.module.data_count_section =
                    Some(decode_data_count_section(section.decoder_mut())?);
                Ok(())
            }
            _ => Err(DecodeError::new("invalid section id")),
        }
    }

    fn decode_custom_section(&mut self, section: &mut Section<'a>) -> DecodeResult<()> {
        let (name, _) = section.decoder_mut().read_utf8("custom section name")?;
        let remaining = section.decoder().remaining() as u64;

        if name == "name" {
            if self.module.name_section.is_some() {
                return Err(DecodeError::new("redundant custom section name"));
            }
            self.module.name_section = Some(decode_name_section(section.decoder_mut(), remaining)?);
        } else {
            self.module.custom_sections.push(decode_custom_section(
                section.decoder_mut(),
                name,
                remaining,
            )?);
        }
        Ok(())
    }
}

fn decode_type_section(
    enabled_features: CoreFeatures,
    decoder: &mut Decoder<'_>,
) -> DecodeResult<Vec<FunctionType>> {
    let count = decoder
        .read_var_u32("get size of vector")
        .map_err(|err| DecodeError::new(err.message))?;

    let mut types = vec![FunctionType::default(); count as usize];
    for (index, ty) in types.iter_mut().enumerate() {
        decode_function_type(enabled_features, decoder, ty)
            .map_err(|err| DecodeError::new(format!("read {index}-th type: {}", err.message)))?;
    }
    Ok(types)
}

fn decode_global_section(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
) -> DecodeResult<Vec<Global>> {
    let count = decoder
        .read_var_u32("get size of vector")
        .map_err(|err| DecodeError::new(err.message))?;
    let extended_const_enabled = enabled_features.contains(CoreFeatures::EXTENDED_CONST);

    let mut globals = Vec::with_capacity(count as usize);
    for index in 0..count {
        globals.push(
            decode_global_with_extended_const(decoder, enabled_features, extended_const_enabled)
                .map_err(|err| DecodeError::new(format!("global[{index}]: {}", err.message)))?,
        );
    }
    Ok(globals)
}

fn decode_start_section(decoder: &mut Decoder<'_>) -> DecodeResult<u32> {
    decoder
        .read_var_u32("get function index")
        .map_err(|err| DecodeError::new(err.message))
}

fn decode_element_section(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
) -> DecodeResult<Vec<ElementSegment>> {
    let count = decoder
        .read_var_u32("get size of vector")
        .map_err(|err| DecodeError::new(err.message))?;
    let extended_const_enabled = enabled_features.contains(CoreFeatures::EXTENDED_CONST);

    let mut elements = Vec::with_capacity(count as usize);
    for _ in 0..count {
        elements.push(
            decode_element_segment_with_extended_const(
                decoder,
                enabled_features,
                extended_const_enabled,
            )
            .map_err(|err| DecodeError::new(format!("read element: {}", err.message)))?,
        );
    }
    Ok(elements)
}

fn decode_data_section(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
) -> DecodeResult<Vec<DataSegment>> {
    let count = decoder
        .read_var_u32("get size of vector")
        .map_err(|err| DecodeError::new(err.message))?;
    let extended_const_enabled = enabled_features.contains(CoreFeatures::EXTENDED_CONST);

    let mut data = Vec::with_capacity(count as usize);
    for _ in 0..count {
        data.push(
            decode_data_segment_with_extended_const(
                decoder,
                enabled_features,
                extended_const_enabled,
            )
            .map_err(|err| DecodeError::new(format!("read data segment: {}", err.message)))?,
        );
    }
    Ok(data)
}

fn decode_data_count_section(decoder: &mut Decoder<'_>) -> DecodeResult<u32> {
    if decoder.remaining() == 0 {
        Ok(0)
    } else {
        decoder
            .read_var_u32("get data count")
            .map_err(|err| DecodeError::new(err.message))
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_module, Decoder, ModuleDecoder};
    use crate::header::{WASM_MAGIC, WASM_VERSION};
    use razero_features::CoreFeatures;
    use razero_wasm::const_expr::ConstExpr;
    use razero_wasm::module::{DataSegment, ExternType, SectionId};
    use std::fs;
    use std::path::PathBuf;

    fn module_prefix() -> Vec<u8> {
        let mut bytes = Vec::from(WASM_MAGIC);
        bytes.extend_from_slice(&WASM_VERSION);
        bytes
    }

    fn fixture(path: &str) -> Vec<u8> {
        let mut full_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        full_path.push(path);
        fs::read(full_path).unwrap()
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
        let bytes = module_prefix();
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

    #[test]
    fn finish_runs_module_validation() {
        let bytes = module_prefix();
        let mut decoder = ModuleDecoder::new(&bytes, CoreFeatures::empty());
        decoder.module_mut().data_section.push(DataSegment {
            offset_expression: ConstExpr::from_i32(0),
            init: vec![1],
            passive: false,
        });

        let err = decoder.finish().unwrap_err();
        assert_eq!("unknown memory", err.message);
    }

    #[test]
    fn decodes_root_testdata_modules() {
        let fac = decode_module(&fixture("../testdata/fac.wasm"), CoreFeatures::V2).unwrap();
        assert!(!fac.type_section.is_empty());
        assert!(!fac.function_section.is_empty());
        assert_eq!(fac.function_section.len(), fac.code_section.len());
        assert!(fac.type_of_function(fac.import_function_count).is_some());

        let mem_grow =
            decode_module(&fixture("../testdata/mem_grow.wasm"), CoreFeatures::V2).unwrap();
        assert!(mem_grow.memory_section.is_some());
        assert!(mem_grow
            .export_section
            .iter()
            .any(|export| export.ty == ExternType::MEMORY));
    }

    #[test]
    fn decodes_real_modules_with_data_and_modern_features() {
        let greet = decode_module(
            &fixture("../examples/allocation/rust/testdata/greet.wasm"),
            CoreFeatures::V2,
        )
        .unwrap();
        assert!(greet.memory_section.is_some() || greet.import_memory_count > 0);
        assert!(!greet.data_section.is_empty());
        assert!(!greet.export_section.is_empty());

        let multi_value = decode_module(
            &fixture("../examples/multiple-results/testdata/multi_value.wasm"),
            CoreFeatures::V2,
        )
        .unwrap();
        assert!(multi_value
            .type_section
            .iter()
            .any(|ty| ty.results.len() > 1));
        assert_eq!(
            multi_value.function_section.len(),
            multi_value.code_section.len()
        );
    }

    #[test]
    fn rejects_remaining_spectest_validator_gap_fixtures() {
        for (path, features) in [
            (
                "../internal/integration_test/spectest/v1/testdata/binary.37.wasm",
                CoreFeatures::V1,
            ),
            (
                "../internal/integration_test/spectest/v1/testdata/binary.80.wasm",
                CoreFeatures::V1,
            ),
            (
                "../internal/integration_test/spectest/v2/testdata/binary.41.wasm",
                CoreFeatures::V2,
            ),
            (
                "../internal/integration_test/spectest/extended-const/testdata/global.1.wasm",
                CoreFeatures::V2 | CoreFeatures::EXTENDED_CONST,
            ),
            (
                "../internal/integration_test/spectest/extended-const/testdata/global.2.wasm",
                CoreFeatures::V2 | CoreFeatures::EXTENDED_CONST,
            ),
        ] {
            assert!(
                decode_module(&fixture(path), features).is_err(),
                "{path} should be rejected"
            );
        }
    }
}
