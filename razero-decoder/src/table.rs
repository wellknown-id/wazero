#![doc = "Table type and section decoding."]

use crate::decoder::Decoder;
use crate::errors::{DecodeError, DecodeResult};
use crate::memory::decode_limits_type;
use razero_features::CoreFeatures;
use razero_wasm::module::{RefType, Table, MAXIMUM_FUNCTION_INDEX};

pub fn decode_table(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
    ret: &mut Table,
) -> DecodeResult<()> {
    ret.ty = RefType(
        decoder
            .read_byte()
            .map_err(|err| DecodeError::new(format!("read leading byte: {}", err.message)))?,
    );

    if ret.ty != RefType::FUNCREF && !enabled_features.contains(CoreFeatures::REFERENCE_TYPES) {
        return Err(DecodeError::new(
            "table type funcref is invalid: feature \"reference-types\" is disabled",
        ));
    }

    let (min, max, shared) = decode_limits_type(decoder)
        .map_err(|err| DecodeError::new(format!("read limits: {}", err.message)))?;
    ret.min = min;
    ret.max = max;

    if ret.min > MAXIMUM_FUNCTION_INDEX {
        return Err(DecodeError::new(format!(
            "table min must be at most {MAXIMUM_FUNCTION_INDEX}"
        )));
    }
    if ret.max.is_some_and(|max| max < ret.min) {
        return Err(DecodeError::new(
            "table size minimum must not be greater than maximum",
        ));
    }
    if shared {
        return Err(DecodeError::new("tables cannot be marked as shared"));
    }
    Ok(())
}

pub fn decode_table_section(
    decoder: &mut Decoder<'_>,
    enabled_features: CoreFeatures,
) -> DecodeResult<Vec<Table>> {
    let count = decoder
        .read_var_u32("error reading size")
        .map_err(|err| DecodeError::new(err.message))?;
    if count > 1 && !enabled_features.contains(CoreFeatures::REFERENCE_TYPES) {
        return Err(DecodeError::new(
            "at most one table allowed in module as feature \"reference-types\" is disabled",
        ));
    }

    let mut tables = vec![Table::default(); count as usize];
    for table in &mut tables {
        decode_table(decoder, enabled_features, table)?;
    }
    Ok(tables)
}

#[cfg(test)]
mod tests {
    use super::{decode_table, decode_table_section};
    use crate::decoder::Decoder;
    use razero_features::CoreFeatures;
    use razero_wasm::module::{RefType, Table};

    #[test]
    fn decodes_table() {
        let mut decoder = Decoder::new(&[RefType::EXTERNREF.0, 0x01, 0x02, 0x03]);
        let mut actual = Table::default();

        decode_table(&mut decoder, CoreFeatures::REFERENCE_TYPES, &mut actual).unwrap();

        assert_eq!(
            Table {
                min: 2,
                max: Some(3),
                ty: RefType::EXTERNREF,
            },
            actual
        );
    }

    #[test]
    fn rejects_reference_tables_when_feature_disabled() {
        let mut decoder = Decoder::new(&[RefType::EXTERNREF.0, 0x00, 0x00]);
        let err =
            decode_table(&mut decoder, CoreFeatures::empty(), &mut Table::default()).unwrap_err();

        assert_eq!(
            "table type funcref is invalid: feature \"reference-types\" is disabled",
            err.message
        );
    }

    #[test]
    fn rejects_shared_tables() {
        let mut decoder = Decoder::new(&[RefType::FUNCREF.0, 0x02, 0x00]);
        let err =
            decode_table(&mut decoder, CoreFeatures::all(), &mut Table::default()).unwrap_err();

        assert_eq!("tables cannot be marked as shared", err.message);
    }

    #[test]
    fn section_enforces_single_table_without_reference_types() {
        let mut decoder = Decoder::new(&[
            0x02,
            RefType::FUNCREF.0,
            0x00,
            0x01,
            RefType::FUNCREF.0,
            0x01,
            0x02,
            0x03,
        ]);
        let err = decode_table_section(&mut decoder, CoreFeatures::empty()).unwrap_err();

        assert_eq!(
            "at most one table allowed in module as feature \"reference-types\" is disabled",
            err.message
        );
    }
}
