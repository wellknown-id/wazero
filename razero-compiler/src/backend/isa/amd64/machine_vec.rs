use core::fmt;

use crate::backend::machine::BackendError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SseOpcode {
    Movss,
    Movsd,
    Movd,
    Movq,
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
    Xorps,
    Xorpd,
    Minps,
    Minpd,
    Maxps,
    Maxpd,
    Cvtss2sd,
    Cvtsd2ss,
    Cvtsi2ss,
    Cvtsi2sd,
    Cvttss2si,
    Cvttsd2si,
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
            Self::Movd => SseEncoding {
                prefix: Some(0x66),
                load_opcode: 0x0F6E,
                store_opcode: 0x0F7E,
                opcode_len: 2,
            },
            Self::Movq => SseEncoding {
                prefix: Some(0x66),
                load_opcode: 0x0F6E,
                store_opcode: 0x0FD6,
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
            Self::Xorps => SseEncoding {
                prefix: None,
                load_opcode: 0x0F57,
                store_opcode: 0x0F57,
                opcode_len: 2,
            },
            Self::Xorpd => SseEncoding {
                prefix: Some(0x66),
                load_opcode: 0x0F57,
                store_opcode: 0x0F57,
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
            Self::Cvttss2si => SseEncoding {
                prefix: Some(0xF3),
                load_opcode: 0x0F2C,
                store_opcode: 0x0F2C,
                opcode_len: 2,
            },
            Self::Cvttsd2si => SseEncoding {
                prefix: Some(0xF2),
                load_opcode: 0x0F2C,
                store_opcode: 0x0F2C,
                opcode_len: 2,
            },
        }
    }

    pub const fn from_u64(raw: u64) -> Self {
        match raw {
            0 => Self::Movss,
            1 => Self::Movsd,
            2 => Self::Movd,
            3 => Self::Movq,
            4 => Self::Movdqu,
            5 => Self::Addss,
            6 => Self::Addsd,
            7 => Self::Subss,
            8 => Self::Subsd,
            9 => Self::Mulss,
            10 => Self::Mulsd,
            11 => Self::Divss,
            12 => Self::Divsd,
            13 => Self::Ucomiss,
            14 => Self::Ucomisd,
            15 => Self::Sqrtss,
            16 => Self::Sqrtsd,
            17 => Self::Roundss,
            18 => Self::Roundsd,
            19 => Self::Andps,
            20 => Self::Andpd,
            21 => Self::Orps,
            22 => Self::Orpd,
            23 => Self::Xorps,
            24 => Self::Xorpd,
            25 => Self::Minps,
            26 => Self::Minpd,
            27 => Self::Maxps,
            28 => Self::Maxpd,
            29 => Self::Cvtss2sd,
            30 => Self::Cvtsd2ss,
            31 => Self::Cvtsi2ss,
            32 => Self::Cvtsi2sd,
            33 => Self::Cvttss2si,
            _ => Self::Cvttsd2si,
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
            Self::Movd => "movd",
            Self::Movq => "movq",
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
            Self::Xorps => "xorps",
            Self::Xorpd => "xorpd",
            Self::Minps => "minps",
            Self::Minpd => "minpd",
            Self::Maxps => "maxps",
            Self::Maxpd => "maxpd",
            Self::Cvtss2sd => "cvtss2sd",
            Self::Cvtsd2ss => "cvtsd2ss",
            Self::Cvtsi2ss => "cvtsi2ss",
            Self::Cvtsi2sd => "cvtsi2sd",
            Self::Cvttss2si => "cvttss2si",
            Self::Cvttsd2si => "cvttsd2si",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::SseOpcode;

    #[test]
    fn sse_encodings_are_stable() {
        let movsd = SseOpcode::Movsd.encoding();
        let movd = SseOpcode::Movd.encoding();
        let movq = SseOpcode::Movq.encoding();
        let addss = SseOpcode::Addss.encoding();
        let ucomisd = SseOpcode::Ucomisd.encoding();
        let sqrtsd = SseOpcode::Sqrtsd.encoding();
        let roundss = SseOpcode::Roundss.encoding();
        let minpd = SseOpcode::Minpd.encoding();
        let cvtss2sd = SseOpcode::Cvtss2sd.encoding();
        let cvtsi2sd = SseOpcode::Cvtsi2sd.encoding();
        let cvttsd2si = SseOpcode::Cvttsd2si.encoding();
        assert_eq!(movsd.prefix, Some(0xF2));
        assert_eq!(movsd.load_opcode, 0x0F10);
        assert_eq!(movd.load_opcode, 0x0F6E);
        assert_eq!(movq.store_opcode, 0x0FD6);
        assert_eq!(addss.prefix, Some(0xF3));
        assert_eq!(addss.load_opcode, 0x0F58);
        assert_eq!(ucomisd.load_opcode, 0x0F2E);
        assert_eq!(sqrtsd.load_opcode, 0x0F51);
        assert_eq!(roundss.load_opcode, 0x0F3A0A);
        assert_eq!(minpd.prefix, Some(0x66));
        assert_eq!(minpd.load_opcode, 0x0F5D);
        assert_eq!(SseOpcode::Xorps.encoding().load_opcode, 0x0F57);
        assert_eq!(SseOpcode::Xorpd.encoding().prefix, Some(0x66));
        assert_eq!(cvtss2sd.load_opcode, 0x0F5A);
        assert_eq!(cvtsi2sd.load_opcode, 0x0F2A);
        assert_eq!(cvttsd2si.load_opcode, 0x0F2C);
        assert_eq!(SseOpcode::Movdqu.to_string(), "movdqu");
        assert_eq!(SseOpcode::Movd.to_string(), "movd");
        assert_eq!(SseOpcode::Movq.to_string(), "movq");
        assert_eq!(SseOpcode::Divsd.to_string(), "divsd");
        assert_eq!(SseOpcode::Ucomiss.to_string(), "ucomiss");
        assert_eq!(SseOpcode::Sqrtss.to_string(), "sqrtss");
        assert_eq!(SseOpcode::Roundsd.to_string(), "roundsd");
        assert_eq!(SseOpcode::Xorps.to_string(), "xorps");
        assert_eq!(SseOpcode::Xorpd.to_string(), "xorpd");
        assert_eq!(SseOpcode::Maxps.to_string(), "maxps");
        assert_eq!(SseOpcode::Cvtsd2ss.to_string(), "cvtsd2ss");
        assert_eq!(SseOpcode::Cvtsi2ss.to_string(), "cvtsi2ss");
        assert_eq!(SseOpcode::Cvttss2si.to_string(), "cvttss2si");
    }
}
