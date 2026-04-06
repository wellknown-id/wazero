use std::ptr::NonNull;

use crate::backend::compiler::{CompilerContext, RelocationInfo};
use crate::backend::machine::{BackendError, Machine};
use crate::backend::FunctionAbi;
use crate::ssa::{BasicBlock, BasicBlockId, Instruction, Opcode, Signature, SourceOffset, Type, Value};
use crate::wazevoapi::ExitCode;

use super::cond::{Cond, CondFlag};
use super::instr::{AluOp, Arm64Instr, LoadKind, StoreKind};
use super::instr_encoding::encode_instruction;
use super::lower_constant::lower_constant;
use super::lower_mem::{AddressMode, AddressModeKind};
use super::machine_relocation;
use super::reg::{vreg_for_real_reg, ARG_RESULT_FLOAT_REGS, ARG_RESULT_INT_REGS, SP};

#[derive(Default)]
pub struct Arm64Machine {
    compiler: Option<NonNull<dyn CompilerContext>>,
    pub(crate) current_abi: FunctionAbi,
    instructions: Vec<Arm64Instr>,
    pub(crate) pending_instructions: Vec<Arm64Instr>,
    pub spill_slot_size: i64,
    pub spill_slots: std::collections::BTreeMap<u32, i64>,
    pub clobbered_regs: Vec<crate::backend::VReg>,
    pub max_required_stack_size_for_calls: i64,
    pub stack_bounds_check_disabled: bool,
    max_block_id: BasicBlockId,
}

impl Arm64Machine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, instr: Arm64Instr) {
        self.pending_instructions.push(instr);
    }

    pub fn instructions(&self) -> &[Arm64Instr] {
        &self.instructions
    }

    pub(crate) fn compiler_mut(&mut self) -> &mut dyn CompilerContext {
        unsafe { self.compiler.expect("compiler not set").as_mut() }
    }

    pub(crate) fn compiler(&self) -> &dyn CompilerContext {
        unsafe { self.compiler.expect("compiler not set").as_ref() }
    }

    pub fn emit_all(&mut self) -> Result<(), BackendError> {
        let instructions = self.instructions.clone();
        for instr in &instructions {
            for word in encode_instruction(instr)? {
                self.compiler_mut().emit4_bytes(word);
            }
        }
        Ok(())
    }
}

impl Machine for Arm64Machine {
    fn start_lowering_function(&mut self, max_block_id: BasicBlockId) {
        self.max_block_id = max_block_id;
    }

    fn link_adjacent_blocks(&mut self, _prev: BasicBlock, _next: BasicBlock) {}

    fn start_block(&mut self, block: BasicBlock) {
        self.instructions.push(Arm64Instr::Label(block.0));
    }

    fn end_block(&mut self) {}

    fn flush_pending_instructions(&mut self) {
        self.instructions.append(&mut self.pending_instructions);
    }

    fn disable_stack_check(&mut self) {
        self.stack_bounds_check_disabled = true;
    }

    fn set_current_abi(&mut self, abi: FunctionAbi) {
        self.current_abi = abi;
    }

    fn set_compiler(&mut self, compiler: NonNull<dyn CompilerContext>) {
        self.compiler = Some(compiler);
    }

    fn lower_single_branch(&mut self, branch: &Instruction) {
        match branch.opcode {
            Opcode::Jump => self.push(Arm64Instr::Br { offset: 0, link: false }),
            Opcode::Return => self.insert_return(),
            _ => self.push(Arm64Instr::Udf { imm: 0xdead }),
        }
    }

    fn lower_conditional_branch(&mut self, branch: &Instruction) {
        let cond_reg = self.compiler().v_reg_of(branch.v);
        let cond = match branch.opcode {
            Opcode::Brz => Cond::from_reg_zero(cond_reg),
            Opcode::Brnz => Cond::from_reg_not_zero(cond_reg),
            _ => Cond::from_flag(CondFlag::Nv),
        };
        self.push(Arm64Instr::CondBr {
            cond,
            offset: 0,
            bits64: true,
        });
    }

    fn lower_instr(&mut self, instruction: &Instruction) {
        match instruction.opcode {
            Opcode::Iconst | Opcode::F32const | Opcode::F64const | Opcode::Vconst => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let seq = lower_constant(dst, instruction.typ, instruction.u1, instruction.u2);
                self.pending_instructions.extend(seq);
            }
            Opcode::Iadd | Opcode::Isub | Opcode::Imul => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let rn = self.compiler().v_reg_of(instruction.v);
                let rm = self.compiler().v_reg_of(instruction.v2);
                let op = match instruction.opcode {
                    Opcode::Iadd => AluOp::Add,
                    Opcode::Isub => AluOp::Sub,
                    Opcode::Imul => AluOp::Mul,
                    _ => unreachable!(),
                };
                self.push(Arm64Instr::AluRRR {
                    op,
                    rd: dst,
                    rn,
                    rm,
                    bits: instruction.v.ty().bits(),
                    set_flags: false,
                });
            }
            Opcode::Call => {
                let (_, sig, _) = instruction.call_data();
                let signature = self
                    .compiler()
                    .ssa_builder()
                    .resolve_signature(sig)
                    .expect("call signature must exist")
                    .clone();
                let abi_info = self
                    .compiler_mut()
                    .get_function_abi(&signature)
                    .abi_info_as_u64();
                self.push(Arm64Instr::Call {
                    offset: 0,
                    abi: abi_info,
                });
            }
            Opcode::CallIndirect => {
                let (func_ptr, sig, _) = instruction.call_indirect_data();
                let signature = self
                    .compiler()
                    .ssa_builder()
                    .resolve_signature(sig)
                    .expect("call signature must exist")
                    .clone();
                let abi_info = self
                    .compiler_mut()
                    .get_function_abi(&signature)
                    .abi_info_as_u64();
                let func_ptr_reg = self.compiler().v_reg_of(func_ptr);
                self.push(Arm64Instr::CallReg {
                    rn: func_ptr_reg,
                    abi: abi_info,
                    tail: false,
                });
            }
            Opcode::Return => self.insert_return(),
            _ => self.push(Arm64Instr::Udf { imm: 0xbeef }),
        }
    }

    fn reset(&mut self) {
        self.instructions.clear();
        self.pending_instructions.clear();
        self.spill_slots.clear();
        self.clobbered_regs.clear();
        self.spill_slot_size = 0;
        self.max_required_stack_size_for_calls = 0;
        self.stack_bounds_check_disabled = false;
    }

    fn insert_move(&mut self, dst: crate::backend::VReg, src: crate::backend::VReg, ty: Type) {
        if ty.is_int() {
            self.push(Arm64Instr::Move {
                rd: dst,
                rn: src,
                bits: ty.bits(),
            });
        } else {
            self.push(Arm64Instr::FpuMove {
                rd: dst,
                rn: src,
                bits: ty.bits(),
            });
        }
    }

    fn insert_return(&mut self) {
        self.push(Arm64Instr::Ret);
    }

    fn insert_load_constant_block_arg(&mut self, instr: &Instruction, dst: crate::backend::VReg) {
        self.push(Arm64Instr::LoadConstBlockArg {
            dst,
            value: instr.u1,
        });
    }

    fn format(&self) -> String {
        self.instructions
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn reg_alloc(&mut self) {}

    fn post_reg_alloc(&mut self) {}

    fn resolve_relocations(
        &mut self,
        ref_to_binary_offset: &[i32],
        imported_fns: usize,
        executable: &mut [u8],
        relocations: &[RelocationInfo],
        call_trampoline_island_offsets: &[i32],
    ) {
        machine_relocation::resolve_relocations(
            ref_to_binary_offset,
            imported_fns,
            executable,
            relocations,
            call_trampoline_island_offsets,
        )
    }

    fn encode(&mut self) -> Result<(), BackendError> {
        self.flush_pending_instructions();
        self.emit_all()
    }

    fn compile_host_function_trampoline(
        &mut self,
        exit_code: ExitCode,
        sig: &Signature,
        need_module_context_ptr: bool,
    ) -> Vec<u8> {
        super::abi_host_call::compile_host_function_trampoline(exit_code, sig, need_module_context_ptr)
    }

    fn compile_stack_grow_call_sequence(&mut self) -> Vec<u8> {
        super::abi_host_call::compile_stack_grow_call_sequence()
    }

    fn compile_entry_preamble(&mut self, signature: &Signature, use_host_stack: bool) -> Vec<u8> {
        super::abi_entry_preamble::compile_entry_preamble(signature, use_host_stack)
    }

    fn lower_params(&mut self, params: &[Value]) {
        for (index, value) in params.iter().copied().enumerate() {
            if !value.valid() {
                continue;
            }
            let reg = self.compiler().v_reg_of(value);
            let arg = &self.current_abi.args[index];
            if arg.kind == crate::backend::AbiArgKind::Reg {
                self.insert_move(reg, arg.reg, arg.ty);
            } else {
                let addr = AddressMode {
                    kind: AddressModeKind::ArgStackSpace,
                    rn: vreg_for_real_reg(SP),
                    rm: crate::backend::VReg::INVALID,
                    ext_op: super::lower_instr_operands::ExtendOp::Uxtx,
                    imm: arg.offset,
                };
                self.push(Arm64Instr::Load {
                    kind: if arg.ty.is_int() { LoadKind::ULoad } else { LoadKind::FpuLoad },
                    rd: reg,
                    mem: addr,
                    bits: arg.ty.bits(),
                });
            }
        }
    }

    fn lower_returns(&mut self, returns: &[Value]) {
        for (index, value) in returns.iter().copied().enumerate().rev() {
            let ret = &self.current_abi.rets[index];
            let reg = self.compiler().v_reg_of(value);
            if ret.kind == crate::backend::AbiArgKind::Reg {
                self.insert_move(ret.reg, reg, ret.ty);
            } else {
                let addr = AddressMode {
                    kind: AddressModeKind::ResultStackSpace,
                    rn: vreg_for_real_reg(SP),
                    rm: crate::backend::VReg::INVALID,
                    ext_op: super::lower_instr_operands::ExtendOp::Uxtx,
                    imm: ret.offset,
                };
                self.push(Arm64Instr::Store {
                    kind: if ret.ty.is_int() { StoreKind::Store } else { StoreKind::FpuStore },
                    src: reg,
                    mem: addr,
                    bits: ret.ty.bits(),
                });
            }
        }
    }

    fn args_results_regs(&self) -> (&[u8], &[u8]) {
        (&ARG_RESULT_INT_REGS, &ARG_RESULT_FLOAT_REGS)
    }

    fn call_trampoline_island_info(
        &self,
        num_functions: usize,
    ) -> Result<(usize, usize), BackendError> {
        machine_relocation::call_trampoline_island_info(num_functions)
            .map_err(BackendError::new)
    }

    fn add_source_offset_info(&mut self, executable_offset: i64, source_offset: SourceOffset) {
        self.compiler_mut()
            .add_source_offset_info(executable_offset, source_offset);
    }
}

#[cfg(test)]
mod tests {
    use super::Arm64Machine;
    use crate::backend::Machine;

    #[test]
    fn machine_flush_and_format_keep_instruction_order() {
        let mut machine = Arm64Machine::new();
        machine.push(super::Arm64Instr::Nop);
        machine.push(super::Arm64Instr::Ret);
        machine.flush_pending_instructions();
        assert_eq!(machine.format(), "nop\nret");
    }
}
