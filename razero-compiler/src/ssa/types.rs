use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Type {
    #[default]
    Invalid = 0,
    I32,
    I64,
    F32,
    F64,
    V128,
}

impl Type {
    pub const COUNT: usize = 6;

    pub const fn is_int(self) -> bool {
        matches!(self, Self::I32 | Self::I64)
    }

    pub const fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    pub const fn bits(self) -> u8 {
        match self {
            Self::I32 | Self::F32 => 32,
            Self::I64 | Self::F64 => 64,
            Self::V128 => 128,
            Self::Invalid => panic!("invalid type"),
        }
    }

    pub const fn size(self) -> u8 {
        self.bits() / 8
    }

    pub const fn is_valid(self) -> bool {
        !matches!(self, Self::Invalid)
    }

    pub const fn index(self) -> usize {
        self as usize
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::V128 => "v128",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum VecLane {
    Invalid = 1,
    I8x16,
    I16x8,
    I32x4,
    I64x2,
    F32x4,
    F64x2,
}

impl fmt::Display for VecLane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid",
            Self::I8x16 => "i8x16",
            Self::I16x8 => "i16x8",
            Self::I32x4 => "i32x4",
            Self::I64x2 => "i64x2",
            Self::F32x4 => "f32x4",
            Self::F64x2 => "f64x2",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Type, VecLane};

    #[test]
    fn type_helpers_match_go_model() {
        assert!(Type::I32.is_int());
        assert!(Type::F64.is_float());
        assert_eq!(Type::V128.bits(), 128);
        assert_eq!(Type::I64.size(), 8);
        assert!(!Type::Invalid.is_valid());
    }

    #[test]
    fn vec_lane_display_is_stable() {
        assert_eq!(VecLane::I8x16.to_string(), "i8x16");
        assert_eq!(VecLane::F64x2.to_string(), "f64x2");
    }
}
