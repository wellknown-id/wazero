use core::fmt;

use crate::ssa::{FloatCmpCond, IntegerCmpCond};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Cond {
    O = 0,
    NO,
    B,
    NB,
    Z,
    NZ,
    BE,
    NBE,
    S,
    NS,
    P,
    NP,
    L,
    NL,
    LE,
    NLE,
}

impl Cond {
    pub const fn encoding(self) -> u8 {
        self as u8
    }

    pub const fn invert(self) -> Self {
        match self {
            Self::O => Self::NO,
            Self::NO => Self::O,
            Self::B => Self::NB,
            Self::NB => Self::B,
            Self::Z => Self::NZ,
            Self::NZ => Self::Z,
            Self::BE => Self::NBE,
            Self::NBE => Self::BE,
            Self::S => Self::NS,
            Self::NS => Self::S,
            Self::P => Self::NP,
            Self::NP => Self::P,
            Self::L => Self::NL,
            Self::NL => Self::L,
            Self::LE => Self::NLE,
            Self::NLE => Self::LE,
        }
    }

    pub fn from_int_cmp(origin: IntegerCmpCond) -> Self {
        match origin {
            IntegerCmpCond::Equal => Self::Z,
            IntegerCmpCond::NotEqual => Self::NZ,
            IntegerCmpCond::SignedLessThan => Self::L,
            IntegerCmpCond::SignedGreaterThanOrEqual => Self::NL,
            IntegerCmpCond::SignedGreaterThan => Self::NLE,
            IntegerCmpCond::SignedLessThanOrEqual => Self::LE,
            IntegerCmpCond::UnsignedLessThan => Self::B,
            IntegerCmpCond::UnsignedGreaterThanOrEqual => Self::NB,
            IntegerCmpCond::UnsignedGreaterThan => Self::NBE,
            IntegerCmpCond::UnsignedLessThanOrEqual => Self::BE,
            IntegerCmpCond::Invalid => panic!("invalid integer comparison"),
        }
    }

    pub fn from_float_cmp(origin: FloatCmpCond) -> Self {
        match origin {
            FloatCmpCond::GreaterThanOrEqual => Self::NB,
            FloatCmpCond::GreaterThan => Self::NBE,
            FloatCmpCond::Equal
            | FloatCmpCond::NotEqual
            | FloatCmpCond::LessThan
            | FloatCmpCond::LessThanOrEqual => {
                panic!("float comparison {origin} must be handled specially")
            }
            FloatCmpCond::Invalid => panic!("invalid float comparison"),
        }
    }
}

impl fmt::Display for Cond {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::O => "o",
            Self::NO => "no",
            Self::B => "b",
            Self::NB => "nb",
            Self::Z => "z",
            Self::NZ => "nz",
            Self::BE => "be",
            Self::NBE => "nbe",
            Self::S => "s",
            Self::NS => "ns",
            Self::P => "p",
            Self::NP => "np",
            Self::L => "l",
            Self::NL => "nl",
            Self::LE => "le",
            Self::NLE => "nle",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Cond;
    use crate::ssa::{FloatCmpCond, IntegerCmpCond};

    #[test]
    fn invert_round_trips() {
        assert_eq!(Cond::B.invert(), Cond::NB);
        assert_eq!(Cond::NB.invert(), Cond::B);
        assert_eq!(Cond::LE.invert().invert(), Cond::LE);
    }

    #[test]
    fn ssa_mappings_match_go() {
        assert_eq!(
            Cond::from_int_cmp(IntegerCmpCond::UnsignedLessThan),
            Cond::B
        );
        assert_eq!(
            Cond::from_int_cmp(IntegerCmpCond::SignedGreaterThan),
            Cond::NLE
        );
        assert_eq!(Cond::from_float_cmp(FloatCmpCond::GreaterThan), Cond::NBE);
    }
}
