//! Register and virtual-register definitions.

use std::fmt;

use crate::ssa::Type;

pub type VRegId = u32;

pub const VREG_ID_INVALID: VRegId = 1 << 31;
pub const VREG_ID_RESERVED_FOR_REAL_NUM: VRegId = 128;
pub const VREG_ID_NON_RESERVED_BEGIN: VRegId = VREG_ID_RESERVED_FOR_REAL_NUM;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VReg(u64);

impl Default for VReg {
    fn default() -> Self {
        VREG_INVALID
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct RealReg(pub u8);

pub const REAL_REG_INVALID: RealReg = RealReg(0);
pub const VREG_INVALID: VReg = VReg(VREG_ID_INVALID as u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RegType {
    #[default]
    Invalid = 0,
    Int,
    Float,
}

pub const NUM_REG_TYPES: usize = 3;

impl RegType {
    pub const fn index(self) -> usize {
        self as usize
    }
}

impl VReg {
    pub const fn new(index: VRegId) -> Self {
        Self(index as u64)
    }

    pub const fn from_real_reg(r: RealReg, typ: RegType) -> Self {
        if r.0 as u32 > VREG_ID_RESERVED_FOR_REAL_NUM {
            panic!("invalid real reg");
        }
        Self::new(r.0 as VRegId).set_real_reg(r).set_reg_type(typ)
    }

    pub const fn real_reg(self) -> RealReg {
        RealReg((self.0 >> 32) as u8)
    }

    pub const fn is_real_reg(self) -> bool {
        self.real_reg().0 != REAL_REG_INVALID.0
    }

    pub const fn set_real_reg(self, r: RealReg) -> Self {
        Self(((r.0 as u64) << 32) | (self.0 & 0xff_00_ffff_ffff))
    }

    pub const fn reg_type(self) -> RegType {
        match ((self.0 >> 40) & 0xff) as u8 {
            1 => RegType::Int,
            2 => RegType::Float,
            _ => RegType::Invalid,
        }
    }

    pub const fn set_reg_type(self, t: RegType) -> Self {
        Self(((t as u64) << 40) | (self.0 & 0x00_ff_ffff_ffff))
    }

    pub const fn id(self) -> VRegId {
        (self.0 & 0xffff_ffff) as VRegId
    }

    pub const fn valid(self) -> bool {
        self.id() != VREG_ID_INVALID && !matches!(self.reg_type(), RegType::Invalid)
    }
}

pub fn reg_type_of(ty: Type) -> RegType {
    match ty {
        Type::I32 | Type::I64 => RegType::Int,
        Type::F32 | Type::F64 | Type::V128 => RegType::Float,
        Type::Invalid => panic!("invalid type"),
    }
}

impl fmt::Display for RealReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == REAL_REG_INVALID {
            f.write_str("invalid")
        } else {
            write!(f, "r{}", self.0)
        }
    }
}

impl fmt::Display for RegType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid",
            Self::Int => "int",
            Self::Float => "float",
        })
    }
}

impl fmt::Display for VReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_real_reg() {
            write!(f, "r{}", self.id())
        } else {
            write!(f, "v{}?", self.id())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{reg_type_of, RealReg, RegType, VReg};
    use crate::ssa::Type;

    #[test]
    fn reg_type_of_matches_go_mapping() {
        assert_eq!(reg_type_of(Type::I32), RegType::Int);
        assert_eq!(reg_type_of(Type::I64), RegType::Int);
        assert_eq!(reg_type_of(Type::F32), RegType::Float);
        assert_eq!(reg_type_of(Type::F64), RegType::Float);
        assert_eq!(reg_type_of(Type::V128), RegType::Float);
    }

    #[test]
    fn vreg_display_is_stable() {
        assert_eq!(VReg::new(0).to_string(), "v0?");
        assert_eq!(VReg::new(100).to_string(), "v100?");
        assert_eq!(
            VReg::from_real_reg(RealReg(5), RegType::Int).to_string(),
            "r5"
        );
    }

    #[test]
    fn from_real_reg_precolors_id_and_register() {
        let reg = VReg::from_real_reg(RealReg(5), RegType::Int);
        assert_eq!(reg.real_reg(), RealReg(5));
        assert_eq!(reg.id(), 5);
        assert_eq!(reg.reg_type(), RegType::Int);
    }
}
