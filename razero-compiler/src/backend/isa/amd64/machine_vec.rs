use core::fmt;

use crate::backend::machine::BackendError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SseOpcode {
    Movss,
    Movsd,
    Movdqu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SseEncoding {
    pub prefix: Option<u8>,
    pub load_opcode: u32,
    pub store_opcode: u32,
    pub opcode_len: usize,
}

impl SseOpcode {
    pub const fn encoding(self) -> SseEncoding {
        match self {
            Self::Movss => SseEncoding {
                prefix: Some(0xF3),
                load_opcode: 0x0F10,
                store_opcode: 0x0F11,
                opcode_len: 2,
            },
            Self::Movsd => SseEncoding {
                prefix: Some(0xF2),
                load_opcode: 0x0F10,
                store_opcode: 0x0F11,
                opcode_len: 2,
            },
            Self::Movdqu => SseEncoding {
                prefix: Some(0xF3),
                load_opcode: 0x0F6F,
                store_opcode: 0x0F7F,
                opcode_len: 2,
            },
        }
    }

    pub fn ensure_supported(self) -> Result<Self, BackendError> {
        Ok(self)
    }
}

impl fmt::Display for SseOpcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Movss => "movss",
            Self::Movsd => "movsd",
            Self::Movdqu => "movdqu",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::SseOpcode;

    #[test]
    fn sse_encodings_are_stable() {
        let movsd = SseOpcode::Movsd.encoding();
        assert_eq!(movsd.prefix, Some(0xF2));
        assert_eq!(movsd.load_opcode, 0x0F10);
        assert_eq!(SseOpcode::Movdqu.to_string(), "movdqu");
    }
}
