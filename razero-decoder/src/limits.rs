#![doc = "Limits decoding."]

use crate::const_expr::read_var_u32_raw;
use crate::decoder::Decoder;
use crate::errors::{DecodeError, DecodeResult, ERR_INVALID_BYTE};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    pub min: u32,
    pub max: Option<u32>,
    pub shared: bool,
}

pub fn decode_limits_type(decoder: &mut Decoder<'_>) -> DecodeResult<Limits> {
    let flag = decoder
        .read_byte()
        .map_err(|err| DecodeError::new(format!("read leading byte: {}", err.message)))?;

    let mut ret = Limits::default();
    match flag {
        0x00 | 0x02 => {
            ret.min = read_var_u32_raw(decoder, "read min of limit")?.0;
        }
        0x01 | 0x03 => {
            ret.min = read_var_u32_raw(decoder, "read min of limit")?.0;
            ret.max = Some(read_var_u32_raw(decoder, "read max of limit")?.0);
        }
        _ => {
            return Err(DecodeError::new(format!(
                "{ERR_INVALID_BYTE} for limits: {flag:#x} not in (0x00, 0x01, 0x02, 0x03)"
            )));
        }
    }

    ret.shared = matches!(flag, 0x02 | 0x03);
    Ok(ret)
}

#[cfg(test)]
mod tests {
    use super::{decode_limits_type, Limits};
    use crate::decoder::Decoder;

    #[test]
    fn decodes_shared_limits_with_max() {
        let mut decoder = Decoder::new(&[0x03, 0x01, 0x02]);
        assert_eq!(
            Limits {
                min: 1,
                max: Some(2),
                shared: true,
            },
            decode_limits_type(&mut decoder).unwrap()
        );
    }

    #[test]
    fn rejects_unknown_limit_flag() {
        let mut decoder = Decoder::new(&[0x04]);
        let err = decode_limits_type(&mut decoder).unwrap_err();
        assert_eq!(
            "invalid byte for limits: 0x4 not in (0x00, 0x01, 0x02, 0x03)",
            err.message
        );
    }
}
