use core::fmt;
use std::error::Error;
use std::ptr::NonNull;

use crate::ssa::{BasicBlock, BasicBlockId, Instruction, Signature, SourceOffset, Type, Value};
use crate::wazevoapi::ExitCode;

use super::{CompilerContext, FunctionAbi, RealReg, RelocationInfo, VReg};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendError {
    message: String,
}

impl BackendError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for BackendError {}

pub trait Machine {
    fn start_lowering_function(&mut self, max_block_id: BasicBlockId);
    fn link_adjacent_blocks(&mut self, prev: BasicBlock, next: BasicBlock);
    fn start_block(&mut self, block: BasicBlock);
    fn end_block(&mut self);
    fn flush_pending_instructions(&mut self);
    fn disable_stack_check(&mut self);
    fn set_current_abi(&mut self, abi: FunctionAbi);
    fn set_compiler(&mut self, compiler: NonNull<dyn CompilerContext>);
    fn lower_single_branch(&mut self, branch: &Instruction);
    fn lower_conditional_branch(&mut self, branch: &Instruction);
    fn lower_instr(&mut self, instruction: &Instruction);
    fn reset(&mut self);
    fn insert_move(&mut self, dst: VReg, src: VReg, ty: Type);
    fn insert_return(&mut self);
    fn insert_load_constant_block_arg(&mut self, instr: &Instruction, dst: VReg);
    fn format(&self) -> String;
    fn reg_alloc(&mut self);
    fn post_reg_alloc(&mut self);
    fn resolve_relocations(
        &mut self,
        ref_to_binary_offset: &[i32],
        imported_fns: usize,
        executable: &mut [u8],
        relocations: &[RelocationInfo],
        call_trampoline_island_offsets: &[i32],
    );
    fn encode(&mut self) -> Result<(), BackendError>;
    fn compile_host_function_trampoline(
        &mut self,
        exit_code: ExitCode,
        sig: &Signature,
        need_module_context_ptr: bool,
    ) -> Vec<u8>;
    fn compile_stack_grow_call_sequence(&mut self) -> Vec<u8>;
    fn compile_entry_preamble(&mut self, signature: &Signature, use_host_stack: bool) -> Vec<u8>;
    fn lower_params(&mut self, params: &[Value]);
    fn lower_returns(&mut self, returns: &[Value]);
    fn args_results_regs(&self) -> (&[RealReg], &[RealReg]);
    fn call_trampoline_island_info(
        &self,
        num_functions: usize,
    ) -> Result<(usize, usize), BackendError>;
    fn add_source_offset_info(&mut self, _executable_offset: i64, _source_offset: SourceOffset) {}
}
