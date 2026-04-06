use std::collections::BTreeMap;
use std::ptr::NonNull;

use crate::backend::machine::{BackendError, Machine as BackendMachine};
use crate::backend::{CompilerContext, FunctionAbi, RealReg, RelocationInfo, VReg};
use crate::ssa::{BasicBlock, BasicBlockId, Instruction, Signature, SourceOffset, Type, Value};
use crate::wazevoapi::ExitCode;

use super::abi::{FLOAT_ARG_RESULT_REGS, INT_ARG_RESULT_REGS};
use super::abi_entry_preamble::compile_entry_preamble;
use super::abi_host_call::compile_host_function_trampoline;
use super::instr::Amd64Instr;
use super::machine_pro_epi_logue::{append_epilogue, append_prologue};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Amd64Block {
    pub id: i32,
    pub instructions: Vec<Amd64Instr>,
    pub preds: Vec<i32>,
    pub succs: Vec<i32>,
    pub entry: bool,
    pub loop_header: bool,
    pub children: Vec<i32>,
    pub params: Vec<VReg>,
}

#[derive(Default)]
pub struct Amd64Machine {
    pub current_abi: FunctionAbi,
    pub blocks: Vec<Amd64Block>,
    pub current_block: Option<usize>,
    pub compiler: Option<NonNull<dyn CompilerContext>>,
    pub clobbered: Vec<VReg>,
    pub spill_slots: BTreeMap<u32, i64>,
    pub stack_check_disabled: bool,
}

impl Amd64Machine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, inst: Amd64Instr) {
        let idx = self.current_block.expect("no current block");
        self.blocks[idx].instructions.push(inst);
    }

    fn ensure_block(&mut self, block: BasicBlock) -> usize {
        let id = block.0 as i32;
        if let Some(idx) = self.blocks.iter().position(|b| b.id == id) {
            idx
        } else {
            let idx = self.blocks.len();
            self.blocks.push(Amd64Block {
                id,
                entry: id == 0,
                ..Amd64Block::default()
            });
            idx
        }
    }

    pub fn encode_all(&self) -> Result<Vec<u8>, BackendError> {
        let mut out = Vec::new();
        for block in &self.blocks {
            for inst in &block.instructions {
                out.extend(inst.encode()?);
            }
        }
        Ok(out)
    }

    pub fn append_prologue(&mut self) {
        append_prologue(self);
    }

    pub fn append_epilogue(&mut self) {
        append_epilogue(self);
    }
}

impl BackendMachine for Amd64Machine {
    fn start_lowering_function(&mut self, max_block_id: BasicBlockId) {
        self.blocks.clear();
        for id in 0..=max_block_id.0 {
            self.blocks.push(Amd64Block {
                id: id as i32,
                entry: id == 0,
                ..Amd64Block::default()
            });
        }
        self.current_block = None;
    }

    fn link_adjacent_blocks(&mut self, prev: BasicBlock, next: BasicBlock) {
        let prev_idx = self.ensure_block(prev);
        let next_idx = self.ensure_block(next);
        self.blocks[prev_idx].succs.push(next.0 as i32);
        self.blocks[next_idx].preds.push(prev.0 as i32);
    }

    fn start_block(&mut self, block: BasicBlock) {
        self.current_block = Some(self.ensure_block(block));
    }

    fn end_block(&mut self) {}

    fn flush_pending_instructions(&mut self) {}

    fn disable_stack_check(&mut self) {
        self.stack_check_disabled = true;
    }

    fn set_current_abi(&mut self, abi: FunctionAbi) {
        self.current_abi = abi;
    }

    fn set_compiler(&mut self, compiler: NonNull<dyn CompilerContext>) {
        self.compiler = Some(compiler);
    }

    fn lower_single_branch(&mut self, _branch: &Instruction) {}

    fn lower_conditional_branch(&mut self, _branch: &Instruction) {}

    fn lower_instr(&mut self, _instruction: &Instruction) {}

    fn reset(&mut self) {
        self.blocks.clear();
        self.current_block = None;
        self.clobbered.clear();
        self.spill_slots.clear();
        self.stack_check_disabled = false;
    }

    fn insert_move(&mut self, dst: VReg, src: VReg, ty: Type) {
        self.push(Amd64Instr::mov_rr(src, dst, ty.is_int() && ty.bits() == 64));
    }

    fn insert_return(&mut self) {
        self.push(Amd64Instr::ret());
    }

    fn insert_load_constant_block_arg(&mut self, _instr: &Instruction, _dst: VReg) {}

    fn format(&self) -> String {
        let mut out = String::new();
        for block in &self.blocks {
            out.push_str(&format!("blk{}:\n", block.id));
            for inst in &block.instructions {
                out.push_str("  ");
                out.push_str(&inst.to_string());
                out.push('\n');
            }
        }
        out
    }

    fn reg_alloc(&mut self) {}

    fn post_reg_alloc(&mut self) {}

    fn resolve_relocations(
        &mut self,
        _ref_to_binary_offset: &[i32],
        _imported_fns: usize,
        _executable: &mut [u8],
        _relocations: &[RelocationInfo],
        _call_trampoline_island_offsets: &[i32],
    ) {
    }

    fn encode(&mut self) -> Result<(), BackendError> {
        let bytes = self.encode_all()?;
        if let Some(mut compiler) = self.compiler {
            unsafe {
                let compiler = compiler.as_mut();
                compiler.buf_mut().clear();
                compiler.buf_mut().extend_from_slice(&bytes);
            }
        }
        Ok(())
    }

    fn compile_host_function_trampoline(
        &mut self,
        exit_code: ExitCode,
        sig: &Signature,
        need_module_context_ptr: bool,
    ) -> Vec<u8> {
        compile_host_function_trampoline(exit_code, sig, need_module_context_ptr)
    }

    fn compile_stack_grow_call_sequence(&mut self) -> Vec<u8> {
        vec![0x0F, 0x0B]
    }

    fn compile_entry_preamble(&mut self, signature: &Signature, use_host_stack: bool) -> Vec<u8> {
        compile_entry_preamble(signature, use_host_stack)
    }

    fn lower_params(&mut self, _params: &[Value]) {}

    fn lower_returns(&mut self, _returns: &[Value]) {}

    fn args_results_regs(&self) -> (&[RealReg], &[RealReg]) {
        (&INT_ARG_RESULT_REGS, &FLOAT_ARG_RESULT_REGS)
    }

    fn call_trampoline_island_info(
        &self,
        _num_functions: usize,
    ) -> Result<(usize, usize), BackendError> {
        Ok((0, 0))
    }

    fn add_source_offset_info(&mut self, _executable_offset: i64, _source_offset: SourceOffset) {}
}

#[cfg(test)]
mod tests {
    use super::Amd64Machine;
    use crate::backend::machine::Machine;
    use crate::backend::{RegType, VReg};
    use crate::ssa::BasicBlockId;

    #[test]
    fn machine_formats_blocks_and_instructions() {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));
        m.insert_move(
            VReg::from_real_reg(2, RegType::Int),
            VReg::from_real_reg(1, RegType::Int),
            crate::ssa::Type::I64,
        );
        m.insert_return();
        assert!(m.format().contains("movq %rax, %rcx"));
        assert!(m.encode_all().unwrap().ends_with(&[0xC3]));
    }
}
