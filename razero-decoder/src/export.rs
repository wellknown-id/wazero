#![doc = "Export section decoding."]

use std::collections::BTreeMap;

use crate::decoder::Decoder;
use crate::errors::{DecodeError, DecodeResult};
use razero_wasm::module::{Export, ExternType};

pub fn decode_export(decoder: &mut Decoder<'_>, ret: &mut Export) -> DecodeResult<()> {
    ret.name = decoder.read_utf8("export name")?.0;

    let byte = decoder
        .read_byte()
        .map_err(|err| DecodeError::new(format!("error decoding export kind: {}", err.message)))?;
    ret.ty = ExternType(byte);

    match ret.ty {
        ExternType::FUNC | ExternType::TABLE | ExternType::MEMORY | ExternType::GLOBAL => {
            ret.index = decoder
                .read_var_u32("error decoding export index")
                .map_err(|err| DecodeError::new(err.message))?;
            Ok(())
        }
        _ => Err(DecodeError::new(format!(
            "invalid byte: invalid byte for exportdesc: {byte:#x}"
        ))),
    }
}

pub fn decode_export_section(
    decoder: &mut Decoder<'_>,
) -> DecodeResult<(Vec<Export>, BTreeMap<String, usize>)> {
    let count = decoder
        .read_var_u32("get size of vector")
        .map_err(|err| DecodeError::new(err.message))?;

    let mut exports = vec![Export::default(); count as usize];
    let mut export_map = BTreeMap::new();
    for (index, export) in exports.iter_mut().enumerate() {
        decode_export(decoder, export)
            .map_err(|err| DecodeError::new(format!("read export: {}", err.message)))?;
        if export_map.insert(export.name.clone(), index).is_some() {
            return Err(DecodeError::new(format!(
                "export[{index}] duplicates name {:?}",
                export.name
            )));
        }
    }
    Ok((exports, export_map))
}

#[cfg(test)]
mod tests {
    use super::decode_export_section;
    use crate::decoder::Decoder;
    use razero_wasm::module::{Export, ExternType};

    #[test]
    fn decodes_exports_and_builds_name_map() {
        let mut decoder = Decoder::new(&[
            0x02,
            0x00,
            ExternType::FUNC.0,
            0x02,
            0x01,
            b'a',
            ExternType::GLOBAL.0,
            0x01,
        ]);

        let (exports, export_map) = decode_export_section(&mut decoder).unwrap();

        assert_eq!(
            vec![
                Export {
                    ty: ExternType::FUNC,
                    name: String::new(),
                    index: 2,
                },
                Export {
                    ty: ExternType::GLOBAL,
                    name: "a".to_string(),
                    index: 1,
                },
            ],
            exports
        );
        assert_eq!(Some(&0), export_map.get(""));
        assert_eq!(Some(&1), export_map.get("a"));
    }

    #[test]
    fn rejects_duplicate_export_names() {
        let mut decoder = Decoder::new(&[
            0x02,
            0x01,
            b'a',
            ExternType::FUNC.0,
            0x00,
            0x01,
            b'a',
            ExternType::FUNC.0,
            0x00,
        ]);
        let err = decode_export_section(&mut decoder).unwrap_err();

        assert_eq!("export[1] duplicates name \"a\"", err.message);
    }
}
