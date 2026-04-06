use core::fmt;

use crate::backend::VReg;
use crate::ssa::{FloatCmpCond, IntegerCmpCond};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CondKind {
    RegisterZero = 0,
    RegisterNotZero = 1,
    CondFlagSet = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CondFlag {
    Eq = 0,
    Ne,
    Hs,
    Lo,
    Mi,
    Pl,
    Vs,
    Vc,
    Hi,
    Ls,
    Ge,
    Lt,
    Gt,
    Le,
    Al,
    Nv,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Cond(u64);

impl Cond {
    pub const fn kind(self) -> CondKind {
        match (self.0 & 0b11) as u8 {
            0 => CondKind::RegisterZero,
            1 => CondKind::RegisterNotZero,
            _ => CondKind::CondFlagSet,
        }
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn register(self) -> VReg {
        VReg(self.0 >> 2)
    }

    pub const fn flag(self) -> CondFlag {
        match (self.0 >> 2) as u8 {
            0 => CondFlag::Eq,
            1 => CondFlag::Ne,
            2 => CondFlag::Hs,
            3 => CondFlag::Lo,
            4 => CondFlag::Mi,
            5 => CondFlag::Pl,
            6 => CondFlag::Vs,
            7 => CondFlag::Vc,
            8 => CondFlag::Hi,
            9 => CondFlag::Ls,
            10 => CondFlag::Ge,
            11 => CondFlag::Lt,
            12 => CondFlag::Gt,
            13 => CondFlag::Le,
            14 => CondFlag::Al,
            _ => CondFlag::Nv,
        }
    }

    pub const fn from_reg_zero(reg: VReg) -> Self {
        Self((reg.0 << 2) | CondKind::RegisterZero as u64)
    }

    pub const fn from_reg_not_zero(reg: VReg) -> Self {
        Self((reg.0 << 2) | CondKind::RegisterNotZero as u64)
    }

    pub const fn from_flag(flag: CondFlag) -> Self {
        Self(((flag as u64) << 2) | CondKind::CondFlagSet as u64)
    }
}

impl CondFlag {
    pub const fn invert(self) -> Self {
        match self {
            Self::Eq => Self::Ne,
            Self::Ne => Self::Eq,
            Self::Hs => Self::Lo,
            Self::Lo => Self::Hs,
            Self::Mi => Self::Pl,
            Self::Pl => Self::Mi,
            Self::Vs => Self::Vc,
            Self::Vc => Self::Vs,
            Self::Hi => Self::Ls,
            Self::Ls => Self::Hi,
            Self::Ge => Self::Lt,
            Self::Lt => Self::Ge,
            Self::Gt => Self::Le,
            Self::Le => Self::Gt,
            Self::Al => Self::Nv,
            Self::Nv => Self::Al,
        }
    }

    pub const fn from_ssa_integer_cmp(cond: IntegerCmpCond) -> Self {
        match cond {
            IntegerCmpCond::Equal => Self::Eq,
            IntegerCmpCond::NotEqual => Self::Ne,
            IntegerCmpCond::SignedLessThan => Self::Lt,
            IntegerCmpCond::SignedGreaterThanOrEqual => Self::Ge,
            IntegerCmpCond::SignedGreaterThan => Self::Gt,
            IntegerCmpCond::SignedLessThanOrEqual => Self::Le,
            IntegerCmpCond::UnsignedLessThan => Self::Lo,
            IntegerCmpCond::UnsignedGreaterThanOrEqual => Self::Hs,
            IntegerCmpCond::UnsignedGreaterThan => Self::Hi,
            IntegerCmpCond::UnsignedLessThanOrEqual => Self::Ls,
            IntegerCmpCond::Invalid => panic!("invalid integer comparison condition"),
        }
    }

    pub const fn from_ssa_float_cmp(cond: FloatCmpCond) -> Self {
        match cond {
            FloatCmpCond::Equal => Self::Eq,
            FloatCmpCond::NotEqual => Self::Ne,
            FloatCmpCond::LessThan => Self::Mi,
            FloatCmpCond::LessThanOrEqual => Self::Ls,
            FloatCmpCond::GreaterThan => Self::Gt,
            FloatCmpCond::GreaterThanOrEqual => Self::Ge,
            FloatCmpCond::Invalid => panic!("invalid float comparison condition"),
        }
    }
}

impl fmt::Display for CondFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Hs => "hs",
            Self::Lo => "lo",
            Self::Mi => "mi",
            Self::Pl => "pl",
            Self::Vs => "vs",
            Self::Vc => "vc",
            Self::Hi => "hi",
            Self::Ls => "ls",
            Self::Ge => "ge",
            Self::Lt => "lt",
            Self::Gt => "gt",
            Self::Le => "le",
            Self::Al => "al",
            Self::Nv => "nv",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Cond, CondFlag, CondKind};
    use crate::backend::{RegType, VReg};
    use crate::ssa::{FloatCmpCond, IntegerCmpCond};

    #[test]
    fn condition_packing_matches_go_layout() {
        let reg = VReg(130).set_reg_type(RegType::Int);
        let zero = Cond::from_reg_zero(reg);
        let non_zero = Cond::from_reg_not_zero(reg);
        let flag = Cond::from_flag(CondFlag::Ge);

        assert_eq!(zero.kind(), CondKind::RegisterZero);
        assert_eq!(zero.register(), reg);
        assert_eq!(non_zero.kind(), CondKind::RegisterNotZero);
        assert_eq!(flag.kind(), CondKind::CondFlagSet);
        assert_eq!(flag.flag(), CondFlag::Ge);
    }

    #[test]
    fn condition_inversion_matches_arm64_table() {
        assert_eq!(CondFlag::Eq.invert(), CondFlag::Ne);
        assert_eq!(CondFlag::Lo.invert(), CondFlag::Hs);
        assert_eq!(CondFlag::Gt.invert(), CondFlag::Le);
        assert_eq!(CondFlag::Al.invert(), CondFlag::Nv);
    }

    #[test]
    fn ssa_condition_mapping_matches_go_backend() {
        assert_eq!(
            CondFlag::from_ssa_integer_cmp(IntegerCmpCond::UnsignedGreaterThan),
            CondFlag::Hi
        );
        assert_eq!(
            CondFlag::from_ssa_integer_cmp(IntegerCmpCond::SignedLessThan),
            CondFlag::Lt
        );
        assert_eq!(
            CondFlag::from_ssa_float_cmp(FloatCmpCond::LessThanOrEqual),
            CondFlag::Ls
        );
        assert_eq!(
            CondFlag::from_ssa_float_cmp(FloatCmpCond::GreaterThan),
            CondFlag::Gt
        );
    }
}
