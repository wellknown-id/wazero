use core::fmt;

use crate::backend::machine::BackendError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SseOpcode {
    Movss,
    Movsd,
    Movdqu,
    Addss,
    Addsd,
    Subss,
    Subsd,
    Mulss,
    Mulsd,
    Divss,
    Divsd,
    Ucomiss,
    Ucomisd,
    Sqrtss,
    Sqrtsd,
    Roundss,
    Roundsd,
    Andps,
    Andpd,
    Orps,
    Orpd,
    Minps,
    Minpd,
    Maxps,
    Maxpd,
    Cvtss2sd,
    Cvtsd2ss,
    Cvtsi2ss,
    Cvtsi2sd,
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
            Self::Addss => SseEncoding {
                prefix: Some(0xF3),
                load_opcode: 0x0F58,
                store_opcode: 0x0F58,
                opcode_len: 2,
            },
            Self::Addsd => SseEncoding {
                prefix: Some(0xF2),
                load_opcode: 0x0F58,
                store_opcode: 0x0F58,
                opcode_len: 2,
            },
            Self::Subss => SseEncoding {
                prefix: Some(0xF3),
                load_opcode: 0x0F5C,
                store_opcode: 0x0F5C,
                opcode_len: 2,
            },
            Self::Subsd => SseEncoding {
                prefix: Some(0xF2),
                load_opcode: 0x0F5C,
                store_opcode: 0x0F5C,
                opcode_len: 2,
            },
            Self::Mulss => SseEncoding {
                prefix: Some(0xF3),
                load_opcode: 0x0F59,
                store_opcode: 0x0F59,
                opcode_len: 2,
            },
            Self::Mulsd => SseEncoding {
                prefix: Some(0xF2),
                load_opcode: 0x0F59,
                store_opcode: 0x0F59,
                opcode_len: 2,
            },
            Self::Divss => SseEncoding {
                prefix: Some(0xF3),
                load_opcode: 0x0F5E,
                store_opcode: 0x0F5E,
                opcode_len: 2,
            },
            Self::Divsd => SseEncoding {
                prefix: Some(0xF2),
                load_opcode: 0x0F5E,
                store_opcode: 0x0F5E,
                opcode_len: 2,
            },
            Self::Ucomiss => SseEncoding {
                prefix: Some(0xF3),
                load_opcode: 0x0F2E,
                store_opcode: 0x0F2E,
                opcode_len: 2,
            },
            Self::Ucomisd => SseEncoding {
                prefix: Some(0xF2),
                load_opcode: 0x0F2E,
                store_opcode: 0x0F2E,
                opcode_len: 2,
            },
            Self::Sqrtss => SseEncoding {
                prefix: Some(0xF3),
                load_opcode: 0x0F51,
                store_opcode: 0x0F51,
                opcode_len: 2,
            },
            Self::Sqrtsd => SseEncoding {
                prefix: Some(0xF2),
                load_opcode: 0x0F51,
                store_opcode: 0x0F51,
                opcode_len: 2,
            },
            Self::Roundss => SseEncoding {
                prefix: Some(0xF3),
                load_opcode: 0x0F3A0A,
                store_opcode: 0x0F3A0A,
                opcode_len: 3,
            },
            Self::Roundsd => SseEncoding {
                prefix: Some(0xF2),
                load_opcode: 0x0F3A0A,
                store_opcode: 0x0F3A0A,
                opcode_len: 3,
            },
            Self::Andps => SseEncoding {
                prefix: None,
                load_opcode: 0x0F54,
                store_opcode: 0x0F54,
                opcode_len: 2,
            },
            Self::Andpd => SseEncoding {
                prefix: Some(0x66),
                load_opcode: 0x0F54,
                store_opcode: 0x0F54,
                opcode_len: 2,
            },
            Self::Orps => SseEncoding {
                prefix: None,
                load_opcode: 0x0F56,
                store_opcode: 0x0F56,
                opcode_len: 2,
            },
            Self::Orpd => SseEncoding {
                prefix: Some(0x66),
                load_opcode: 0x0F56,
                store_opcode: 0x0F56,
                opcode_len: 2,
            },
            Self::Minps => SseEncoding {
                prefix: None,
                load_opcode: 0x0F5D,
                store_opcode: 0x0F5D,
                opcode_len: 2,
            },
            Self::Minpd => SseEncoding {
                prefix: Some(0x66),
                load_opcode: 0x0F5D,
                store_opcode: 0x0F5D,
                opcode_len: 2,
            },
            Self::Maxps => SseEncoding {
                prefix: None,
                load_opcode: 0x0F5F,
                store_opcode: 0x0F5F,
                opcode_len: 2,
            },
            Self::Maxpd => SseEncoding {
                prefix: Some(0x66),
                load_opcode: 0x0F5F,
                store_opcode: 0x0F5F,
                opcode_len: 2,
            },
            Self::Cvtss2sd => SseEncoding {
                prefix: Some(0xF3),
                load_opcode: 0x0F5A,
                store_opcode: 0x0F5A,
                opcode_len: 2,
            },
            Self::Cvtsd2ss => SseEncoding {
                prefix: Some(0xF2),
                load_opcode: 0x0F5A,
                store_opcode: 0x0F5A,
                opcode_len: 2,
            },
            Self::Cvtsi2ss => SseEncoding {
                prefix: Some(0xF3),
                load_opcode: 0x0F2A,
                store_opcode: 0x0F2A,
                opcode_len: 2,
            },
            Self::Cvtsi2sd => SseEncoding {
                prefix: Some(0xF2),
                load_opcode: 0x0F2A,
                store_opcode: 0x0F2A,
                opcode_len: 2,
            },
        }
    }

    pub const fn from_u64(raw: u64) -> Self {
        match raw {
            0 => Self::Movss,
            1 => Self::Movsd,
            2 => Self::Movdqu,
            3 => Self::Addss,
            4 => Self::Addsd,
            5 => Self::Subss,
            6 => Self::Subsd,
            7 => Self::Mulss,
            8 => Self::Mulsd,
            9 => Self::Divss,
            10 => Self::Divsd,
            11 => Self::Ucomiss,
            12 => Self::Ucomisd,
            13 => Self::Sqrtss,
            14 => Self::Sqrtsd,
            15 => Self::Roundss,
            16 => Self::Roundsd,
            17 => Self::Andps,
            18 => Self::Andpd,
            19 => Self::Orps,
            20 => Self::Orpd,
            21 => Self::Minps,
            22 => Self::Minpd,
            23 => Self::Maxps,
            24 => Self::Maxpd,
            25 => Self::Cvtss2sd,
            26 => Self::Cvtsd2ss,
            27 => Self::Cvtsi2ss,
            _ => Self::Cvtsi2sd,
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
            Self::Addss => "addss",
            Self::Addsd => "addsd",
            Self::Subss => "subss",
            Self::Subsd => "subsd",
            Self::Mulss => "mulss",
            Self::Mulsd => "mulsd",
            Self::Divss => "divss",
            Self::Divsd => "divsd",
            Self::Ucomiss => "ucomiss",
            Self::Ucomisd => "ucomisd",
            Self::Sqrtss => "sqrtss",
            Self::Sqrtsd => "sqrtsd",
            Self::Roundss => "roundss",
            Self::Roundsd => "roundsd",
            Self::Andps => "andps",
            Self::Andpd => "andpd",
            Self::Orps => "orps",
            Self::Orpd => "orpd",
            Self::Minps => "minps",
            Self::Minpd => "minpd",
            Self::Maxps => "maxps",
            Self::Maxpd => "maxpd",
            Self::Cvtss2sd => "cvtss2sd",
            Self::Cvtsd2ss => "cvtsd2ss",
            Self::Cvtsi2ss => "cvtsi2ss",
            Self::Cvtsi2sd => "cvtsi2sd",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::SseOpcode;

    #[test]
    fn sse_encodings_are_stable() {
        let movsd = SseOpcode::Movsd.encoding();
        let addss = SseOpcode::Addss.encoding();
        let ucomisd = SseOpcode::Ucomisd.encoding();
        let sqrtsd = SseOpcode::Sqrtsd.encoding();
        let roundss = SseOpcode::Roundss.encoding();
        let minpd = SseOpcode::Minpd.encoding();
        let cvtss2sd = SseOpcode::Cvtss2sd.encoding();
        let cvtsi2sd = SseOpcode::Cvtsi2sd.encoding();
        assert_eq!(movsd.prefix, Some(0xF2));
        assert_eq!(movsd.load_opcode, 0x0F10);
        assert_eq!(addss.prefix, Some(0xF3));
        assert_eq!(addss.load_opcode, 0x0F58);
        assert_eq!(ucomisd.load_opcode, 0x0F2E);
        assert_eq!(sqrtsd.load_opcode, 0x0F51);
        assert_eq!(roundss.load_opcode, 0x0F3A0A);
        assert_eq!(minpd.prefix, Some(0x66));
        assert_eq!(minpd.load_opcode, 0x0F5D);
        assert_eq!(cvtss2sd.load_opcode, 0x0F5A);
        assert_eq!(cvtsi2sd.load_opcode, 0x0F2A);
        assert_eq!(SseOpcode::Movdqu.to_string(), "movdqu");
        assert_eq!(SseOpcode::Divsd.to_string(), "divsd");
        assert_eq!(SseOpcode::Ucomiss.to_string(), "ucomiss");
        assert_eq!(SseOpcode::Sqrtss.to_string(), "sqrtss");
        assert_eq!(SseOpcode::Roundsd.to_string(), "roundsd");
        assert_eq!(SseOpcode::Maxps.to_string(), "maxps");
        assert_eq!(SseOpcode::Cvtsd2ss.to_string(), "cvtsd2ss");
        assert_eq!(SseOpcode::Cvtsi2ss.to_string(), "cvtsi2ss");
    }
}
