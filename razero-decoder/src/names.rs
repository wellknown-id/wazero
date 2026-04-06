#![doc = "Name custom section decoding."]

use crate::decoder::Decoder;
use crate::errors::{DecodeError, DecodeResult};
use razero_wasm::module::{IndirectNameMap, NameAssoc, NameMap, NameMapAssoc, NameSection};

const SUBSECTION_ID_MODULE_NAME: u8 = 0;
const SUBSECTION_ID_FUNCTION_NAMES: u8 = 1;
const SUBSECTION_ID_LOCAL_NAMES: u8 = 2;

pub fn decode_name_section(decoder: &mut Decoder<'_>, mut limit: u64) -> DecodeResult<NameSection> {
    let mut result = NameSection::default();

    while limit > 0 {
        let Some(subsection_id) = decoder.read_optional_byte()? else {
            return Ok(result);
        };
        limit = limit.saturating_sub(1);

        let size_before = decoder.position();
        let subsection_size = decoder.read_var_u32(&format!(
            "failed to read the size of subsection[{subsection_id}]"
        ))?;
        let size_after = decoder.position();
        limit = limit.saturating_sub((size_after - size_before) as u64);

        match subsection_id {
            SUBSECTION_ID_MODULE_NAME => {
                result.module_name = decoder.read_utf8("module name")?.0;
            }
            SUBSECTION_ID_FUNCTION_NAMES => {
                result.function_names = decode_function_names(decoder)?;
            }
            SUBSECTION_ID_LOCAL_NAMES => {
                result.local_names = decode_local_names(decoder)?;
            }
            _ => {
                decoder.skip(subsection_size as usize).map_err(|err| {
                    DecodeError::new(format!(
                        "failed to skip subsection[{subsection_id}]: {}",
                        err.message
                    ))
                })?;
            }
        }
        limit = limit.saturating_sub(u64::from(subsection_size));
    }

    Ok(result)
}

fn decode_function_names(decoder: &mut Decoder<'_>) -> DecodeResult<NameMap> {
    let function_count =
        decoder.read_var_u32("failed to read the function count of subsection[1]")?;
    let mut result = Vec::with_capacity(function_count as usize);

    for _ in 0..function_count {
        let function_index =
            decoder.read_var_u32("failed to read a function index in subsection[1]")?;
        let (name, _) = decoder.read_utf8(&format!("function[{function_index}] name"))?;
        result.push(NameAssoc {
            index: function_index,
            name,
        });
    }

    Ok(result)
}

fn decode_local_names(decoder: &mut Decoder<'_>) -> DecodeResult<IndirectNameMap> {
    let function_count =
        decoder.read_var_u32("failed to read the function count of subsection[2]")?;
    let mut result = Vec::with_capacity(function_count as usize);

    for _ in 0..function_count {
        let function_index =
            decoder.read_var_u32("failed to read a function index in subsection[2]")?;
        let local_count = decoder.read_var_u32(&format!(
            "failed to read the local count for function[{function_index}]"
        ))?;

        let mut locals = Vec::with_capacity(local_count as usize);
        for _ in 0..local_count {
            let local_index = decoder.read_var_u32(&format!(
                "failed to read a local index of function[{function_index}]"
            ))?;
            let (name, _) = decoder.read_utf8(&format!(
                "function[{function_index}] local[{local_index}] name"
            ))?;
            locals.push(NameAssoc {
                index: local_index,
                name,
            });
        }

        result.push(NameMapAssoc {
            index: function_index,
            name_map: locals,
        });
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::decode_name_section;
    use crate::decoder::Decoder;
    use razero_wasm::module::{NameAssoc, NameMapAssoc, NameSection};

    #[test]
    fn decodes_name_section() {
        let bytes = vec![
            0, 7, 6, b's', b'i', b'm', b'p', b'l', b'e', // module name subsection
            1, 7, 1, 0, 4, b'm', b'a', b'i', b'n', // function names subsection
            2, 9, 1, 0, 1, 0, 3, b'a', b'r', b'g', // local names subsection
        ];
        let mut decoder = Decoder::new(&bytes);

        assert_eq!(
            NameSection {
                module_name: "simple".to_string(),
                function_names: vec![NameAssoc {
                    index: 0,
                    name: "main".to_string(),
                }],
                local_names: vec![NameMapAssoc {
                    index: 0,
                    name_map: vec![NameAssoc {
                        index: 0,
                        name: "arg".to_string(),
                    }],
                }],
                result_names: vec![],
            },
            decode_name_section(&mut decoder, bytes.len() as u64).unwrap()
        );
    }

    #[test]
    fn skips_unknown_subsection() {
        let bytes = vec![4, 2, 0xaa, 0xbb, 0, 4, 3, b'f', b'o', b'o'];
        let mut decoder = Decoder::new(&bytes);
        let section = decode_name_section(&mut decoder, bytes.len() as u64).unwrap();
        assert_eq!("foo", section.module_name);
    }

    #[test]
    fn reports_function_name_eof() {
        let bytes = vec![1, 50, 2, 0];
        let mut decoder = Decoder::new(&bytes);
        let err = decode_name_section(&mut decoder, bytes.len() as u64).unwrap_err();
        assert_eq!(
            "failed to read function[0] name size: unexpected end of LEB128 input",
            err.message
        );
    }
}
