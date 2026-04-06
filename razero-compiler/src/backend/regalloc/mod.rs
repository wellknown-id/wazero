//! ISA-agnostic register allocation support.

pub mod api;
pub mod reg;
pub mod regalloc;
pub mod regset;

pub use api::{Block, Function, Instr};
pub use reg::{
    reg_type_of, RealReg, RegType, VReg, VRegId, NUM_REG_TYPES, REAL_REG_INVALID, VREG_ID_INVALID,
    VREG_ID_NON_RESERVED_BEGIN, VREG_ID_RESERVED_FOR_REAL_NUM, VREG_INVALID,
};
pub use regalloc::{Allocator, RegisterInfo};
pub use regset::RegSet;
