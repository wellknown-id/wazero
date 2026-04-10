use std::collections::BTreeMap;
use std::ptr::NonNull;

use crate::backend::machine::{BackendError, Machine as BackendMachine};
use crate::backend::{
    AbiArgKind, CompilerContext, FunctionAbi, RealReg, RegType, RelocationInfo, VReg,
};
use crate::ssa::{
    cmp::{FloatCmpCond, IntegerCmpCond}, BasicBlock, BasicBlockId, Instruction, Opcode, Signature,
    SourceOffset, Type, Value,
};
use crate::wazevoapi::ExitCode;

use super::abi::{FLOAT_ARG_RESULT_REGS, INT_ARG_RESULT_REGS};
use super::abi_entry_preamble::compile_entry_preamble;
use super::abi_host_call::compile_host_function_trampoline;
use super::cond::Cond;
use super::ext::ExtMode;
use super::instr::Amd64Instr;
use super::lower_constant::lower_constant;
use super::lower_mem::mem_operand_from_base;
use super::machine_pro_epi_logue::{append_epilogue, append_prologue};
use super::machine_regalloc::do_regalloc;
use super::operands::{AddressMode, Label, Operand};
use super::SseOpcode;

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
    pub block_order: Vec<i32>,
    pub current_block: Option<usize>,
    pub compiler: Option<NonNull<dyn CompilerContext>>,
    pub clobbered: Vec<VReg>,
    pub spill_slots: BTreeMap<u32, i64>,
    pub spill_slot_size: i64,
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

    pub(crate) fn compiler(&self) -> &dyn CompilerContext {
        unsafe { self.compiler.expect("compiler not set").as_ref() }
    }

    pub(crate) fn compiler_mut(&mut self) -> &mut dyn CompilerContext {
        unsafe { self.compiler.expect("compiler not set").as_mut() }
    }

    pub(crate) fn current_block_mut(&mut self) -> &mut Amd64Block {
        let idx = self.current_block.expect("no current block");
        &mut self.blocks[idx]
    }

    fn ensure_block(&mut self, block: BasicBlock) -> usize {
        let id = block.0 as i32;
        if let Some(idx) = self.blocks.iter().position(|b| b.id == id) {
            self.blocks[idx].entry |= block.is_entry();
            idx
        } else {
            let idx = self.blocks.len();
            self.blocks.push(Amd64Block {
                id,
                entry: block.is_entry(),
                ..Amd64Block::default()
            });
            idx
        }
    }

    pub(crate) fn ensure_spill_slot(&mut self, vreg: VReg) -> i64 {
        if let Some(offset) = self.spill_slots.get(&vreg.id()) {
            *offset
        } else {
            self.spill_slot_size += if matches!(vreg.reg_type(), crate::backend::RegType::Float) {
                16
            } else {
                8
            };
            let offset = -self.spill_slot_size;
            self.spill_slots.insert(vreg.id(), offset);
            offset
        }
    }

    pub(crate) fn load_from_spill_slot(&mut self, vreg: VReg) -> Amd64Instr {
        let offset = self.ensure_spill_slot(vreg) as i32 as u32;
        if matches!(vreg.reg_type(), crate::backend::RegType::Float) {
            Amd64Instr::xmm_unary_rm_r(
                SseOpcode::Movdqu,
                Operand::mem(AddressMode::imm_rbp(offset)),
                vreg,
            )
        } else {
            Amd64Instr::mov64_mr(Operand::mem(AddressMode::imm_rbp(offset)), vreg)
        }
    }

    pub(crate) fn store_to_spill_slot(&mut self, vreg: VReg) -> Amd64Instr {
        let offset = self.ensure_spill_slot(vreg) as i32 as u32;
        if matches!(vreg.reg_type(), crate::backend::RegType::Float) {
            Amd64Instr::xmm_mov_rm(
                SseOpcode::Movdqu,
                vreg,
                Operand::mem(AddressMode::imm_rbp(offset)),
            )
        } else {
            Amd64Instr::mov_rm(vreg, Operand::mem(AddressMode::imm_rbp(offset)), 8)
        }
    }

    pub fn encode_all(&self) -> Result<Vec<u8>, BackendError> {
        let mut out = Vec::new();
        let mut label_offsets = BTreeMap::new();
        let mut pending_branches = Vec::new();

        let mut encode_block =
            |block: &Amd64Block, out: &mut Vec<u8>| -> Result<(), BackendError> {
                label_offsets.insert(block.id as u32, out.len() as i64);
                for inst in &block.instructions {
                    let before = out.len();
                    out.extend(inst.encode()?);
                    match inst.kind() {
                        super::InstructionKind::Jmp | super::InstructionKind::JmpIf => {
                            let data = inst.0.borrow();
                            let Some(Operand::Label(label)) = data.op1.as_ref() else {
                                return Err(BackendError::new("branch missing label target"));
                            };
                            pending_branches.push((before, out.len() - 4, label.0));
                        }
                        _ => {}
                    }
                }
                Ok(())
            };

        if self.block_order.is_empty() {
            for block in &self.blocks {
                encode_block(block, &mut out)?;
            }
        } else {
            for &id in &self.block_order {
                let block = self
                    .blocks
                    .iter()
                    .find(|block| block.id == id)
                    .ok_or_else(|| BackendError::new("missing block for encode order"))?;
                encode_block(block, &mut out)?;
            }
        }

        for (_inst_offset, imm_offset, label) in pending_branches {
            let target_offset = *label_offsets
                .get(&label)
                .ok_or_else(|| BackendError::new("missing branch label"))?;
            let branch_offset = target_offset - (imm_offset as i64 + 4);
            let disp = i32::try_from(branch_offset)
                .map_err(|_| BackendError::new("branch target out of range"))?;
            out[imm_offset..imm_offset + 4].copy_from_slice(&disp.to_le_bytes());
        }
        Ok(out)
    }

    pub fn append_prologue(&mut self) {
        append_prologue(self);
    }

    pub fn append_epilogue(&mut self) {
        append_epilogue(self);
    }

    fn emit_exit_with_code(&mut self, exec_ctx: VReg, exit_code: ExitCode) {
        let tmp = crate::backend::VReg::from_real_reg(
            super::reg::R11,
            crate::backend::RegType::Int,
        );
        self.current_block_mut()
            .instructions
            .push(Amd64Instr::imm(tmp, exit_code.raw() as u64, false));
        self.current_block_mut().instructions.push(Amd64Instr::mov_rm(
            tmp,
            Operand::mem(AddressMode::imm_reg(
                crate::wazevoapi::offsetdata::EXECUTION_CONTEXT_OFFSET_EXIT_CODE_OFFSET.u32(),
                exec_ctx,
            )),
            4,
        ));
        append_epilogue(self);
    }

    fn next_synthetic_block_id(&self) -> u32 {
        self.blocks
            .iter()
            .map(|block| block.id.max(0) as u32)
            .max()
            .unwrap_or(0)
            + 1
    }

    fn start_synthetic_block(&mut self, id: u32) {
        let block = BasicBlockId(id);
        let idx = self.ensure_block(block);
        self.current_block = Some(idx);
        self.blocks[idx].entry = false;
        self.blocks[idx].params.clear();
        let id = id as i32;
        if !self.block_order.contains(&id) {
            self.block_order.push(id);
        }
    }

    fn lower_icmp_to_flags(&mut self, x: Value, y: Value, cond: IntegerCmpCond) -> Cond {
        let lhs = self.compiler().v_reg_of(x);
        let rhs = self.compiler().v_reg_of(y);
        self.current_block_mut()
            .instructions
            .push(Amd64Instr::cmp_rmi_r(
                Operand::reg(rhs),
                lhs,
                true,
                x.ty().bits() == 64,
            ));
        Cond::from_int_cmp(cond)
    }

    fn lower_fcmp_to_flags(&mut self, instruction: &Instruction) -> (Cond, Option<(Cond, bool)>) {
        let mut x = instruction.v;
        let mut y = instruction.v2;
        let cond = Self::float_cmp_cond_from_u8(instruction.u1 as u8);
        let (first, second) = match cond {
            FloatCmpCond::Equal => (Cond::NP, Some((Cond::Z, true))),
            FloatCmpCond::NotEqual => (Cond::P, Some((Cond::NZ, false))),
            FloatCmpCond::LessThan => {
                x = instruction.v2;
                y = instruction.v;
                (Cond::from_float_cmp(FloatCmpCond::GreaterThan), None)
            }
            FloatCmpCond::LessThanOrEqual => {
                x = instruction.v2;
                y = instruction.v;
                (Cond::from_float_cmp(FloatCmpCond::GreaterThanOrEqual), None)
            }
            FloatCmpCond::GreaterThan | FloatCmpCond::GreaterThanOrEqual => {
                (Cond::from_float_cmp(cond), None)
            }
            FloatCmpCond::Invalid => panic!("invalid float comparison condition"),
        };

        let op = match x.ty() {
            Type::F32 => SseOpcode::Ucomiss,
            Type::F64 => SseOpcode::Ucomisd,
            _ => panic!("unsupported amd64 float comparison type: {:?}", x.ty()),
        };
        let lhs = self.compiler().v_reg_of(x);
        let rhs = self.compiler().v_reg_of(y);
        self.current_block_mut()
            .instructions
            .push(Amd64Instr::xmm_cmp_rm_r(op, Operand::reg(rhs), lhs));
        (first, second)
    }

    fn integer_cmp_cond_from_u8(raw: u8) -> IntegerCmpCond {
        match raw {
            1 => IntegerCmpCond::Equal,
            2 => IntegerCmpCond::NotEqual,
            3 => IntegerCmpCond::SignedLessThan,
            4 => IntegerCmpCond::SignedGreaterThanOrEqual,
            5 => IntegerCmpCond::SignedGreaterThan,
            6 => IntegerCmpCond::SignedLessThanOrEqual,
            7 => IntegerCmpCond::UnsignedLessThan,
            8 => IntegerCmpCond::UnsignedGreaterThanOrEqual,
            9 => IntegerCmpCond::UnsignedGreaterThan,
            10 => IntegerCmpCond::UnsignedLessThanOrEqual,
            _ => panic!("invalid integer comparison condition"),
        }
    }

    fn float_cmp_cond_from_u8(raw: u8) -> FloatCmpCond {
        match raw {
            1 => FloatCmpCond::Equal,
            2 => FloatCmpCond::NotEqual,
            3 => FloatCmpCond::LessThan,
            4 => FloatCmpCond::LessThanOrEqual,
            5 => FloatCmpCond::GreaterThan,
            6 => FloatCmpCond::GreaterThanOrEqual,
            _ => panic!("invalid float comparison condition"),
        }
    }

    fn lower_idivrem(&mut self, instruction: &Instruction, is_div: bool, is_signed: bool) {
        let dst = self.compiler().v_reg_of(instruction.return_());
        let lhs = self.compiler().v_reg_of(instruction.v);
        let rhs = self.compiler().v_reg_of(instruction.v2);
        let exec_ctx = self.compiler().v_reg_of(instruction.v3);
        let is_64 = instruction.typ.bits() == 64;
        let rax = VReg::from_real_reg(super::reg::RAX, RegType::Int);
        let rdx = VReg::from_real_reg(super::reg::RDX, RegType::Int);

        let div_block = self.next_synthetic_block_id();
        self.current_block_mut()
            .instructions
            .push(Amd64Instr::cmp_rmi_r(
                Operand::reg(rhs),
                rhs,
                false,
                is_64,
            ));
        self.current_block_mut()
            .instructions
            .push(Amd64Instr::jmp_if(Cond::NZ, Operand::label(Label(div_block))));
        self.link_branch_edge(BasicBlockId(div_block));
        self.emit_exit_with_code(exec_ctx, ExitCode::INTEGER_DIVISION_BY_ZERO);
        self.start_synthetic_block(div_block);

        self.current_block_mut()
            .instructions
            .push(Amd64Instr::mov_rr(lhs, rax, is_64));

        if is_signed {
            let neg1 = self.compiler_mut().allocate_vreg(instruction.typ);
            self.current_block_mut()
                .instructions
                .push(Amd64Instr::imm(
                    neg1,
                    if is_64 { u64::MAX } else { u32::MAX as u64 },
                    is_64,
                ));
            if is_div {
                let normal_block = self.next_synthetic_block_id();
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::cmp_rmi_r(
                        Operand::reg(neg1),
                        rhs,
                        true,
                        is_64,
                    ));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::jmp_if(
                        Cond::NZ,
                        Operand::label(Label(normal_block)),
                    ));

                let min_int = self.compiler_mut().allocate_vreg(instruction.typ);
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::imm(
                        min_int,
                        if is_64 {
                            0x8000_0000_0000_0000
                        } else {
                            0x8000_0000
                        },
                        is_64,
                    ));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::cmp_rmi_r(
                        Operand::reg(min_int),
                        lhs,
                        true,
                        is_64,
                    ));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::jmp_if(
                        Cond::NZ,
                        Operand::label(Label(normal_block)),
                    ));
                self.link_branch_edge(BasicBlockId(normal_block));
                self.emit_exit_with_code(exec_ctx, ExitCode::INTEGER_OVERFLOW);
                self.start_synthetic_block(normal_block);
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::sign_extend_data(is_64));
            } else {
                let normal_block = self.next_synthetic_block_id();
                let done_block = normal_block + 1;
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::cmp_rmi_r(
                        Operand::reg(neg1),
                        rhs,
                        true,
                        is_64,
                    ));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::jmp_if(
                        Cond::NZ,
                        Operand::label(Label(normal_block)),
                    ));
                self.link_branch_edge(BasicBlockId(normal_block));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::imm(rdx, 0, false));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::jmp(Operand::label(Label(done_block))));
                self.link_branch_edge(BasicBlockId(done_block));

                self.start_synthetic_block(normal_block);
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::sign_extend_data(is_64));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::div(Operand::reg(rhs), true, is_64));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::jmp(Operand::label(Label(done_block))));
                self.link_branch_edge(BasicBlockId(done_block));

                self.start_synthetic_block(done_block);
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::mov_rr(rdx, dst, is_64));
                return;
            }
        } else {
            self.current_block_mut()
                .instructions
                .push(Amd64Instr::imm(rdx, 0, false));
        }

        self.current_block_mut()
            .instructions
            .push(Amd64Instr::div(Operand::reg(rhs), is_signed, is_64));
        self.current_block_mut().instructions.push(Amd64Instr::mov_rr(
            if is_div { rax } else { rdx },
            dst,
            is_64,
        ));
    }

    fn link_branch_edge(&mut self, target: BasicBlock) {
        let Some(current) = self.current_block else {
            return;
        };
        let target_idx = self.ensure_block(target);
        let current_id = self.blocks[current].id;
        let target_id = target.0 as i32;
        if !self.blocks[current].succs.contains(&target_id) {
            self.blocks[current].succs.push(target_id);
        }
        if !self.blocks[target_idx].preds.contains(&current_id) {
            self.blocks[target_idx].preds.push(current_id);
        }
    }

    fn lower_call_arguments(&mut self, abi: &FunctionAbi, args: &[Value]) {
        for (value, arg) in args.iter().copied().zip(&abi.args) {
            let src = self.compiler().v_reg_of(value);
            if arg.kind == AbiArgKind::Reg {
                self.insert_move(arg.reg, src, arg.ty);
            } else if arg.ty.is_int() {
                self.push(Amd64Instr::mov_rm(
                    src,
                    Operand::mem(AddressMode::imm_rbp((arg.offset + 16) as i32 as u32)),
                    (arg.ty.bits() / 8) as u8,
                ));
            } else {
                self.push(Amd64Instr::xmm_mov_rm(
                    match arg.ty {
                        Type::F32 => SseOpcode::Movss,
                        Type::F64 => SseOpcode::Movsd,
                        Type::V128 => SseOpcode::Movdqu,
                        Type::I32 | Type::I64 | Type::Invalid => unreachable!(),
                    },
                    src,
                    Operand::mem(AddressMode::imm_rbp((arg.offset + 16) as i32 as u32)),
                ));
            }
        }
    }

    fn lower_call_results(&mut self, instruction: &Instruction, abi: &FunctionAbi) {
        let (ret0, rest) = instruction.returns();
        for (index, value) in std::iter::once(ret0)
            .filter(|value| value.valid())
            .chain(rest.as_slice().iter().copied())
            .enumerate()
        {
            let ret = &abi.rets[index];
            let dst = self.compiler().v_reg_of(value);
            if ret.kind == AbiArgKind::Reg {
                self.insert_move(dst, ret.reg, ret.ty);
            } else if ret.ty.is_int() {
                self.push(Amd64Instr::mov64_mr(
                    Operand::mem(AddressMode::imm_rbp((ret.offset + 16) as i32 as u32)),
                    dst,
                ));
            } else {
                self.push(Amd64Instr::xmm_unary_rm_r(
                    match ret.ty {
                        Type::F32 => SseOpcode::Movss,
                        Type::F64 => SseOpcode::Movsd,
                        Type::V128 => SseOpcode::Movdqu,
                        Type::I32 | Type::I64 | Type::Invalid => unreachable!(),
                    },
                    Operand::mem(AddressMode::imm_rbp((ret.offset + 16) as i32 as u32)),
                    dst,
                ));
            }
        }
    }
}

impl BackendMachine for Amd64Machine {
    fn start_lowering_function(&mut self, max_block_id: BasicBlockId) {
        self.blocks.clear();
        self.block_order.clear();
        for id in 0..=max_block_id.0 {
            self.blocks.push(Amd64Block {
                id: id as i32,
                ..Amd64Block::default()
            });
        }
        self.current_block = None;
        self.spill_slot_size = 0;
    }

    fn link_adjacent_blocks(&mut self, prev: BasicBlock, next: BasicBlock) {
        let prev_idx = self.ensure_block(prev);
        let next_idx = self.ensure_block(next);
        if !self.blocks[prev_idx].succs.contains(&(next.0 as i32)) {
            self.blocks[prev_idx].succs.push(next.0 as i32);
        }
        if !self.blocks[next_idx].preds.contains(&(prev.0 as i32)) {
            self.blocks[next_idx].preds.push(prev.0 as i32);
        }
    }

    fn start_block(&mut self, block: BasicBlock) {
        let idx = self.ensure_block(block);
        self.current_block = Some(idx);
        self.blocks[idx].entry = block.is_entry();
        let id = block.0 as i32;
        if !self.block_order.contains(&id) {
            self.block_order.push(id);
        }
        let params = if let Some(compiler) = self.compiler {
            unsafe {
                compiler
                    .as_ref()
                    .ssa_builder()
                    .block(block)
                    .params
                    .iter()
                    .map(|value| compiler.as_ref().v_reg_of(value))
                    .collect()
            }
        } else {
            Vec::new()
        };
        self.blocks[idx].params = params;
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

    fn lower_single_branch(&mut self, branch: &Instruction) {
        match branch.opcode {
            Opcode::Jump => {
                let (_, _, target) = branch.branch_data();
                self.link_branch_edge(target);
                self.push(Amd64Instr::jmp(Operand::label(Label(target.0))));
            }
            Opcode::Return => self.insert_return(),
            _ => {}
        }
    }

    fn lower_conditional_branch(&mut self, branch: &Instruction) {
        let (cond, _, target) = branch.branch_data();
        let cond_reg = self.compiler().v_reg_of(cond);
        self.push(Amd64Instr::cmp_rmi_r(
            Operand::imm32(0),
            cond_reg,
            true,
            true,
        ));
        let cc = match branch.opcode {
            Opcode::Brz => Cond::Z,
            Opcode::Brnz => Cond::NZ,
            _ => return,
        };
        self.link_branch_edge(target);
        self.push(Amd64Instr::jmp_if(cc, Operand::label(Label(target.0))));
    }

    fn lower_instr(&mut self, instruction: &Instruction) {
        match instruction.opcode {
            Opcode::Iconst | Opcode::F32const | Opcode::F64const => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                self.current_block_mut().instructions.extend(lower_constant(
                    dst,
                    instruction.typ,
                    instruction.u1,
                ));
            }
            Opcode::Iadd
            | Opcode::Isub
            | Opcode::Imul
            | Opcode::Band
            | Opcode::Bor
            | Opcode::Bxor => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let lhs = self.compiler().v_reg_of(instruction.v);
                let rhs = self.compiler().v_reg_of(instruction.v2);
                let is_64 = instruction.typ.bits() == 64;
                let op = match instruction.opcode {
                    Opcode::Iadd => super::AluRmiROpcode::Add,
                    Opcode::Isub => super::AluRmiROpcode::Sub,
                    Opcode::Band => super::AluRmiROpcode::And,
                    Opcode::Bor => super::AluRmiROpcode::Or,
                    Opcode::Bxor => super::AluRmiROpcode::Xor,
                    Opcode::Imul => super::AluRmiROpcode::Mul,
                    _ => unreachable!(),
                };
                let lhs_def = self.compiler().value_definition(instruction.v);
                let rhs_def = self.compiler().value_definition(instruction.v2);
                match instruction.opcode {
                    Opcode::Iadd | Opcode::Imul | Opcode::Band | Opcode::Bor | Opcode::Bxor => {
                        if self.compiler().match_instr(rhs_def, Opcode::Iconst) {
                            let imm = self
                                .compiler()
                                .ssa_builder()
                                .instruction_of_value(instruction.v2)
                                .expect("iconst instruction")
                                .u1 as u32;
                            self.current_block_mut()
                                .instructions
                                .push(Amd64Instr::mov_rr(lhs, dst, is_64));
                            self.current_block_mut()
                                .instructions
                                .push(Amd64Instr::alu_rmi_r(op, Operand::imm32(imm), dst, is_64));
                        } else if self.compiler().match_instr(lhs_def, Opcode::Iconst) {
                            let imm = self
                                .compiler()
                                .ssa_builder()
                                .instruction_of_value(instruction.v)
                                .expect("iconst instruction")
                                .u1 as u32;
                            self.current_block_mut()
                                .instructions
                                .push(Amd64Instr::mov_rr(rhs, dst, is_64));
                            self.current_block_mut()
                                .instructions
                                .push(Amd64Instr::alu_rmi_r(op, Operand::imm32(imm), dst, is_64));
                        } else {
                            self.current_block_mut()
                                .instructions
                                .push(Amd64Instr::mov_rr(lhs, dst, is_64));
                            self.current_block_mut()
                                .instructions
                                .push(Amd64Instr::alu_rmi_r(op, Operand::reg(rhs), dst, is_64));
                        }
                    }
                    Opcode::Isub => {
                        if self.compiler().match_instr(rhs_def, Opcode::Iconst) {
                            let imm = self
                                .compiler()
                                .ssa_builder()
                                .instruction_of_value(instruction.v2)
                                .expect("iconst instruction")
                                .u1 as u32;
                            self.current_block_mut()
                                .instructions
                                .push(Amd64Instr::mov_rr(lhs, dst, is_64));
                            self.current_block_mut()
                                .instructions
                                .push(Amd64Instr::alu_rmi_r(op, Operand::imm32(imm), dst, is_64));
                        } else {
                            self.current_block_mut()
                                .instructions
                                .push(Amd64Instr::mov_rr(lhs, dst, is_64));
                            self.current_block_mut()
                                .instructions
                                .push(Amd64Instr::alu_rmi_r(op, Operand::reg(rhs), dst, is_64));
                        }
                    }
                    _ => unreachable!(),
                }
            }
            Opcode::Clz | Opcode::Ctz | Opcode::Popcnt => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let src = self.compiler().v_reg_of(instruction.v);
                let is_64 = instruction.typ.bits() == 64;
                let op = match instruction.opcode {
                    Opcode::Clz => super::UnaryRmROpcode::Lzcnt,
                    Opcode::Ctz => super::UnaryRmROpcode::Tzcnt,
                    Opcode::Popcnt => super::UnaryRmROpcode::Popcnt,
                    _ => unreachable!(),
                };
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::unary_rm_r(op, Operand::reg(src), dst, is_64));
            }
            Opcode::Fcmp => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let (first, second) = self.lower_fcmp_to_flags(instruction);
                match second {
                    None => {
                        let tmp = self.compiler_mut().allocate_vreg(Type::I32);
                        self.current_block_mut()
                            .instructions
                            .push(Amd64Instr::setcc(first, tmp));
                        self.current_block_mut()
                            .instructions
                            .push(Amd64Instr::movzx_rm_r(ExtMode::BQ, Operand::reg(tmp), dst));
                    }
                    Some((second_cond, and)) => {
                        let tmp1 = self.compiler_mut().allocate_vreg(Type::I32);
                        let tmp2 = self.compiler_mut().allocate_vreg(Type::I32);
                        self.current_block_mut()
                            .instructions
                            .push(Amd64Instr::setcc(first, tmp1));
                        self.current_block_mut()
                            .instructions
                            .push(Amd64Instr::setcc(second_cond, tmp2));
                        self.current_block_mut().instructions.push(Amd64Instr::alu_rmi_r(
                            if and {
                                super::AluRmiROpcode::And
                            } else {
                                super::AluRmiROpcode::Or
                            },
                            Operand::reg(tmp1),
                            tmp2,
                            false,
                        ));
                        self.current_block_mut()
                            .instructions
                            .push(Amd64Instr::movzx_rm_r(ExtMode::BQ, Operand::reg(tmp2), dst));
                    }
                }
            }
            Opcode::Fadd | Opcode::Fsub | Opcode::Fmul | Opcode::Fdiv => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let lhs = self.compiler().v_reg_of(instruction.v);
                let rhs = self.compiler().v_reg_of(instruction.v2);
                let op = match (instruction.opcode, instruction.typ) {
                    (Opcode::Fadd, Type::F32) => SseOpcode::Addss,
                    (Opcode::Fadd, Type::F64) => SseOpcode::Addsd,
                    (Opcode::Fsub, Type::F32) => SseOpcode::Subss,
                    (Opcode::Fsub, Type::F64) => SseOpcode::Subsd,
                    (Opcode::Fmul, Type::F32) => SseOpcode::Mulss,
                    (Opcode::Fmul, Type::F64) => SseOpcode::Mulsd,
                    (Opcode::Fdiv, Type::F32) => SseOpcode::Divss,
                    (Opcode::Fdiv, Type::F64) => SseOpcode::Divsd,
                    _ => panic!("unsupported amd64 float arithmetic type: {:?}", instruction.typ),
                };
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::xmm_unary_rm_r(
                        match instruction.typ {
                            Type::F32 => SseOpcode::Movss,
                            Type::F64 => SseOpcode::Movsd,
                            _ => unreachable!(),
                        },
                        Operand::reg(lhs),
                        dst,
                    ));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::xmm_unary_rm_r(op, Operand::reg(rhs), dst));
            }
            Opcode::Sqrt => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let src = self.compiler().v_reg_of(instruction.v);
                let op = match instruction.typ {
                    Type::F32 => SseOpcode::Sqrtss,
                    Type::F64 => SseOpcode::Sqrtsd,
                    _ => panic!("unsupported amd64 sqrt type: {:?}", instruction.typ),
                };
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::xmm_unary_rm_r(op, Operand::reg(src), dst));
            }
            Opcode::Fpromote | Opcode::Fdemote => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let src = self.compiler().v_reg_of(instruction.v);
                let op = match instruction.opcode {
                    Opcode::Fpromote => SseOpcode::Cvtss2sd,
                    Opcode::Fdemote => SseOpcode::Cvtsd2ss,
                    _ => unreachable!(),
                };
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::xmm_unary_rm_r(op, Operand::reg(src), dst));
            }
            Opcode::Bitcast => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let src = self.compiler().v_reg_of(instruction.v);
                match (instruction.v.ty(), instruction.typ) {
                    (Type::I32, Type::F32) => self
                        .current_block_mut()
                        .instructions
                        .push(Amd64Instr::gpr_to_xmm(
                            SseOpcode::Movd,
                            Operand::reg(src),
                            dst,
                            false,
                        )),
                    (Type::F32, Type::I32) => self
                        .current_block_mut()
                        .instructions
                        .push(Amd64Instr::xmm_to_gpr(SseOpcode::Movd, src, dst, false)),
                    (Type::I64, Type::F64) => self
                        .current_block_mut()
                        .instructions
                        .push(Amd64Instr::gpr_to_xmm(
                            SseOpcode::Movq,
                            Operand::reg(src),
                            dst,
                            true,
                        )),
                    (Type::F64, Type::I64) => self
                        .current_block_mut()
                        .instructions
                        .push(Amd64Instr::xmm_to_gpr(SseOpcode::Movq, src, dst, true)),
                    _ => panic!(
                        "unsupported amd64 bitcast: {:?} -> {:?}",
                        instruction.v.ty(),
                        instruction.typ
                    ),
                }
            }
            Opcode::Ireduce => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let src = self.compiler().v_reg_of(instruction.v);
                match instruction.typ {
                    Type::I32 => self
                        .current_block_mut()
                        .instructions
                        .push(Amd64Instr::movzx_rm_r(ExtMode::LQ, Operand::reg(src), dst)),
                    _ => panic!("unsupported amd64 ireduce type: {:?}", instruction.typ),
                }
            }
            Opcode::FcvtFromSint => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let src = self.compiler().v_reg_of(instruction.v);
                let op = match (instruction.v.ty(), instruction.typ) {
                    (Type::I32, Type::F32) => SseOpcode::Cvtsi2ss,
                    (Type::I32, Type::F64) => SseOpcode::Cvtsi2sd,
                    (Type::I64, Type::F32) => SseOpcode::Cvtsi2ss,
                    (Type::I64, Type::F64) => SseOpcode::Cvtsi2sd,
                    _ => panic!(
                        "unsupported amd64 signed int-to-float conversion: {:?} -> {:?}",
                        instruction.v.ty(),
                        instruction.typ
                    ),
                };
                let src_64 = matches!(instruction.v.ty(), Type::I64);
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::gpr_to_xmm(op, Operand::reg(src), dst, src_64));
            }
            Opcode::FcvtFromUint => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let src = self.compiler().v_reg_of(instruction.v);
                let op = match (instruction.v.ty(), instruction.typ) {
                    (Type::I32, Type::F32) => SseOpcode::Cvtsi2ss,
                    (Type::I32, Type::F64) => SseOpcode::Cvtsi2sd,
                    (Type::I64, Type::F32) => SseOpcode::Cvtsi2ss,
                    (Type::I64, Type::F64) => SseOpcode::Cvtsi2sd,
                    _ => panic!(
                        "unsupported amd64 unsigned int-to-float conversion: {:?} -> {:?}",
                        instruction.v.ty(),
                        instruction.typ
                    ),
                };
                if instruction.v.ty() == Type::I32 {
                    let tmp = self.compiler_mut().allocate_vreg(Type::I32);
                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::movzx_rm_r(ExtMode::LQ, Operand::reg(src), tmp));
                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::gpr_to_xmm(op, Operand::reg(tmp), dst, true));
                } else {
                    let sign_block = self.next_synthetic_block_id();
                    let done_block = sign_block + 1;
                    let add_op = match instruction.typ {
                        Type::F32 => SseOpcode::Addss,
                        Type::F64 => SseOpcode::Addsd,
                        _ => unreachable!(),
                    };
                    let tmp = self.compiler_mut().allocate_vreg(Type::I64);
                    let tmp2 = self.compiler_mut().allocate_vreg(Type::I64);
                    let rcx =
                        crate::backend::VReg::from_real_reg(super::reg::RCX, crate::backend::RegType::Int);

                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::cmp_rmi_r(Operand::reg(src), src, false, true));
                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::jmp_if(Cond::S, Operand::label(Label(sign_block))));
                    self.link_branch_edge(BasicBlockId(sign_block));

                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::gpr_to_xmm(op, Operand::reg(src), dst, true));
                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::jmp(Operand::label(Label(done_block))));
                    self.link_branch_edge(BasicBlockId(done_block));

                    self.start_synthetic_block(sign_block);
                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::mov_rr(src, tmp, true));
                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::imm(rcx, 1, false));
                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::shift_r(super::instr::ShiftROpcode::Shr, tmp, true));
                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::mov_rr(src, tmp2, true));
                    self.current_block_mut().instructions.push(Amd64Instr::alu_rmi_r(
                        super::instr::AluRmiROpcode::And,
                        Operand::imm32(1),
                        tmp2,
                        true,
                    ));
                    self.current_block_mut().instructions.push(Amd64Instr::alu_rmi_r(
                        super::instr::AluRmiROpcode::Or,
                        Operand::reg(tmp2),
                        tmp,
                        true,
                    ));
                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::gpr_to_xmm(op, Operand::reg(tmp), dst, true));
                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::xmm_unary_rm_r(add_op, Operand::reg(dst), dst));
                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::jmp(Operand::label(Label(done_block))));
                    self.link_branch_edge(BasicBlockId(done_block));

                    self.start_synthetic_block(done_block);
                }
            }
            Opcode::Ceil | Opcode::Floor | Opcode::Trunc | Opcode::Nearest => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let src = self.compiler().v_reg_of(instruction.v);
                let op = match instruction.typ {
                    Type::F32 => SseOpcode::Roundss,
                    Type::F64 => SseOpcode::Roundsd,
                    _ => panic!("unsupported amd64 round type: {:?}", instruction.typ),
                };
                let mode = match instruction.opcode {
                    Opcode::Nearest => 0,
                    Opcode::Floor => 1,
                    Opcode::Ceil => 2,
                    Opcode::Trunc => 3,
                    _ => unreachable!(),
                };
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::xmm_unary_rm_r_imm(
                        op,
                        mode,
                        Operand::reg(src),
                        dst,
                    ));
            }
            Opcode::Fmin | Opcode::Fmax => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let lhs = self.compiler().v_reg_of(instruction.v);
                let rhs = self.compiler().v_reg_of(instruction.v2);
                let is_min = matches!(instruction.opcode, Opcode::Fmin);
                let (cmp_op, diff_op, same_op, nan_op) = match (instruction.typ, is_min) {
                    (Type::F32, true) => (
                        SseOpcode::Ucomiss,
                        SseOpcode::Minps,
                        SseOpcode::Orps,
                        SseOpcode::Addss,
                    ),
                    (Type::F32, false) => (
                        SseOpcode::Ucomiss,
                        SseOpcode::Maxps,
                        SseOpcode::Andps,
                        SseOpcode::Addss,
                    ),
                    (Type::F64, true) => (
                        SseOpcode::Ucomisd,
                        SseOpcode::Minpd,
                        SseOpcode::Orpd,
                        SseOpcode::Addsd,
                    ),
                    (Type::F64, false) => (
                        SseOpcode::Ucomisd,
                        SseOpcode::Maxpd,
                        SseOpcode::Andpd,
                        SseOpcode::Addsd,
                    ),
                    _ => panic!("unsupported amd64 min/max type: {:?}", instruction.typ),
                };

                let nan_block = self.next_synthetic_block_id();
                let diff_block = nan_block + 1;
                let done_block = diff_block + 1;

                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::xmm_unary_rm_r(
                        SseOpcode::Movdqu,
                        Operand::reg(lhs),
                        dst,
                    ));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::xmm_cmp_rm_r(cmp_op, Operand::reg(rhs), dst));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::jmp_if(Cond::NZ, Operand::label(Label(diff_block))));
                self.link_branch_edge(BasicBlockId(diff_block));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::jmp_if(Cond::P, Operand::label(Label(nan_block))));
                self.link_branch_edge(BasicBlockId(nan_block));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::xmm_unary_rm_r(same_op, Operand::reg(rhs), dst));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::jmp(Operand::label(Label(done_block))));
                self.link_branch_edge(BasicBlockId(done_block));

                self.start_synthetic_block(nan_block);
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::xmm_unary_rm_r(nan_op, Operand::reg(rhs), dst));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::jmp(Operand::label(Label(done_block))));
                self.link_branch_edge(BasicBlockId(done_block));

                self.start_synthetic_block(diff_block);
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::xmm_unary_rm_r(diff_op, Operand::reg(rhs), dst));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::jmp(Operand::label(Label(done_block))));
                self.link_branch_edge(BasicBlockId(done_block));

                self.start_synthetic_block(done_block);
            }
            Opcode::Icmp => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let cond = Self::integer_cmp_cond_from_u8(instruction.u1 as u8);
                let cc = self.lower_icmp_to_flags(instruction.v, instruction.v2, cond);
                let tmp = self.compiler_mut().allocate_vreg(Type::I32);
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::setcc(cc, tmp));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::movzx_rm_r(ExtMode::BQ, Operand::reg(tmp), dst));
            }
            Opcode::Sdiv | Opcode::Udiv | Opcode::Srem | Opcode::Urem => {
                let is_div = matches!(instruction.opcode, Opcode::Sdiv | Opcode::Udiv);
                let is_signed = matches!(instruction.opcode, Opcode::Sdiv | Opcode::Srem);
                self.lower_idivrem(instruction, is_div, is_signed);
            }
            Opcode::Select => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let cond = self.compiler().v_reg_of(instruction.v);
                let v_true = self.compiler().v_reg_of(instruction.v2);
                let v_false = self.compiler().v_reg_of(instruction.v3);
                let is_64 = instruction.typ.bits() == 64;
                match instruction.typ {
                    Type::I32 | Type::I64 => {
                        self.current_block_mut()
                            .instructions
                            .push(Amd64Instr::cmp_rmi_r(
                                Operand::reg(cond),
                                cond,
                                false,
                                false,
                            ));
                        self.current_block_mut()
                            .instructions
                            .push(Amd64Instr::mov_rr(v_false, dst, is_64));
                        self.current_block_mut()
                            .instructions
                            .push(Amd64Instr::cmove(Cond::NZ, Operand::reg(v_true), dst, is_64));
                    }
                    _ => panic!("unsupported amd64 select type: {:?}", instruction.typ),
                }
            }
            Opcode::UExtend => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let src = self.compiler().v_reg_of(instruction.v);
                match instruction.typ {
                    Type::I64 => {
                        self.current_block_mut()
                            .instructions
                            .push(Amd64Instr::movzx_rm_r(ExtMode::LQ, Operand::reg(src), dst));
                    }
                    _ => panic!("unsupported amd64 uextend type: {:?}", instruction.typ),
                }
            }
            Opcode::SExtend => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let src = self.compiler().v_reg_of(instruction.v);
                match instruction.typ {
                    Type::I64 => {
                        self.current_block_mut()
                            .instructions
                            .push(Amd64Instr::movsx_rm_r(ExtMode::LQ, Operand::reg(src), dst));
                    }
                    _ => panic!("unsupported amd64 sextend type: {:?}", instruction.typ),
                }
            }
            Opcode::Ishl | Opcode::Ushr | Opcode::Sshr | Opcode::Rotl | Opcode::Rotr => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let lhs = self.compiler().v_reg_of(instruction.v);
                let rhs = self.compiler().v_reg_of(instruction.v2);
                let is_64 = instruction.typ.bits() == 64;
                let tmp = crate::backend::VReg::from_real_reg(
                    super::reg::R11,
                    crate::backend::RegType::Int,
                );
                let rcx = crate::backend::VReg::from_real_reg(
                    super::reg::RCX,
                    crate::backend::RegType::Int,
                );
                let op = match instruction.opcode {
                    Opcode::Ishl => super::instr::ShiftROpcode::Shl,
                    Opcode::Ushr => super::instr::ShiftROpcode::Shr,
                    Opcode::Sshr => super::instr::ShiftROpcode::Sar,
                    Opcode::Rotl => super::instr::ShiftROpcode::Rol,
                    Opcode::Rotr => super::instr::ShiftROpcode::Ror,
                    _ => unreachable!(),
                };
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::mov_rr(lhs, tmp, is_64));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::mov_rr(rhs, rcx, false));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::shift_r(op, tmp, is_64));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::mov_rr(tmp, dst, is_64));
            }
            Opcode::Load
            | Opcode::Uload8
            | Opcode::Sload8
            | Opcode::Uload16
            | Opcode::Sload16
            | Opcode::Uload32
            | Opcode::Sload32 => {
                let (ptr, offset, typ) = instruction.load_data();
                let dst = self.compiler().v_reg_of(instruction.return_());
                let ptr = self.compiler().v_reg_of(ptr);
                let mem = mem_operand_from_base(ptr, offset);
                let inst = match instruction.opcode {
                    Opcode::Load => match typ {
                        Type::I32 => Amd64Instr::movzx_rm_r(ExtMode::LQ, mem, dst),
                        Type::I64 => Amd64Instr::mov64_mr(mem, dst),
                        Type::F32 => Amd64Instr::xmm_unary_rm_r(SseOpcode::Movss, mem, dst),
                        Type::F64 => Amd64Instr::xmm_unary_rm_r(SseOpcode::Movsd, mem, dst),
                        Type::V128 => Amd64Instr::xmm_unary_rm_r(SseOpcode::Movdqu, mem, dst),
                        Type::Invalid => panic!("invalid load type"),
                    },
                    Opcode::Uload8 => match typ {
                        Type::I32 => Amd64Instr::movzx_rm_r(ExtMode::BL, mem, dst),
                        Type::I64 => Amd64Instr::movzx_rm_r(ExtMode::BQ, mem, dst),
                        _ => panic!("unsupported amd64 uload8 type: {:?}", typ),
                    },
                    Opcode::Sload8 => match typ {
                        Type::I32 => Amd64Instr::movsx_rm_r(ExtMode::BL, mem, dst),
                        Type::I64 => Amd64Instr::movsx_rm_r(ExtMode::BQ, mem, dst),
                        _ => panic!("unsupported amd64 sload8 type: {:?}", typ),
                    },
                    Opcode::Uload16 => match typ {
                        Type::I32 => Amd64Instr::movzx_rm_r(ExtMode::WL, mem, dst),
                        Type::I64 => Amd64Instr::movzx_rm_r(ExtMode::WQ, mem, dst),
                        _ => panic!("unsupported amd64 uload16 type: {:?}", typ),
                    },
                    Opcode::Sload16 => match typ {
                        Type::I32 => Amd64Instr::movsx_rm_r(ExtMode::WL, mem, dst),
                        Type::I64 => Amd64Instr::movsx_rm_r(ExtMode::WQ, mem, dst),
                        _ => panic!("unsupported amd64 sload16 type: {:?}", typ),
                    },
                    Opcode::Uload32 => match typ {
                        Type::I64 => Amd64Instr::movzx_rm_r(ExtMode::LQ, mem, dst),
                        _ => panic!("unsupported amd64 uload32 type: {:?}", typ),
                    },
                    Opcode::Sload32 => match typ {
                        Type::I64 => Amd64Instr::movsx_rm_r(ExtMode::LQ, mem, dst),
                        _ => panic!("unsupported amd64 sload32 type: {:?}", typ),
                    },
                    _ => unreachable!(),
                };
                self.current_block_mut().instructions.push(inst);
            }
            Opcode::Store | Opcode::Istore8 | Opcode::Istore16 | Opcode::Istore32 => {
                let (value, ptr, offset, size_bits) = instruction.store_data();
                let value = self.compiler().v_reg_of(value);
                let ptr = self.compiler().v_reg_of(ptr);
                let mem = mem_operand_from_base(ptr, offset);
                let inst = match instruction.opcode {
                    Opcode::Store | Opcode::Istore8 | Opcode::Istore16 | Opcode::Istore32
                        if matches!(instruction.v.ty(), Type::I32 | Type::I64) =>
                    {
                        Amd64Instr::mov_rm(value, mem, size_bits / 8)
                    }
                    Opcode::Store if matches!(instruction.v.ty(), Type::F32) => {
                        Amd64Instr::xmm_mov_rm(SseOpcode::Movss, value, mem)
                    }
                    Opcode::Store if matches!(instruction.v.ty(), Type::F64) => {
                        Amd64Instr::xmm_mov_rm(SseOpcode::Movsd, value, mem)
                    }
                    Opcode::Store if matches!(instruction.v.ty(), Type::V128) => {
                        Amd64Instr::xmm_mov_rm(SseOpcode::Movdqu, value, mem)
                    }
                    _ => panic!("unsupported amd64 store type: {:?}", instruction.v.ty()),
                };
                self.current_block_mut().instructions.push(inst);
            }
            Opcode::ExitWithCode => {
                let (ctx, code) = instruction.exit_with_code_data();
                let exec_ctx = self.compiler().v_reg_of(ctx);
                self.emit_exit_with_code(exec_ctx, code);
            }
            Opcode::ExitIfTrueWithCode => {
                let (ctx, cond, code) = instruction.exit_if_true_with_code_data();
                let exec_ctx = self.compiler().v_reg_of(ctx);
                let current_id = self.current_block_mut().id as u32;
                let cont_id = self.next_synthetic_block_id();
                let cond_def = self.compiler().value_definition(cond);
                let continue_cond = if self.compiler().match_instr(cond_def, Opcode::Icmp) {
                    let icmp = self
                        .compiler()
                        .ssa_builder()
                        .instruction_of_value(cond)
                        .expect("icmp value must map to instruction");
                    let cond = Self::integer_cmp_cond_from_u8(icmp.u1 as u8);
                    self.lower_icmp_to_flags(icmp.v, icmp.v2, cond).invert()
                } else {
                    let cond_reg = self.compiler().v_reg_of(cond);
                    self.current_block_mut()
                        .instructions
                        .push(Amd64Instr::cmp_rmi_r(
                            Operand::reg(cond_reg),
                            cond_reg,
                            false,
                            false,
                        ));
                    Cond::Z
                };
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::jmp_if(continue_cond, Operand::label(Label(cont_id))));
                self.link_adjacent_blocks(BasicBlockId(current_id), BasicBlockId(cont_id));
                self.emit_exit_with_code(exec_ctx, code);
                self.start_synthetic_block(cont_id);
            }
            Opcode::Call => {
                let (func_ref, sig, args) = instruction.call_data();
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
                let abi = self.compiler_mut().get_function_abi(&signature).clone();
                self.lower_call_arguments(&abi, args);
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::call(func_ref.0 as u64, abi_info));
                self.lower_call_results(instruction, &abi);
            }
            Opcode::CallIndirect => {
                let (func_ptr, sig, args) = instruction.call_indirect_data();
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
                let abi = self.compiler_mut().get_function_abi(&signature).clone();
                self.lower_call_arguments(&abi, args);
                let func_ptr_reg = self.compiler().v_reg_of(func_ptr);
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::mov_rr(
                        func_ptr_reg,
                        crate::backend::VReg::from_real_reg(
                            super::reg::R10,
                            crate::backend::RegType::Int,
                        ),
                        true,
                    ));
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::call_indirect(
                        Operand::reg(crate::backend::VReg::from_real_reg(
                            super::reg::R10,
                            crate::backend::RegType::Int,
                        )),
                        abi_info,
                    ));
                self.lower_call_results(instruction, &abi);
            }
            _ => panic!("unhandled amd64 opcode lowering: {:?}", instruction.opcode),
        }
    }

    fn reset(&mut self) {
        self.blocks.clear();
        self.block_order.clear();
        self.current_block = None;
        self.clobbered.clear();
        self.spill_slots.clear();
        self.spill_slot_size = 0;
        self.stack_check_disabled = false;
    }

    fn insert_move(&mut self, dst: VReg, src: VReg, ty: Type) {
        if ty.is_int() {
            self.push(Amd64Instr::mov_rr(src, dst, ty.bits() == 64));
        } else {
            let op = match ty {
                Type::F32 => SseOpcode::Movss,
                Type::F64 => SseOpcode::Movsd,
                Type::V128 => SseOpcode::Movdqu,
                Type::I32 | Type::I64 | Type::Invalid => unreachable!(),
            };
            self.push(Amd64Instr::xmm_unary_rm_r(op, Operand::reg(src), dst));
        }
    }

    fn insert_return(&mut self) {
        self.push(Amd64Instr::ret());
    }

    fn insert_load_constant_block_arg(&mut self, instr: &Instruction, dst: VReg) {
        self.current_block_mut()
            .instructions
            .extend(lower_constant(dst, instr.typ, instr.u1));
    }

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

    fn reg_alloc(&mut self) {
        do_regalloc(self);
    }

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

    fn lower_params(&mut self, params: &[Value]) {
        let abi = self.current_abi.clone();
        let mut lowered = Vec::new();
        for (value, arg) in params.iter().copied().zip(&abi.args) {
            if !value.valid() {
                continue;
            }
            let reg = self.compiler().v_reg_of(value);
            if arg.kind == AbiArgKind::Reg {
                if arg.ty.is_int() {
                    lowered.push(Amd64Instr::mov_rr(arg.reg, reg, arg.ty.bits() == 64));
                } else {
                    lowered.push(Amd64Instr::xmm_unary_rm_r(
                        match arg.ty {
                            Type::F32 => SseOpcode::Movss,
                            Type::F64 => SseOpcode::Movsd,
                            _ => SseOpcode::Movdqu,
                        },
                        Operand::reg(arg.reg),
                        reg,
                    ));
                }
            } else if arg.ty.is_int() {
                lowered.push(Amd64Instr::mov64_mr(
                    Operand::mem(AddressMode::imm_rbp((arg.offset + 16) as u32)),
                    reg,
                ));
            } else {
                lowered.push(Amd64Instr::xmm_unary_rm_r(
                    match arg.ty {
                        Type::F32 => SseOpcode::Movss,
                        Type::F64 => SseOpcode::Movsd,
                        _ => SseOpcode::Movdqu,
                    },
                    Operand::mem(AddressMode::imm_rbp((arg.offset + 16) as u32)),
                    reg,
                ));
            }
        }
        self.current_block_mut().instructions.splice(0..0, lowered);
    }

    fn lower_returns(&mut self, returns: &[Value]) {
        for (index, value) in returns.iter().copied().enumerate().rev() {
            let ret = &self.current_abi.rets[index];
            let reg = self.compiler().v_reg_of(value);
            if ret.kind == AbiArgKind::Reg {
                self.insert_move(ret.reg, reg, ret.ty);
            } else if ret.ty.is_int() {
                self.push(Amd64Instr::mov_rm(
                    reg,
                    Operand::mem(AddressMode::imm_rbp((ret.offset + 16) as i32 as u32)),
                    8,
                ));
            } else {
                self.push(Amd64Instr::xmm_mov_rm(
                    match ret.ty {
                        Type::F32 => SseOpcode::Movss,
                        Type::F64 => SseOpcode::Movsd,
                        Type::V128 => SseOpcode::Movdqu,
                        Type::I32 | Type::I64 | Type::Invalid => unreachable!(),
                    },
                    reg,
                    Operand::mem(AddressMode::imm_rbp((ret.offset + 16) as i32 as u32)),
                ));
            }
        }
    }

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
    use std::ptr::NonNull;

    use super::Amd64Machine;
    use crate::backend::isa::amd64::{Amd64Instr, Label, Operand};
    use crate::backend::machine::Machine;
    use crate::backend::{
        CompilerContext, FunctionAbi, RegType, SSAValueDefinition, SourceOffsetInfo, VReg,
    };
    use crate::ssa::{
        cmp::{FloatCmpCond, IntegerCmpCond}, BasicBlockId, Builder, FuncRef, Opcode, Signature,
        SourceOffset, Type, Value,
    };
    use crate::wazevoapi::ExitCode;

    struct TestCompilerContext {
        builder: Builder,
        regs: Vec<VReg>,
        defs: Vec<SSAValueDefinition>,
        abi: FunctionAbi,
        buf: Vec<u8>,
        source_offsets: Vec<SourceOffsetInfo>,
    }

    impl Default for TestCompilerContext {
        fn default() -> Self {
            Self {
                builder: Builder::new(),
                regs: Vec::new(),
                defs: Vec::new(),
                abi: FunctionAbi::default(),
                buf: Vec::new(),
                source_offsets: Vec::new(),
            }
        }
    }

    impl CompilerContext for TestCompilerContext {
        fn ssa_builder(&self) -> &Builder {
            &self.builder
        }

        fn format(&self) -> String {
            String::new()
        }

        fn allocate_vreg(&mut self, ty: Type) -> VReg {
            let id = crate::backend::VREG_ID_NON_RESERVED_BEGIN + self.regs.len() as u32;
            let vreg = VReg(id as u64).set_reg_type(RegType::of(ty));
            self.regs.push(vreg);
            vreg
        }

        fn value_definition(&self, value: Value) -> SSAValueDefinition {
            self.defs.get(value.id().0 as usize).copied().unwrap_or_default()
        }

        fn v_reg_of(&self, value: Value) -> VReg {
            self.regs[value.id().0 as usize]
        }

        fn type_of(&self, vreg: VReg) -> Type {
            match vreg.reg_type() {
                RegType::Int => Type::I64,
                RegType::Float => Type::F64,
                RegType::Invalid => Type::Invalid,
            }
        }

        fn match_instr(&self, def: SSAValueDefinition, opcode: Opcode) -> bool {
            def.instr
                .map(|id| self.builder.instruction(id).opcode == opcode)
                .unwrap_or(false)
        }

        fn match_instr_one_of(&self, def: SSAValueDefinition, opcodes: &[Opcode]) -> Opcode {
            let Some(id) = def.instr else {
                return Opcode::Undefined;
            };
            let opcode = self.builder.instruction(id).opcode;
            if opcodes.contains(&opcode) {
                opcode
            } else {
                Opcode::Undefined
            }
        }

        fn add_relocation_info(&mut self, _func_ref: FuncRef, _is_tail_call: bool) {}

        fn add_source_offset_info(
            &mut self,
            _executable_offset: i64,
            _source_offset: SourceOffset,
        ) {
        }

        fn source_offset_info(&self) -> &[SourceOffsetInfo] {
            &self.source_offsets
        }

        fn emit_byte(&mut self, b: u8) {
            self.buf.push(b);
        }

        fn emit4_bytes(&mut self, b: u32) {
            self.buf.extend_from_slice(&b.to_le_bytes());
        }

        fn emit8_bytes(&mut self, b: u64) {
            self.buf.extend_from_slice(&b.to_le_bytes());
        }

        fn buf(&self) -> &[u8] {
            &self.buf
        }

        fn buf_mut(&mut self) -> &mut Vec<u8> {
            &mut self.buf
        }

        fn get_function_abi(&mut self, _sig: &Signature) -> &FunctionAbi {
            &self.abi
        }
    }

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

    #[test]
    fn float_moves_use_sse_forms() {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));
        m.insert_move(
            VReg::from_real_reg(18, RegType::Float),
            VReg::from_real_reg(17, RegType::Float),
            crate::ssa::Type::F64,
        );
        assert!(m.format().contains("movsd %xmm0, %xmm1"));
    }

    #[test]
    fn stack_returns_use_caller_visible_area() {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));
        m.current_abi.rets = vec![crate::backend::AbiArg {
            index: 0,
            kind: crate::backend::AbiArgKind::Stack,
            reg: VReg::INVALID,
            offset: 0,
            ty: Type::I64,
        }];

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![VReg::from_real_reg(1, RegType::Int)];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);
        m.lower_returns(&[Value(0).with_type(Type::I64)]);

        assert!(m.format().contains("16(%rbp)"));
    }

    #[test]
    fn encode_all_resolves_block_label_branches() {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(1));
        m.start_block(BasicBlockId(0));
        m.insert_return();
        m.start_block(BasicBlockId(1));
        m.push(Amd64Instr::jmp(Operand::label(Label(0))));

        let bytes = m.encode_all().unwrap();
        assert_eq!(bytes, vec![0xC3, 0xE9, 0xFA, 0xFF, 0xFF, 0xFF]);
    }

    fn lower_int_binary_opcode(opcode: Opcode) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(1, RegType::Int),
            VReg::from_real_reg(2, RegType::Int),
            VReg::from_real_reg(4, RegType::Int),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(opcode);
        instruction.v = Value(0).with_type(Type::I64);
        instruction.v2 = Value(1).with_type(Type::I64);
        instruction.r_value = Value(2).with_type(Type::I64);
        instruction.typ = Type::I64;

        m.lower_instr(&instruction);
        m.format()
    }

    #[test]
    fn lowers_band_to_amd64_and() {
        let formatted = lower_int_binary_opcode(Opcode::Band);
        assert!(formatted.contains("and "));
    }

    #[test]
    fn lowers_bor_to_amd64_or() {
        let formatted = lower_int_binary_opcode(Opcode::Bor);
        assert!(formatted.contains("or "));
    }

    #[test]
    fn lowers_bxor_to_amd64_xor() {
        let formatted = lower_int_binary_opcode(Opcode::Bxor);
        assert!(formatted.contains("xor "));
    }

    fn lower_shift_opcode(opcode: Opcode) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(1, RegType::Int),
            VReg::from_real_reg(4, RegType::Int),
            VReg::from_real_reg(8, RegType::Int),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(opcode);
        instruction.v = Value(0).with_type(Type::I64);
        instruction.v2 = Value(1).with_type(Type::I64);
        instruction.r_value = Value(2).with_type(Type::I64);
        instruction.typ = Type::I64;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_select_opcode(typ: Type) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(1, RegType::Int),
            VReg::from_real_reg(4, RegType::Int),
            VReg::from_real_reg(2, RegType::Int),
            VReg::from_real_reg(7, RegType::Int),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::Select);
        instruction.v = Value(0).with_type(Type::I32);
        instruction.v2 = Value(1).with_type(typ);
        instruction.v3 = Value(2).with_type(typ);
        instruction.r_value = Value(3).with_type(typ);
        instruction.typ = typ;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_uextend_opcode() -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(1, RegType::Int),
            VReg::from_real_reg(7, RegType::Int),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::UExtend);
        instruction.v = Value(0).with_type(Type::I32);
        instruction.r_value = Value(1).with_type(Type::I64);
        instruction.typ = Type::I64;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_sextend_opcode() -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(1, RegType::Int),
            VReg::from_real_reg(7, RegType::Int),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::SExtend);
        instruction.v = Value(0).with_type(Type::I32);
        instruction.r_value = Value(1).with_type(Type::I64);
        instruction.typ = Type::I64;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_exit_with_code(exit_code: ExitCode) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![VReg::from_real_reg(1, RegType::Int)];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::ExitWithCode);
        instruction.v = Value(0).with_type(Type::I64);
        instruction.u1 = exit_code.raw() as u64;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_divrem_opcode(opcode: Opcode, ty: Type) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(3, RegType::Int),
            VReg::from_real_reg(4, RegType::Int),
            VReg::from_real_reg(7, RegType::Int),
            VReg::from_real_reg(8, RegType::Int),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(opcode);
        instruction.v = Value(0).with_type(ty);
        instruction.v2 = Value(1).with_type(ty);
        instruction.v3 = Value(2).with_type(Type::I64);
        instruction.r_value = Value(3).with_type(ty);
        instruction.typ = ty;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_float_binary_opcode(opcode: Opcode, ty: Type) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM0, RegType::Float),
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM1, RegType::Float),
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM2, RegType::Float),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(opcode);
        instruction.v = Value(0).with_type(ty);
        instruction.v2 = Value(1).with_type(ty);
        instruction.r_value = Value(2).with_type(ty);
        instruction.typ = ty;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_sqrt_opcode(ty: Type) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM0, RegType::Float),
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM2, RegType::Float),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::Sqrt);
        instruction.v = Value(0).with_type(ty);
        instruction.r_value = Value(1).with_type(ty);
        instruction.typ = ty;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_float_convert_opcode(opcode: Opcode, src_ty: Type, dst_ty: Type) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM0, RegType::Float),
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM2, RegType::Float),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(opcode);
        instruction.v = Value(0).with_type(src_ty);
        instruction.r_value = Value(1).with_type(dst_ty);
        instruction.typ = dst_ty;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_fcvt_from_sint_opcode(src_ty: Type, dst_ty: Type) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(crate::backend::isa::amd64::reg::RAX, RegType::Int),
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM2, RegType::Float),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::FcvtFromSint);
        instruction.v = Value(0).with_type(src_ty);
        instruction.r_value = Value(1).with_type(dst_ty);
        instruction.typ = dst_ty;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_bitcast_opcode(src_ty: Type, dst_ty: Type) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(
                if matches!(src_ty, Type::I32 | Type::I64) {
                    crate::backend::isa::amd64::reg::RAX
                } else {
                    crate::backend::isa::amd64::reg::XMM0
                },
                RegType::of(src_ty),
            ),
            VReg::from_real_reg(
                if matches!(dst_ty, Type::I32 | Type::I64) {
                    crate::backend::isa::amd64::reg::RAX
                } else {
                    crate::backend::isa::amd64::reg::XMM2
                },
                RegType::of(dst_ty),
            ),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::Bitcast);
        instruction.v = Value(0).with_type(src_ty);
        instruction.r_value = Value(1).with_type(dst_ty);
        instruction.typ = dst_ty;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_ireduce_opcode(src_ty: Type, dst_ty: Type) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(crate::backend::isa::amd64::reg::RAX, RegType::of(src_ty)),
            VReg::from_real_reg(crate::backend::isa::amd64::reg::RDX, RegType::of(dst_ty)),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::Ireduce);
        instruction.v = Value(0).with_type(src_ty);
        instruction.r_value = Value(1).with_type(dst_ty);
        instruction.typ = dst_ty;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_fcvt_from_uint_opcode(src_ty: Type, dst_ty: Type) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(crate::backend::isa::amd64::reg::RAX, RegType::Int),
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM2, RegType::Float),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::FcvtFromUint);
        instruction.v = Value(0).with_type(src_ty);
        instruction.r_value = Value(1).with_type(dst_ty);
        instruction.typ = dst_ty;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_round_opcode(opcode: Opcode, ty: Type) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM0, RegType::Float),
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM2, RegType::Float),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(opcode);
        instruction.v = Value(0).with_type(ty);
        instruction.r_value = Value(1).with_type(ty);
        instruction.typ = ty;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_fminmax_opcode(opcode: Opcode, ty: Type) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM0, RegType::Float),
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM1, RegType::Float),
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM2, RegType::Float),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(opcode);
        instruction.v = Value(0).with_type(ty);
        instruction.v2 = Value(1).with_type(ty);
        instruction.r_value = Value(2).with_type(ty);
        instruction.typ = ty;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_fcmp_opcode(ty: Type, cond: FloatCmpCond) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM0, RegType::Float),
            VReg::from_real_reg(crate::backend::isa::amd64::reg::XMM1, RegType::Float),
            VReg::from_real_reg(4, RegType::Int),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::Fcmp);
        instruction.v = Value(0).with_type(ty);
        instruction.v2 = Value(1).with_type(ty);
        instruction.r_value = Value(2).with_type(Type::I32);
        instruction.typ = Type::I32;
        instruction.u1 = cond as u64;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_icmp_opcode(ty: Type, cond: IntegerCmpCond) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(1, RegType::Int),
            VReg::from_real_reg(3, RegType::Int),
            VReg::from_real_reg(4, RegType::Int),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::Icmp);
        instruction.v = Value(0).with_type(ty);
        instruction.v2 = Value(1).with_type(ty);
        instruction.r_value = Value(2).with_type(Type::I32);
        instruction.typ = Type::I32;
        instruction.u1 = cond as u64;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_exit_if_true_with_code_generic(exit_code: ExitCode) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(1, RegType::Int),
            VReg::from_real_reg(4, RegType::Int),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::ExitIfTrueWithCode);
        instruction.v = Value(0).with_type(Type::I64);
        instruction.v2 = Value(1).with_type(Type::I32);
        instruction.u1 = exit_code.raw() as u64;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_exit_if_true_with_code_icmp(exit_code: ExitCode) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        let bb = compiler.builder.allocate_basic_block();
        compiler.builder.set_current_block(bb);
        let x = compiler.builder.allocate_value(Type::I32);
        let y = compiler.builder.allocate_value(Type::I32);
        let icmp_id = compiler
            .builder
            .insert_instruction(compiler.builder.allocate_instruction().as_icmp(
                x,
                y,
                IntegerCmpCond::Equal,
            ));
        let cond = compiler.builder.instruction(icmp_id).return_();
        let exec_ctx = compiler.builder.allocate_value(Type::I64);
        compiler.regs = vec![
            VReg::from_real_reg(4, RegType::Int),
            VReg::from_real_reg(2, RegType::Int),
            VReg::from_real_reg(3, RegType::Int),
            VReg::from_real_reg(1, RegType::Int),
        ];
        compiler.defs.resize(cond.id().0 as usize + 1, SSAValueDefinition::default());
        compiler.defs[cond.id().0 as usize] = SSAValueDefinition {
            value: cond,
            instr: Some(icmp_id),
            ref_count: 1,
        };
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(Opcode::ExitIfTrueWithCode);
        instruction.v = exec_ctx;
        instruction.v2 = cond;
        instruction.u1 = exit_code.raw() as u64;

        m.lower_instr(&instruction);
        m.format()
    }

    fn lower_partial_load_opcode(opcode: Opcode, typ: Type) -> String {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));

        let mut compiler = Box::new(TestCompilerContext::default());
        compiler.regs = vec![
            VReg::from_real_reg(1, RegType::Int),
            VReg::from_real_reg(4, RegType::Int),
        ];
        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        m.set_compiler(ptr);

        let mut instruction = crate::ssa::Instruction::new().with_opcode(opcode);
        instruction.v = Value(0).with_type(Type::I64);
        instruction.r_value = Value(1).with_type(typ);
        instruction.typ = typ;

        m.lower_instr(&instruction);
        m.format()
    }

    #[test]
    fn lowers_ishl_to_amd64_shift_sequence() {
        let formatted = lower_shift_opcode(Opcode::Ishl);
        assert!(formatted.contains("movq %rax, %r11"));
        assert!(formatted.contains("movl %ebx, %ecx"));
        assert!(formatted.contains("shlq %cl, %r11"));
        assert!(formatted.contains("movq %r11, %rdi"));
    }

    #[test]
    fn lowers_sshr_to_amd64_shift_sequence() {
        let formatted = lower_shift_opcode(Opcode::Sshr);
        assert!(formatted.contains("sarq %cl, %r11"));
    }

    #[test]
    fn lowers_rotr_to_amd64_rotate_sequence() {
        let formatted = lower_shift_opcode(Opcode::Rotr);
        assert!(formatted.contains("rorq %cl, %r11"));
    }

    #[test]
    fn lowers_i32_select_to_amd64_cmov_sequence() {
        let formatted = lower_select_opcode(Type::I32);
        assert!(formatted.contains("testl %eax, %eax"));
        assert!(formatted.contains("movl %ecx, %esi"));
        assert!(formatted.contains("cmovnzl %ebx, %esi"));
    }

    #[test]
    fn lowers_i64_select_to_amd64_cmov_sequence() {
        let formatted = lower_select_opcode(Type::I64);
        assert!(formatted.contains("testl %eax, %eax"));
        assert!(formatted.contains("movq %rcx, %rsi"));
        assert!(formatted.contains("cmovnzq %rbx, %rsi"));
    }

    #[test]
    fn lowers_i32_icmp_with_setcc_and_zero_extend() {
        let formatted = lower_icmp_opcode(Type::I32, IntegerCmpCond::Equal);
        assert!(formatted.contains("cmpl "));
        assert!(formatted.contains("setz"));
        assert!(formatted.contains("movzx.bq"));
    }

    #[test]
    fn lowers_i64_icmp_with_64bit_compare() {
        let formatted = lower_icmp_opcode(Type::I64, IntegerCmpCond::UnsignedLessThan);
        assert!(formatted.contains("cmpq "));
        assert!(formatted.contains("setb"));
        assert!(formatted.contains("movzx.bq"));
    }

    #[test]
    fn lowers_f32_add_with_sse_sequence() {
        let formatted = lower_float_binary_opcode(Opcode::Fadd, Type::F32);
        assert!(formatted.contains("movss %xmm0, %xmm2"));
        assert!(formatted.contains("addss %xmm1, %xmm2"));
    }

    #[test]
    fn lowers_f64_sub_with_sse_sequence() {
        let formatted = lower_float_binary_opcode(Opcode::Fsub, Type::F64);
        assert!(formatted.contains("movsd %xmm0, %xmm2"));
        assert!(formatted.contains("subsd %xmm1, %xmm2"));
    }

    #[test]
    fn lowers_f64_div_with_sse_sequence() {
        let formatted = lower_float_binary_opcode(Opcode::Fdiv, Type::F64);
        assert!(formatted.contains("movsd %xmm0, %xmm2"));
        assert!(formatted.contains("divsd %xmm1, %xmm2"));
    }

    #[test]
    fn lowers_f32_sqrt_with_sse_sequence() {
        let formatted = lower_sqrt_opcode(Type::F32);
        assert!(formatted.contains("sqrtss %xmm0, %xmm2"));
    }

    #[test]
    fn lowers_f64_sqrt_with_sse_sequence() {
        let formatted = lower_sqrt_opcode(Type::F64);
        assert!(formatted.contains("sqrtsd %xmm0, %xmm2"));
    }

    #[test]
    fn lowers_f32_promote_to_f64_with_sse_sequence() {
        let formatted = lower_float_convert_opcode(Opcode::Fpromote, Type::F32, Type::F64);
        assert!(formatted.contains("cvtss2sd %xmm0, %xmm2"));
    }

    #[test]
    fn lowers_f64_demote_to_f32_with_sse_sequence() {
        let formatted = lower_float_convert_opcode(Opcode::Fdemote, Type::F64, Type::F32);
        assert!(formatted.contains("cvtsd2ss %xmm0, %xmm2"));
    }

    #[test]
    fn lowers_i32_to_f32_with_cvtsi2ss_sequence() {
        let formatted = lower_fcvt_from_sint_opcode(Type::I32, Type::F32);
        assert!(formatted.contains("cvtsi2ss %eax, %xmm2"));
    }

    #[test]
    fn lowers_i32_to_f64_with_cvtsi2sd_sequence() {
        let formatted = lower_fcvt_from_sint_opcode(Type::I32, Type::F64);
        assert!(formatted.contains("cvtsi2sd %eax, %xmm2"));
    }

    #[test]
    fn lowers_i64_to_f32_with_cvtsi2ss_sequence() {
        let formatted = lower_fcvt_from_sint_opcode(Type::I64, Type::F32);
        assert!(formatted.contains("cvtsi2ss %rax, %xmm2"));
    }

    #[test]
    fn lowers_i64_to_f64_with_cvtsi2sd_sequence() {
        let formatted = lower_fcvt_from_sint_opcode(Type::I64, Type::F64);
        assert!(formatted.contains("cvtsi2sd %rax, %xmm2"));
    }

    #[test]
    fn lowers_i32_to_f32_bitcast_with_movd_sequence() {
        let formatted = lower_bitcast_opcode(Type::I32, Type::F32);
        assert!(formatted.contains("movd %eax, %xmm2"));
    }

    #[test]
    fn lowers_f32_to_i32_bitcast_with_movd_sequence() {
        let formatted = lower_bitcast_opcode(Type::F32, Type::I32);
        assert!(formatted.contains("movd %xmm0, %eax"));
    }

    #[test]
    fn lowers_i64_to_i32_with_movzx_lq_sequence() {
        let formatted = lower_ireduce_opcode(Type::I64, Type::I32);
        assert!(formatted.contains("movzx.lq %rax, %rdx"));
    }

    #[test]
    fn lowers_i64_to_f64_bitcast_with_movq_sequence() {
        let formatted = lower_bitcast_opcode(Type::I64, Type::F64);
        assert!(formatted.contains("movq %rax, %xmm2"));
    }

    #[test]
    fn lowers_f64_to_i64_bitcast_with_movq_sequence() {
        let formatted = lower_bitcast_opcode(Type::F64, Type::I64);
        assert!(formatted.contains("movq %xmm0, %rax"));
    }

    #[test]
    fn lowers_u32_to_f32_with_zero_extend_then_cvtsi2ss_sequence() {
        let formatted = lower_fcvt_from_uint_opcode(Type::I32, Type::F32);
        assert!(formatted.contains("movzx.lq %rax, %r130?"));
        assert!(formatted.contains("cvtsi2ss %r130?, %xmm2"));
    }

    #[test]
    fn lowers_u32_to_f64_with_zero_extend_then_cvtsi2sd_sequence() {
        let formatted = lower_fcvt_from_uint_opcode(Type::I32, Type::F64);
        assert!(formatted.contains("movzx.lq %rax, %r130?"));
        assert!(formatted.contains("cvtsi2sd %r130?, %xmm2"));
    }

    #[test]
    fn lowers_u64_to_f32_with_msb_adjust_sequence() {
        let formatted = lower_fcvt_from_uint_opcode(Type::I64, Type::F32);
        assert!(formatted.contains("testq %rax, %rax"));
        assert!(formatted.contains("cvtsi2ss %rax, %xmm2"));
        assert!(formatted.contains("cvtsi2ss %r130?, %xmm2"));
        assert!(formatted.contains("addss %xmm2, %xmm2"));
    }

    #[test]
    fn lowers_u64_to_f64_with_msb_adjust_sequence() {
        let formatted = lower_fcvt_from_uint_opcode(Type::I64, Type::F64);
        assert!(formatted.contains("testq %rax, %rax"));
        assert!(formatted.contains("cvtsi2sd %rax, %xmm2"));
        assert!(formatted.contains("cvtsi2sd %r130?, %xmm2"));
        assert!(formatted.contains("addsd %xmm2, %xmm2"));
    }

    #[test]
    fn lowers_f32_ceil_with_roundss_up_mode() {
        let formatted = lower_round_opcode(Opcode::Ceil, Type::F32);
        assert!(formatted.contains("roundss $2, %xmm0, %xmm2"));
    }

    #[test]
    fn lowers_f64_floor_with_roundsd_down_mode() {
        let formatted = lower_round_opcode(Opcode::Floor, Type::F64);
        assert!(formatted.contains("roundsd $1, %xmm0, %xmm2"));
    }

    #[test]
    fn lowers_f32_trunc_with_roundss_zero_mode() {
        let formatted = lower_round_opcode(Opcode::Trunc, Type::F32);
        assert!(formatted.contains("roundss $3, %xmm0, %xmm2"));
    }

    #[test]
    fn lowers_f64_nearest_with_roundsd_nearest_mode() {
        let formatted = lower_round_opcode(Opcode::Nearest, Type::F64);
        assert!(formatted.contains("roundsd $0, %xmm0, %xmm2"));
    }

    #[test]
    fn lowers_f32_fmin_with_nan_and_zero_sensitive_sequence() {
        let formatted = lower_fminmax_opcode(Opcode::Fmin, Type::F32);
        assert!(formatted.contains("movdqu %xmm0, %xmm2"));
        assert!(formatted.contains("ucomiss %xmm1, %xmm2"));
        assert!(formatted.contains("jnz L"));
        assert!(formatted.contains("jp L"));
        assert!(formatted.contains("orps %xmm1, %xmm2"));
        assert!(formatted.contains("addss %xmm1, %xmm2"));
        assert!(formatted.contains("minps %xmm1, %xmm2"));
    }

    #[test]
    fn lowers_f64_fmax_with_nan_and_zero_sensitive_sequence() {
        let formatted = lower_fminmax_opcode(Opcode::Fmax, Type::F64);
        assert!(formatted.contains("movdqu %xmm0, %xmm2"));
        assert!(formatted.contains("ucomisd %xmm1, %xmm2"));
        assert!(formatted.contains("jnz L"));
        assert!(formatted.contains("jp L"));
        assert!(formatted.contains("andpd %xmm1, %xmm2"));
        assert!(formatted.contains("addsd %xmm1, %xmm2"));
        assert!(formatted.contains("maxpd %xmm1, %xmm2"));
    }

    #[test]
    fn lowers_f32_fcmp_eq_with_nan_safe_flag_sequence() {
        let formatted = lower_fcmp_opcode(Type::F32, FloatCmpCond::Equal);
        assert!(formatted.contains("ucomiss %xmm1, %xmm0"));
        assert!(formatted.contains("setnp"));
        assert!(formatted.contains("setz"));
        assert!(formatted.contains("and "));
        assert!(formatted.contains("movzx.bq"));
    }

    #[test]
    fn lowers_f64_fcmp_ne_with_nan_safe_flag_sequence() {
        let formatted = lower_fcmp_opcode(Type::F64, FloatCmpCond::NotEqual);
        assert!(formatted.contains("ucomisd %xmm1, %xmm0"));
        assert!(formatted.contains("setp"));
        assert!(formatted.contains("setnz"));
        assert!(formatted.contains("or "));
        assert!(formatted.contains("movzx.bq"));
    }

    #[test]
    fn lowers_f64_fcmp_gt_with_single_condition() {
        let formatted = lower_fcmp_opcode(Type::F64, FloatCmpCond::GreaterThan);
        assert!(formatted.contains("ucomisd %xmm1, %xmm0"));
        assert!(formatted.contains("setnbe"));
        assert!(formatted.contains("movzx.bq"));
    }

    #[test]
    fn lowers_uextend_i32_to_i64_to_amd64_movzx_lq() {
        let formatted = lower_uextend_opcode();
        assert!(formatted.contains("movzx.lq %rax, %rsi"));
    }

    #[test]
    fn lowers_sextend_i32_to_i64_to_amd64_movsx_lq() {
        let formatted = lower_sextend_opcode();
        assert!(formatted.contains("movsx.lq %rax, %rsi"));
    }

    #[test]
    fn lowers_i64_udiv_with_zero_guard_and_rax_result() {
        let formatted = lower_divrem_opcode(Opcode::Udiv, Type::I64);
        assert!(formatted.contains("testq "));
        assert!(formatted.contains("movl $10, %r11d"));
        assert!(formatted.contains("movq "));
        assert!(formatted.contains("movl $0, %edx"));
        assert!(formatted.contains("divq "));
        assert!(formatted.contains("movq %rax"));
    }

    #[test]
    fn lowers_i64_sdiv_with_overflow_guard() {
        let formatted = lower_divrem_opcode(Opcode::Sdiv, Type::I64);
        assert!(formatted.contains("testq "));
        assert!(formatted.contains("movl $11, %r11d"));
        assert!(formatted.contains("cqo"));
        assert!(formatted.contains("idivq "));
        assert!(formatted.contains("movq %rax"));
    }

    #[test]
    fn lowers_i32_srem_with_neg1_fast_path() {
        let formatted = lower_divrem_opcode(Opcode::Srem, Type::I32);
        assert!(formatted.contains("testl "));
        assert!(formatted.contains("movl $0, %edx"));
        assert!(formatted.contains("idivl "));
        assert!(formatted.contains("movl %edx"));
    }

    #[test]
    fn lowers_exit_with_code_to_exec_ctx_store_and_ret() {
        let formatted = lower_exit_with_code(ExitCode::UNREACHABLE);
        assert!(formatted.contains("movl $3, %r11d"));
        assert!(formatted.contains("mov.l %r11, (%rax)"));
        assert!(formatted.contains("movq %rbp, %rsp"));
        assert!(formatted.contains("popq %rbp"));
        assert!(formatted.contains("ret"));
    }

    #[test]
    fn lowers_exit_if_true_with_code_from_generic_cond() {
        let formatted = lower_exit_if_true_with_code_generic(ExitCode::UNREACHABLE);
        assert!(formatted.contains("testl %ebx, %ebx"));
        assert!(formatted.contains("jz L1"));
        assert!(formatted.contains("movl $3, %r11d"));
        assert!(formatted.contains("blk1:"));
    }

    #[test]
    fn lowers_exit_if_true_with_code_from_icmp() {
        let formatted = lower_exit_if_true_with_code_icmp(ExitCode::INTEGER_DIVISION_BY_ZERO);
        assert!(formatted.contains("cmpl %ecx, %ebx"));
        assert!(formatted.contains("jnz L1"));
        assert!(formatted.contains("movl $10, %r11d"));
        assert!(formatted.contains("blk1:"));
    }

    #[test]
    fn lowers_i32_uload8_to_amd64_movzx_bl() {
        let formatted = lower_partial_load_opcode(Opcode::Uload8, Type::I32);
        assert!(formatted.contains("movzx.bl"));
    }

    #[test]
    fn lowers_i64_sload8_to_amd64_movsx_bq() {
        let formatted = lower_partial_load_opcode(Opcode::Sload8, Type::I64);
        assert!(formatted.contains("movsx.bq"));
    }

    #[test]
    fn lowers_i64_uload16_to_amd64_movzx_wq() {
        let formatted = lower_partial_load_opcode(Opcode::Uload16, Type::I64);
        assert!(formatted.contains("movzx.wq"));
    }

    #[test]
    fn lowers_i32_sload16_to_amd64_movsx_wl() {
        let formatted = lower_partial_load_opcode(Opcode::Sload16, Type::I32);
        assert!(formatted.contains("movsx.wl"));
    }

    #[test]
    fn lowers_i64_uload32_to_amd64_movzx_lq() {
        let formatted = lower_partial_load_opcode(Opcode::Uload32, Type::I64);
        assert!(formatted.contains("movzx.lq"));
    }

    #[test]
    fn lowers_i64_sload32_to_amd64_movsx_lq() {
        let formatted = lower_partial_load_opcode(Opcode::Sload32, Type::I64);
        assert!(formatted.contains("movsx.lq"));
    }
}
