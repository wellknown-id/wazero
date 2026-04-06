#![doc = "Wasm header parsing."]

use crate::errors::{DecodeError, DecodeResult};

pub const WASM_MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6d];
pub const WASM_VERSION: [u8; 4] = [0x01, 0x00, 0x00, 0x00];
pub const WASM_HEADER_LEN: usize = WASM_MAGIC.len() + WASM_VERSION.len();

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Header;

impl Header {
    pub fn decode(bytes: &[u8]) -> DecodeResult<Self> {
        validate_header(bytes)?;
        Ok(Self)
    }
}

pub fn validate_header(bytes: &[u8]) -> DecodeResult<()> {
    if bytes.get(..WASM_MAGIC.len()) != Some(WASM_MAGIC.as_slice()) {
        return Err(DecodeError::new("invalid magic number"));
    }

    let version_start = WASM_MAGIC.len();
    let version_end = version_start + WASM_VERSION.len();
    if bytes.get(version_start..version_end) != Some(WASM_VERSION.as_slice()) {
        return Err(DecodeError::new("invalid version header"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_header, Header, WASM_HEADER_LEN, WASM_MAGIC, WASM_VERSION};

    #[test]
    fn decodes_valid_header() {
        let mut bytes = Vec::from(WASM_MAGIC);
        bytes.extend_from_slice(&WASM_VERSION);

        assert_eq!(Ok(Header), Header::decode(&bytes));
        assert!(validate_header(&bytes).is_ok());
        assert_eq!(WASM_HEADER_LEN, bytes.len());
    }

    #[test]
    fn rejects_invalid_magic() {
        let err = Header::decode(b"wasm\x01\x00\x00\x00").unwrap_err();
        assert_eq!("invalid magic number", err.message);
    }

    #[test]
    fn rejects_invalid_version() {
        let err = Header::decode(b"\0asm\x01\x00\x00\x01").unwrap_err();
        assert_eq!("invalid version header", err.message);
    }
}
