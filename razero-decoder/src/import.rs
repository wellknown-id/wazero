#![doc = "Import section decoding."]

use std::collections::BTreeMap;

use crate::decoder::Decoder;
use crate::errors::{DecodeError, DecodeResult};
use crate::function::decode_value_type;
use crate::memory::{decode_memory, MemorySizer};
use crate::table::decode_table;
use razero::CoreFeatures;
use razero_wasm::module::{ExternType, GlobalType, Import, ImportDesc, Table};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecodedImports {
    pub imports: Vec<Import>,
    pub per_module: BTreeMap<String, Vec<usize>>,
    pub func_count: u32,
    pub global_count: u32,
    pub memory_count: u32,
    pub table_count: u32,
}

fn decode_global_type(decoder: &mut Decoder<'_>) -> DecodeResult<GlobalType> {
    let val_type = decode_value_type(
        decoder
            .read_byte()
            .map_err(|err| DecodeError::new(format!("read value type: {}", err.message)))?,
    )
    .map_err(|err| DecodeError::new(format!("read value type: {}", err.message)))?;

    let mutability = decoder
        .read_byte()
        .map_err(|err| DecodeError::new(format!("read mutablity: {}", err.message)))?;
    let mutable = match mutability {
        0x00 => false,
        0x01 => true,
        _ => {
            return Err(DecodeError::new(format!(
                "invalid byte for mutability: {mutability:#x} != 0x00 or 0x01"
            )))
        }
    };

    Ok(GlobalType { val_type, mutable })
}

pub fn decode_import(
    decoder: &mut Decoder<'_>,
    idx: u32,
    memory_sizer: MemorySizer,
    enabled_features: CoreFeatures,
) -> DecodeResult<Import> {
    let module = decoder
        .read_utf8("import module")
        .map(|pair| pair.0)
        .map_err(|err| {
            DecodeError::new(format!(
                "import[{idx}] error decoding module: {}",
                err.message
            ))
        })?;
    let name = decoder
        .read_utf8("import name")
        .map(|pair| pair.0)
        .map_err(|err| {
            DecodeError::new(format!(
                "import[{idx}] error decoding name: {}",
                err.message
            ))
        })?;

    let ty = ExternType(decoder.read_byte().map_err(|err| {
        DecodeError::new(format!(
            "import[{idx}] error decoding type: {}",
            err.message
        ))
    })?);

    let desc = match ty {
        ExternType::FUNC => decoder
            .read_var_u32("get type index")
            .map(ImportDesc::Func)
            .map_err(|err| DecodeError::new(err.message)),
        ExternType::TABLE => {
            let mut table = Table::default();
            decode_table(decoder, enabled_features, &mut table).map(|_| ImportDesc::Table(table))
        }
        ExternType::MEMORY => {
            decode_memory(decoder, enabled_features, memory_sizer).map(ImportDesc::Memory)
        }
        ExternType::GLOBAL => decode_global_type(decoder).map(ImportDesc::Global),
        _ => Err(DecodeError::new(format!(
            "invalid byte: invalid byte for importdesc: {:#x}",
            ty.0
        ))),
    }
    .map_err(|err| {
        DecodeError::new(format!(
            "import[{idx}] {}[{}.{}]: {}",
            ty.name(),
            module,
            name,
            err.message
        ))
    })?;

    Ok(Import {
        ty,
        module,
        name,
        desc,
        index_per_type: 0,
    })
}

pub fn decode_import_section(
    decoder: &mut Decoder<'_>,
    memory_sizer: MemorySizer,
    enabled_features: CoreFeatures,
) -> DecodeResult<DecodedImports> {
    let count = decoder
        .read_var_u32("get size of vector")
        .map_err(|err| DecodeError::new(err.message))?;

    let mut result = DecodedImports {
        imports: Vec::with_capacity(count as usize),
        per_module: BTreeMap::new(),
        ..DecodedImports::default()
    };

    for idx in 0..count {
        let mut import = decode_import(decoder, idx, memory_sizer, enabled_features)?;
        match import.ty {
            ExternType::FUNC => {
                import.index_per_type = result.func_count;
                result.func_count += 1;
            }
            ExternType::GLOBAL => {
                import.index_per_type = result.global_count;
                result.global_count += 1;
            }
            ExternType::MEMORY => {
                import.index_per_type = result.memory_count;
                result.memory_count += 1;
            }
            ExternType::TABLE => {
                import.index_per_type = result.table_count;
                result.table_count += 1;
            }
            _ => {}
        }

        let import_index = result.imports.len();
        result
            .per_module
            .entry(import.module.clone())
            .or_default()
            .push(import_index);
        result.imports.push(import);
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::decode_import_section;
    use crate::decoder::Decoder;
    use crate::memory::MemorySizer;
    use razero::CoreFeatures;
    use razero_wasm::module::{
        ExternType, GlobalType, Import, ImportDesc, Memory, RefType, Table, ValueType,
    };

    #[test]
    fn decodes_imports_counts_and_modules() {
        let mut decoder = Decoder::new(&[
            0x04,
            0x03,
            b'e',
            b'n',
            b'v',
            0x01,
            b'f',
            ExternType::FUNC.0,
            0x02,
            0x03,
            b'e',
            b'n',
            b'v',
            0x01,
            b't',
            ExternType::TABLE.0,
            RefType::EXTERNREF.0,
            0x01,
            0x02,
            0x03,
            0x04,
            b'h',
            b'o',
            b's',
            b't',
            0x01,
            b'm',
            ExternType::MEMORY.0,
            0x01,
            0x01,
            0x02,
            0x04,
            b'h',
            b'o',
            b's',
            b't',
            0x01,
            b'g',
            ExternType::GLOBAL.0,
            ValueType::F64.0,
            0x01,
        ]);

        let actual = decode_import_section(
            &mut decoder,
            MemorySizer::default(),
            CoreFeatures::REFERENCE_TYPES | CoreFeatures::THREADS,
        )
        .unwrap();

        assert_eq!(1, actual.func_count);
        assert_eq!(1, actual.table_count);
        assert_eq!(1, actual.memory_count);
        assert_eq!(1, actual.global_count);
        assert_eq!(Some(&vec![0, 1]), actual.per_module.get("env"));
        assert_eq!(Some(&vec![2, 3]), actual.per_module.get("host"));
        assert_eq!(
            vec![
                Import {
                    ty: ExternType::FUNC,
                    module: "env".to_string(),
                    name: "f".to_string(),
                    desc: ImportDesc::Func(2),
                    index_per_type: 0,
                },
                Import {
                    ty: ExternType::TABLE,
                    module: "env".to_string(),
                    name: "t".to_string(),
                    desc: ImportDesc::Table(Table {
                        min: 2,
                        max: Some(3),
                        ty: RefType::EXTERNREF,
                    }),
                    index_per_type: 0,
                },
                Import {
                    ty: ExternType::MEMORY,
                    module: "host".to_string(),
                    name: "m".to_string(),
                    desc: ImportDesc::Memory(Memory {
                        min: 1,
                        cap: 1,
                        max: 2,
                        is_max_encoded: true,
                        is_shared: false,
                    }),
                    index_per_type: 0,
                },
                Import {
                    ty: ExternType::GLOBAL,
                    module: "host".to_string(),
                    name: "g".to_string(),
                    desc: ImportDesc::Global(GlobalType {
                        val_type: ValueType::F64,
                        mutable: true,
                    }),
                    index_per_type: 0,
                },
            ],
            actual.imports
        );
    }

    #[test]
    fn wraps_global_type_errors_in_import_context() {
        let mut decoder = Decoder::new(&[
            0x01,
            0x03,
            b'e',
            b'n',
            b'v',
            0x01,
            b'g',
            ExternType::GLOBAL.0,
            ValueType::I32.0,
            0x02,
        ]);
        let err = decode_import_section(&mut decoder, MemorySizer::default(), CoreFeatures::all())
            .unwrap_err();

        assert_eq!(
            "import[0] global[env.g]: invalid byte for mutability: 0x2 != 0x00 or 0x01",
            err.message
        );
    }
}
