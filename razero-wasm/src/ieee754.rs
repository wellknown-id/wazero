use std::error::Error;
use std::fmt::{Display, Formatter};

pub const F32_CANONICAL_NAN_BITS: u32 = 0x7fc0_0000;
pub const F64_CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    UnexpectedEof { expected: usize, actual: usize },
}

impl Display for DecodeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedEof { expected, actual } => {
                write!(f, "expected at least {expected} bytes, got {actual}")
            }
        }
    }
}

impl Error for DecodeError {}

pub fn decode_float32(buf: &[u8]) -> Result<f32, DecodeError> {
    if buf.len() < 4 {
        return Err(DecodeError::UnexpectedEof {
            expected: 4,
            actual: buf.len(),
        });
    }

    let mut raw = [0_u8; 4];
    raw.copy_from_slice(&buf[..4]);
    Ok(f32::from_bits(u32::from_le_bytes(raw)))
}

pub fn decode_float64(buf: &[u8]) -> Result<f64, DecodeError> {
    if buf.len() < 8 {
        return Err(DecodeError::UnexpectedEof {
            expected: 8,
            actual: buf.len(),
        });
    }

    let mut raw = [0_u8; 8];
    raw.copy_from_slice(&buf[..8]);
    Ok(f64::from_bits(u64::from_le_bytes(raw)))
}

pub fn canonicalize_f32_nan(value: f32) -> f32 {
    if value.is_nan() {
        f32::from_bits(F32_CANONICAL_NAN_BITS)
    } else {
        value
    }
}

pub fn canonicalize_f64_nan(value: f64) -> f64 {
    if value.is_nan() {
        f64::from_bits(F64_CANONICAL_NAN_BITS)
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_float32_reads_little_endian() {
        let value = decode_float32(&[0xdb, 0x0f, 0x49, 0x40, 0xff]).unwrap();
        assert_eq!(value.to_bits(), 0x4049_0fdb);
    }

    #[test]
    fn decode_float64_reads_little_endian() {
        let value =
            decode_float64(&[0x18, 0x2d, 0x44, 0x54, 0xfb, 0x21, 0x09, 0x40, 0xff]).unwrap();
        assert_eq!(value.to_bits(), 0x4009_21fb_5444_2d18);
    }

    #[test]
    fn decode_float32_rejects_short_input() {
        assert_eq!(
            decode_float32(&[0, 1, 2]).unwrap_err(),
            DecodeError::UnexpectedEof {
                expected: 4,
                actual: 3,
            }
        );
    }

    #[test]
    fn decode_float64_rejects_short_input() {
        assert_eq!(
            decode_float64(&[0, 1, 2, 3, 4, 5, 6]).unwrap_err(),
            DecodeError::UnexpectedEof {
                expected: 8,
                actual: 7,
            }
        );
    }

    #[test]
    fn canonicalize_f32_nan_uses_canonical_bits() {
        let arithmetic = f32::from_bits(0x7fc0_0001);
        assert_eq!(
            canonicalize_f32_nan(arithmetic).to_bits(),
            F32_CANONICAL_NAN_BITS
        );
        assert_eq!(canonicalize_f32_nan(-1.25).to_bits(), (-1.25_f32).to_bits());
    }

    #[test]
    fn canonicalize_f64_nan_uses_canonical_bits() {
        let arithmetic = f64::from_bits(0x7ff8_0000_0000_0001);
        assert_eq!(
            canonicalize_f64_nan(arithmetic).to_bits(),
            F64_CANONICAL_NAN_BITS
        );
        assert_eq!(canonicalize_f64_nan(-1.25).to_bits(), (-1.25_f64).to_bits());
    }
}
