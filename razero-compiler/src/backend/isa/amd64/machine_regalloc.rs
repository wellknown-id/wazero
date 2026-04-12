use crate::backend::regalloc::{
    Allocator, Block, Function, Instr, RealReg, RegType, RegisterInfo, VReg,
};

use super::abi::amd64_register_info;
use super::instr::Amd64Instr;
use super::machine::{Amd64Block, Amd64Machine};
use super::operands::Operand;
use super::SseOpcode;

pub fn regalloc_register_info() -> RegisterInfo {
    amd64_register_info()
}

pub fn do_regalloc(machine: &mut Amd64Machine) {
    let mut alloc = Allocator::new(regalloc_register_info());
    alloc.do_allocation(machine);
}

fn to_reg_type(ty: crate::backend::RegType) -> RegType {
    match ty {
        crate::backend::RegType::Invalid => RegType::Invalid,
        crate::backend::RegType::Int => RegType::Int,
        crate::backend::RegType::Float => RegType::Float,
    }
}

fn from_reg_type(ty: RegType) -> crate::backend::RegType {
    match ty {
        RegType::Invalid => crate::backend::RegType::Invalid,
        RegType::Int => crate::backend::RegType::Int,
        RegType::Float => crate::backend::RegType::Float,
    }
}

fn to_regalloc_vreg(v: crate::backend::VReg) -> VReg {
    VReg::new(v.id())
        .set_real_reg(RealReg(v.real_reg()))
        .set_reg_type(to_reg_type(v.reg_type()))
}

fn from_regalloc_vreg(v: VReg) -> crate::backend::VReg {
    crate::backend::VReg(v.id() as u64)
        .set_real_reg(v.real_reg().0)
        .set_reg_type(from_reg_type(v.reg_type()))
}

impl Instr for Amd64Instr {
    fn defs(&self, out: &mut Vec<VReg>) {
        let mut defs = Vec::new();
        Amd64Instr::defs(self, &mut defs);
        out.clear();
        out.extend(defs.into_iter().map(to_regalloc_vreg));
    }

    fn uses(&self, out: &mut Vec<VReg>) {
        let mut uses = Vec::new();
        Amd64Instr::uses(self, &mut uses);
        out.clear();
        out.extend(uses.into_iter().map(to_regalloc_vreg));
    }

    fn assign_use(&self, index: usize, v: VReg) {
        Amd64Instr::assign_use(self, index, from_regalloc_vreg(v));
    }

    fn assign_def(&self, v: VReg) {
        Amd64Instr::assign_def(self, from_regalloc_vreg(v));
    }

    fn is_copy(&self) -> bool {
        let mut uses = Vec::new();
        Amd64Instr::uses(self, &mut uses);
        Amd64Instr::is_copy(self) && uses.iter().any(|use_| !use_.is_real_reg())
    }

    fn is_call(&self) -> bool {
        Amd64Instr::is_call(self)
    }

    fn is_indirect_call(&self) -> bool {
        Amd64Instr::is_indirect_call(self)
    }

    fn is_return(&self) -> bool {
        Amd64Instr::is_return(self)
    }
}

impl Block<Amd64Instr> for Amd64Block {
    fn id(&self) -> i32 {
        self.id
    }

    fn instructions(&self, out: &mut Vec<Amd64Instr>) {
        out.clear();
        out.extend(self.instructions.iter().cloned());
    }

    fn instructions_rev(&self, out: &mut Vec<Amd64Instr>) {
        out.clear();
        out.extend(self.instructions.iter().rev().cloned());
    }

    fn first_instr(&self) -> Option<Amd64Instr> {
        self.instructions.first().cloned()
    }

    fn last_instr_for_insertion(&self) -> Option<Amd64Instr> {
        self.instructions.last().cloned()
    }

    fn preds(&self) -> usize {
        self.preds.len()
    }

    fn entry(&self) -> bool {
        self.entry
    }

    fn succs(&self) -> usize {
        self.succs.len()
    }

    fn loop_header(&self) -> bool {
        self.loop_header
    }

    fn loop_nesting_forest_children(&self) -> usize {
        self.children.len()
    }
}

impl Amd64Machine {
    fn block_by_id(&self, id: i32) -> Amd64Block {
        self.blocks
            .iter()
            .find(|block| block.id == id)
            .cloned()
            .expect("block must exist")
    }

    fn ordered_blocks(&self) -> Vec<Amd64Block> {
        if self.block_order.is_empty() {
            self.blocks.clone()
        } else {
            self.block_order
                .iter()
                .map(|&id| self.block_by_id(id))
                .collect()
        }
    }

    fn insertion_point(&self, anchor: Option<&Amd64Instr>) -> (usize, usize) {
        if let Some(anchor) = anchor {
            for (block_index, block) in self.blocks.iter().enumerate() {
                if let Some(instr_index) =
                    block.instructions.iter().position(|instr| instr == anchor)
                {
                    return (block_index, instr_index);
                }
            }
            panic!("anchor instruction must exist");
        }

        let block_index = self.current_block.unwrap_or(0);
        let instr_index = self
            .blocks
            .get(block_index)
            .map(|b| b.instructions.len())
            .unwrap_or(0);
        (block_index, instr_index)
    }

    fn insert_before_anchor(&mut self, anchor: Option<Amd64Instr>, mut insts: Vec<Amd64Instr>) {
        let (block_index, instr_index) = self.insertion_point(anchor.as_ref());
        self.blocks[block_index]
            .instructions
            .splice(instr_index..instr_index, insts.drain(..));
    }

    fn insert_after_anchor(&mut self, anchor: Option<Amd64Instr>, mut insts: Vec<Amd64Instr>) {
        let (block_index, instr_index) = self.insertion_point(anchor.as_ref());
        let insert_at = if anchor.is_some() {
            instr_index + 1
        } else {
            instr_index
        };
        self.blocks[block_index]
            .instructions
            .splice(insert_at..insert_at, insts.drain(..));
    }

    fn insert_move_instr(dst: crate::backend::VReg, src: crate::backend::VReg) -> Amd64Instr {
        if matches!(dst.reg_type(), crate::backend::RegType::Float) {
            Amd64Instr::xmm_unary_rm_r(SseOpcode::Movdqu, Operand::reg(src), dst)
        } else {
            Amd64Instr::mov_rr(src, dst, true)
        }
    }

    fn idom_block(&self, blk: Amd64Block) -> Amd64Block {
        if blk.entry || blk.preds.is_empty() {
            return blk;
        }
        self.block_by_id(blk.preds[0])
    }
}

impl Function<Amd64Instr, Amd64Block> for Amd64Machine {
    fn post_order_blocks(&self, out: &mut Vec<Amd64Block>) {
        out.clear();
        out.extend(self.ordered_blocks().into_iter().rev());
    }

    fn reverse_post_order_blocks(&self, out: &mut Vec<Amd64Block>) {
        out.clear();
        out.extend(self.ordered_blocks());
    }

    fn clobbered_registers(&mut self, regs: &[VReg]) {
        self.clobbered.clear();
        self.clobbered
            .extend(regs.iter().copied().map(from_regalloc_vreg));
    }

    fn loop_nesting_forest_roots(&self, out: &mut Vec<Amd64Block>) {
        out.clear();
        let mut roots: Vec<_> = self
            .ordered_blocks()
            .into_iter()
            .filter(|block| block.entry || block.preds.is_empty())
            .collect();
        if roots.is_empty() && !self.blocks.is_empty() {
            roots.push(self.blocks[0].clone());
        }
        out.extend(roots);
    }

    fn loop_nesting_forest_child(&self, block: Amd64Block, index: usize) -> Amd64Block {
        self.block_by_id(block.children[index])
    }

    fn lowest_common_ancestor(&self, blk1: Amd64Block, blk2: Amd64Block) -> Amd64Block {
        if blk1 == blk2 {
            return blk1;
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut cur = blk1.clone();
        loop {
            seen.insert(cur.id);
            let next = self.idom_block(cur.clone());
            if next == cur {
                break;
            }
            cur = next;
        }

        let mut cur = blk2;
        loop {
            if seen.contains(&cur.id) {
                return cur;
            }
            let next = self.idom_block(cur.clone());
            if next == cur {
                return next;
            }
            cur = next;
        }
    }

    fn idom(&self, blk: Amd64Block) -> Amd64Block {
        self.idom_block(blk)
    }

    fn pred(&self, block: Amd64Block, index: usize) -> Amd64Block {
        self.block_by_id(block.preds[index])
    }

    fn succ(&self, block: Amd64Block, index: usize) -> Amd64Block {
        self.block_by_id(block.succs[index])
    }

    fn block_params(&self, block: Amd64Block, out: &mut Vec<VReg>) {
        out.clear();
        out.extend(block.params.into_iter().map(to_regalloc_vreg));
    }

    fn swap_before(&mut self, x1: VReg, x2: VReg, tmp: VReg, instr: Option<Amd64Instr>) {
        let x1 = from_regalloc_vreg(x1);
        let x2 = from_regalloc_vreg(x2);
        let tmp = from_regalloc_vreg(tmp);
        let insts = if tmp.valid() {
            vec![
                Self::insert_move_instr(tmp, x1),
                Self::insert_move_instr(x1, x2),
                Self::insert_move_instr(x2, tmp),
            ]
        } else {
            vec![Amd64Instr::xchg(Operand::reg(x1), Operand::reg(x2), 8)]
        };
        self.insert_before_anchor(instr, insts);
    }

    fn store_register_before(&mut self, v: VReg, instr: Option<Amd64Instr>) {
        let inst = self.store_to_spill_slot(from_regalloc_vreg(v));
        self.insert_before_anchor(instr, vec![inst]);
    }

    fn store_register_after(&mut self, v: VReg, instr: Option<Amd64Instr>) {
        let inst = self.store_to_spill_slot(from_regalloc_vreg(v));
        self.insert_after_anchor(instr, vec![inst]);
    }

    fn reload_register_before(&mut self, v: VReg, instr: Option<Amd64Instr>) {
        let inst = self.load_from_spill_slot(from_regalloc_vreg(v));
        self.insert_before_anchor(instr, vec![inst]);
    }

    fn reload_register_after(&mut self, v: VReg, instr: Option<Amd64Instr>) {
        let inst = self.load_from_spill_slot(from_regalloc_vreg(v));
        self.insert_after_anchor(instr, vec![inst]);
    }

    fn insert_move_before(&mut self, dst: VReg, src: VReg, instr: Option<Amd64Instr>) {
        self.insert_before_anchor(
            instr,
            vec![Self::insert_move_instr(
                from_regalloc_vreg(dst),
                from_regalloc_vreg(src),
            )],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{do_regalloc, regalloc_register_info};
    use crate::backend::compiler::Compiler;
    use crate::backend::isa::amd64::operands::{AddressMode, Operand};
    use crate::backend::isa::amd64::{Amd64Instr, Amd64Machine, AluRmiROpcode, R15, RAX};
    use crate::backend::machine::Machine;
    use crate::backend::{RegType, VReg};
    use crate::ssa::{BasicBlockId, Builder, Signature, SignatureId, Type, Values};

    fn build_add_compiler() -> Box<Compiler<Amd64Machine>> {
        let mut builder = Builder::new();
        builder.init(Signature::new(
            SignatureId(0),
            vec![Type::I64, Type::I64],
            vec![Type::I64],
        ));
        let entry = builder.allocate_basic_block();
        builder.set_current_block(entry);
        builder.reverse_post_ordered_blocks.push(entry);

        let lhs = builder.allocate_value(Type::I64);
        let rhs = builder.allocate_value(Type::I64);
        builder.block_mut(entry).add_param(lhs);
        builder.block_mut(entry).add_param(rhs);
        builder
            .values_info
            .resize(rhs.id().0 as usize + 1, Default::default());
        builder.values_info[lhs.id().0 as usize].ref_count = 1;
        builder.values_info[rhs.id().0 as usize].ref_count = 1;

        let add = builder.insert_instruction(builder.allocate_instruction().as_iadd(lhs, rhs));
        builder.insert_instruction(
            builder
                .allocate_instruction()
                .as_return(Values::from_vec(vec![builder.instruction(add).return_()])),
        );

        Compiler::new(Amd64Machine::new(), builder)
    }

    #[test]
    fn register_info_smoke_test() {
        let info = regalloc_register_info();
        assert_eq!(
            (info.real_reg_name)(crate::backend::regalloc::RealReg(1)),
            "rax"
        );
    }

    #[test]
    fn compiler_lowering_populates_virtual_param_moves_and_arithmetic() {
        let mut compiler = build_add_compiler();
        compiler.lower();
        let text = compiler.machine().format();
        assert!(text.contains("movq %rax"));
        assert!(text.contains("add "));
        assert!(text.contains('?'));
    }

    #[test]
    fn compiler_lowering_emits_float_abi_moves() {
        let mut builder = Builder::new();
        builder.init(Signature::new(
            SignatureId(1),
            vec![Type::F64],
            vec![Type::F64],
        ));
        let entry = builder.allocate_basic_block();
        builder.set_current_block(entry);
        builder.reverse_post_ordered_blocks.push(entry);

        let value = builder.allocate_value(Type::F64);
        builder.block_mut(entry).add_param(value);
        builder
            .values_info
            .resize(value.id().0 as usize + 1, Default::default());
        builder.values_info[value.id().0 as usize].ref_count = 1;
        builder.insert_instruction(
            builder
                .allocate_instruction()
                .as_return(Values::from_vec(vec![value])),
        );

        let mut compiler = Compiler::new(Amd64Machine::new(), builder);
        compiler.lower();
        let text = compiler.machine().format();
        // Entry-block float params are pre-coloured to physical XMM regs, so
        // lower_params emits a physical-to-physical self-move (a regalloc nop).
        assert!(text.contains("movsd %xmm0, %xmm0"));
        assert!(text.contains("%xmm0"));
    }

    #[test]
    fn machine_regalloc_entrypoint_runs_allocator() {
        let mut machine = Amd64Machine::new();
        Machine::start_lowering_function(&mut machine, BasicBlockId(0));
        Machine::start_block(&mut machine, BasicBlockId(0));
        machine.push(Amd64Instr::mov_rr(
            VReg::from_real_reg(RAX, RegType::Int),
            VReg(128).set_reg_type(RegType::Int),
            true,
        ));
        do_regalloc(&mut machine);
        assert!(!machine.format().contains('?'));
    }

    #[test]
    fn machine_regalloc_keeps_load_def_distinct_from_memory_base() {
        let exec_ctx = VReg::from_real_reg(R15, RegType::Int);
        let loaded = VReg(128).set_reg_type(RegType::Int);
        let decremented = VReg(129).set_reg_type(RegType::Int);
        let mem = Operand::mem(AddressMode::imm_reg(0x4a0, exec_ctx));
        let mut machine = Amd64Machine::new();
        Machine::start_lowering_function(&mut machine, BasicBlockId(0));
        Machine::start_block(&mut machine, BasicBlockId(0));
        machine.push(Amd64Instr::mov64_mr(mem.clone(), loaded));
        machine.push(Amd64Instr::mov_rr(loaded, decremented, true));
        machine.push(Amd64Instr::alu_rmi_r(
            AluRmiROpcode::Sub,
            Operand::imm32(1),
            decremented,
            true,
        ));
        machine.push(Amd64Instr::mov_rm(decremented, mem, 8));

        do_regalloc(&mut machine);

        let load = &machine.blocks[0].instructions[0];
        let mut uses = Vec::new();
        Amd64Instr::uses(load, &mut uses);
        let mut defs = Vec::new();
        Amd64Instr::defs(load, &mut defs);
        assert_eq!(uses.len(), 1);
        assert_eq!(defs.len(), 1);
        assert!(uses[0].is_real_reg());
        assert!(defs[0].is_real_reg());
        assert_ne!(uses[0].real_reg(), defs[0].real_reg());
    }
}
