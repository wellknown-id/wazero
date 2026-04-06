use std::collections::BTreeMap;
use std::ptr::NonNull;

use crate::backend::machine::{BackendError, Machine as BackendMachine};
use crate::backend::{AbiArgKind, CompilerContext, FunctionAbi, RealReg, RelocationInfo, VReg};
use crate::ssa::{
    BasicBlock, BasicBlockId, Instruction, Opcode, Signature, SourceOffset, Type, Value,
};
use crate::wazevoapi::ExitCode;

use super::abi::{lower_abi_params, FLOAT_ARG_RESULT_REGS, INT_ARG_RESULT_REGS};
use super::abi_entry_preamble::compile_entry_preamble;
use super::abi_host_call::compile_host_function_trampoline;
use super::cond::Cond;
use super::instr::Amd64Instr;
use super::lower_constant::lower_constant;
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
            Opcode::Iadd | Opcode::Isub | Opcode::Imul => {
                let dst = self.compiler().v_reg_of(instruction.return_());
                let lhs = self.compiler().v_reg_of(instruction.v);
                let rhs = self.compiler().v_reg_of(instruction.v2);
                let is_64 = instruction.typ.bits() == 64;
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::mov_rr(lhs, dst, is_64));
                let op = match instruction.opcode {
                    Opcode::Iadd => super::AluRmiROpcode::Add,
                    Opcode::Isub => super::AluRmiROpcode::Sub,
                    Opcode::Imul => super::AluRmiROpcode::Mul,
                    _ => unreachable!(),
                };
                self.current_block_mut()
                    .instructions
                    .push(Amd64Instr::alu_rmi_r(op, Operand::reg(rhs), dst, is_64));
            }
            _ => {}
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
        if self.block_order.len() != 1 {
            return;
        }
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
        let regs: Vec<_> = params
            .iter()
            .copied()
            .filter(|value| value.valid())
            .map(|value| self.compiler().v_reg_of(value))
            .collect();
        let abi = self.current_abi.clone();
        let lowered = lower_abi_params(&abi, &regs);
        self.current_block_mut().params = regs.clone();
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
    use crate::backend::machine::Machine;
    use crate::backend::{
        CompilerContext, FunctionAbi, RegType, SSAValueDefinition, SourceOffsetInfo, VReg,
    };
    use crate::ssa::{
        BasicBlockId, Builder, FuncRef, Opcode, Signature, SourceOffset, Type, Value,
    };

    struct TestCompilerContext {
        builder: Builder,
        regs: Vec<VReg>,
        abi: FunctionAbi,
        buf: Vec<u8>,
        source_offsets: Vec<SourceOffsetInfo>,
    }

    impl Default for TestCompilerContext {
        fn default() -> Self {
            Self {
                builder: Builder::new(),
                regs: Vec::new(),
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

        fn allocate_vreg(&mut self, _ty: Type) -> VReg {
            VReg::INVALID
        }

        fn value_definition(&self, _value: Value) -> SSAValueDefinition {
            SSAValueDefinition::default()
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

        fn match_instr(&self, _def: SSAValueDefinition, _opcode: Opcode) -> bool {
            false
        }

        fn match_instr_one_of(&self, _def: SSAValueDefinition, _opcodes: &[Opcode]) -> Opcode {
            Opcode::Undefined
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
}
