//! Partial amd64 backend port with instruction modeling, encoding, ABI helpers,
//! regalloc hooks, and entry/trampoline scaffolding.

pub mod abi;
pub mod abi_entry;
pub mod abi_entry_preamble;
pub mod abi_host_call;
pub mod cond;
pub mod ext;
pub mod instr;
pub mod instr_encoding;
pub mod lower_constant;
pub mod lower_mem;
pub mod machine;
pub mod machine_pro_epi_logue;
pub mod machine_regalloc;
pub mod machine_vec;
pub mod operands;
pub mod reg;
pub mod stack;

pub use abi::{
    amd64_function_abi, amd64_register_info, FLOAT_ARG_RESULT_REGS, INT_ARG_RESULT_REGS,
};
pub use cond::Cond;
pub use ext::ExtMode;
pub use instr::{AluRmiROpcode, Amd64Instr, InstructionKind, UnaryRmROpcode};
pub use machine::{Amd64Block, Amd64Machine};
pub use machine_vec::SseOpcode;
pub use operands::{AddressMode, Label, Operand};
pub use reg::{
    format_vreg_sized, real_reg_name, real_reg_type, vreg_for_real_reg, R10, R11, R12, R13, R14,
    R15, R8, R9, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP, XMM0, XMM15,
};
