//! Backend-facing register allocation interfaces.

use std::fmt;

use super::reg::VReg;

pub trait Function<I: Instr, B: Block<I>> {
    fn post_order_blocks(&self, out: &mut Vec<B>);
    fn reverse_post_order_blocks(&self, out: &mut Vec<B>);
    fn clobbered_registers(&mut self, regs: &[VReg]);
    fn loop_nesting_forest_roots(&self, out: &mut Vec<B>);
    fn loop_nesting_forest_child(&self, block: B, index: usize) -> B;
    fn lowest_common_ancestor(&self, blk1: B, blk2: B) -> B;
    fn idom(&self, blk: B) -> B;
    fn pred(&self, block: B, index: usize) -> B;
    fn succ(&self, block: B, index: usize) -> B;
    fn block_params(&self, block: B, out: &mut Vec<VReg>);

    fn swap_before(&mut self, x1: VReg, x2: VReg, tmp: VReg, instr: Option<I>);
    fn store_register_before(&mut self, v: VReg, instr: Option<I>);
    fn store_register_after(&mut self, v: VReg, instr: Option<I>);
    fn reload_register_before(&mut self, v: VReg, instr: Option<I>);
    fn reload_register_after(&mut self, v: VReg, instr: Option<I>);
    fn insert_move_before(&mut self, dst: VReg, src: VReg, instr: Option<I>);
}

pub trait Block<I: Instr>: Clone + Eq {
    fn id(&self) -> i32;
    fn instructions(&self, out: &mut Vec<I>);
    fn instructions_rev(&self, out: &mut Vec<I>);
    fn first_instr(&self) -> Option<I>;
    fn last_instr_for_insertion(&self) -> Option<I>;
    fn preds(&self) -> usize;
    fn entry(&self) -> bool;
    fn succs(&self) -> usize;
    fn loop_header(&self) -> bool;
    fn loop_nesting_forest_children(&self) -> usize;
}

pub trait Instr: Clone + Eq + fmt::Display {
    fn defs(&self, out: &mut Vec<VReg>);
    fn uses(&self, out: &mut Vec<VReg>);
    fn assign_use(&self, index: usize, v: VReg);
    fn assign_def(&self, v: VReg);
    fn is_copy(&self) -> bool;
    fn is_call(&self) -> bool;
    fn is_indirect_call(&self) -> bool;
    fn is_return(&self) -> bool;
}
