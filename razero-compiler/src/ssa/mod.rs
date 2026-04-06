//! SSA IR core modeled after wazero's wazevo SSA package.

pub mod basic_block;
pub mod basic_block_sort;
pub mod builder;
pub mod cmp;
pub mod funcref;
pub mod instructions;
pub mod pass;
pub mod pass_blk_layouts;
pub mod pass_cfg;
pub mod signature;
pub mod types;
pub mod vs;

pub use basic_block::{
    BasicBlock, BasicBlockData, BasicBlockId, BasicBlockPredecessorInfo,
    BASIC_BLOCK_ID_RETURN_BLOCK,
};
pub use builder::{Builder, ValueInfo};
pub use cmp::{FloatCmpCond, IntegerCmpCond};
pub use funcref::FuncRef;
pub use instructions::{
    AtomicRmwOp, Instruction, InstructionGroupId, InstructionId, Opcode, SourceOffset,
};
pub use signature::{Signature, SignatureId};
pub use types::{Type, VecLane};
pub use vs::{Value, ValueId, Values, Variable};
