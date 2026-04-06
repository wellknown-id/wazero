//! ISA-independent backend compiler core.

use core::fmt;

use crate::ssa::Type;

pub mod abi;
pub mod compiler;
pub mod compiler_lower;
pub mod host_call;
pub mod isa;
pub mod machine;
pub mod regalloc;
pub mod vdef;

pub use abi::{abi_info_from_u64, AbiArg, AbiArgKind, FunctionAbi};
pub use compiler::{
    CompilationOutput, Compiler, CompilerContext, RelocationInfo, SourceOffsetInfo,
};
pub use host_call::go_function_call_required_stack_size;
pub use machine::{BackendError, Machine};
pub use vdef::SSAValueDefinition;

pub type RealReg = u8;
pub const REAL_REG_INVALID: RealReg = 0;

pub type VRegId = u32;
pub const VREG_ID_INVALID: VRegId = 1 << 31;
const VREG_ID_RESERVED_FOR_REAL_NUM: VRegId = 128;
pub const VREG_ID_NON_RESERVED_BEGIN: VRegId = VREG_ID_RESERVED_FOR_REAL_NUM;

const REAL_REG_MASK: u64 = 0x0000_00ff_0000_0000;
const REG_TYPE_MASK: u64 = 0x0000_ff00_0000_0000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum RegType {
    #[default]
    Invalid = 0,
    Int,
    Float,
}

impl RegType {
    pub const fn of(ty: Type) -> Self {
        match ty {
            Type::I32 | Type::I64 => Self::Int,
            Type::F32 | Type::F64 | Type::V128 => Self::Float,
            Type::Invalid => panic!("invalid SSA type"),
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VReg(pub u64);

impl VReg {
    pub const INVALID: Self = Self(VREG_ID_INVALID as u64);

    pub const fn real_reg(self) -> RealReg {
        ((self.0 & REAL_REG_MASK) >> 32) as RealReg
    }

    pub const fn is_real_reg(self) -> bool {
        self.real_reg() != REAL_REG_INVALID
    }

    pub fn from_real_reg(real_reg: RealReg, reg_type: RegType) -> Self {
        let id = real_reg as VRegId;
        assert!(
            id <= VREG_ID_RESERVED_FOR_REAL_NUM,
            "invalid real register {real_reg}"
        );
        Self(id as u64)
            .set_real_reg(real_reg)
            .set_reg_type(reg_type)
    }

    pub const fn set_real_reg(self, real_reg: RealReg) -> Self {
        Self((self.0 & !REAL_REG_MASK) | ((real_reg as u64) << 32))
    }

    pub const fn reg_type(self) -> RegType {
        match ((self.0 & REG_TYPE_MASK) >> 40) as u8 {
            1 => RegType::Int,
            2 => RegType::Float,
            _ => RegType::Invalid,
        }
    }

    pub const fn set_reg_type(self, reg_type: RegType) -> Self {
        Self((self.0 & !REG_TYPE_MASK) | ((reg_type as u64) << 40))
    }

    pub const fn id(self) -> VRegId {
        self.0 as u32
    }

    pub const fn valid(self) -> bool {
        self.id() != VREG_ID_INVALID && !matches!(self.reg_type(), RegType::Invalid)
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
    use super::{RegType, VReg, REAL_REG_INVALID, VREG_ID_NON_RESERVED_BEGIN};
    use crate::ssa::Type;

    #[test]
    fn reg_type_follows_ssa_type_classes() {
        assert_eq!(RegType::of(Type::I64), RegType::Int);
        assert_eq!(RegType::of(Type::F32), RegType::Float);
        assert_eq!(RegType::of(Type::V128), RegType::Float);
    }

    #[test]
    fn vreg_tracks_id_real_reg_and_type() {
        let real = VReg::from_real_reg(7, RegType::Int);
        assert_eq!(real.id(), 7);
        assert_eq!(real.real_reg(), 7);
        assert_eq!(real.reg_type(), RegType::Int);
        assert!(real.is_real_reg());
        assert_eq!(real.to_string(), "r7");

        let virtual_reg = VReg(VREG_ID_NON_RESERVED_BEGIN as u64).set_reg_type(RegType::Float);
        assert_eq!(virtual_reg.real_reg(), REAL_REG_INVALID);
        assert_eq!(virtual_reg.reg_type(), RegType::Float);
        assert_eq!(virtual_reg.to_string(), "v128?");
    }
}
