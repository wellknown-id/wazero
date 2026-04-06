//! arm64 backend scaffold.

pub mod abi;
pub mod abi_entry;
pub mod abi_entry_preamble;
pub mod abi_host_call;
pub mod cond;
pub mod instr;
pub mod instr_encoding;
pub mod lower_constant;
pub mod lower_instr;
pub mod lower_instr_operands;
pub mod lower_mem;
pub mod machine;
pub mod machine_pro_epi_logue;
pub mod machine_regalloc;
pub mod machine_relocation;
pub mod reg;
pub mod unwind_stack;

pub use abi::Arm64Abi;
pub use cond::{Cond, CondFlag, CondKind};
pub use instr::{AluOp, Arm64Instr, LoadKind, StoreKind};
pub use lower_instr_operands::{as_imm12, ExtendOp, Imm12, Operand, ShiftOp};
pub use lower_mem::{AddressMode, AddressModeKind};
pub use machine::Arm64Machine;
