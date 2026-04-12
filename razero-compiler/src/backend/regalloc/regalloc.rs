//! ISA-agnostic register allocation.

use std::ptr::NonNull;

use crate::wazevoapi::{IDedPool, Pool};

use super::api::{Block, Function, Instr};
use super::reg::{
    RealReg, RegType, VReg, VRegId, NUM_REG_TYPES, REAL_REG_INVALID, VREG_ID_RESERVED_FOR_REAL_NUM,
    VREG_INVALID,
};
use super::regset::{RegInUseSet, RegSet};

const PROGRAM_COUNTER_LIVE_IN: i32 = i32::MIN;
const PROGRAM_COUNTER_LIVE_OUT: i32 = i32::MAX;

pub struct RegisterInfo {
    pub allocatable_registers: [Vec<RealReg>; NUM_REG_TYPES],
    pub callee_saved_registers: RegSet,
    pub caller_saved_registers: RegSet,
    pub real_reg_to_vreg: Vec<VReg>,
    pub real_reg_name: fn(RealReg) -> String,
    pub real_reg_type: fn(RealReg) -> RegType,
}

impl Default for RegisterInfo {
    fn default() -> Self {
        Self {
            allocatable_registers: std::array::from_fn(|_| Vec::new()),
            callee_saved_registers: RegSet::default(),
            caller_saved_registers: RegSet::default(),
            real_reg_to_vreg: Vec::new(),
            real_reg_name: |reg| reg.to_string(),
            real_reg_type: |_| RegType::Invalid,
        }
    }
}

pub struct Allocator<I: Instr, B: Block<I>, F: Function<I, B>> {
    reg_info: RegisterInfo,
    allocatable_set: RegSet,
    allocated_callee_saved_regs: Vec<VReg>,
    vs: Vec<VReg>,
    ss: Vec<VRegId>,
    blks: Vec<B>,
    reals: Vec<RealReg>,
    instrs: Vec<I>,
    copies: Vec<CopyRecord>,
    phi_def_inst_list_pool: Pool<PhiDefInst<I>>,
    block_states: IDedPool<BlockState>,
    state: State<I, B>,
    _marker: std::marker::PhantomData<F>,
}

#[derive(Clone, Copy)]
struct CopyRecord {
    src_id: VRegId,
    dst_id: VRegId,
}

struct State<I: Instr, B: Block<I>> {
    arg_real_regs: Vec<VReg>,
    regs_in_use: RegInUseSet,
    vr_states: IDedPool<VrState<I, B>>,
    current_block_id: i32,
    allocated_reg_set: RegSet,
}

#[derive(Clone, Default)]
struct BlockState {
    live_ins: Vec<VRegId>,
    seen: bool,
    visited: bool,
    start_from_pred_index: isize,
    start_regs: RegInUseSet,
    end_regs: RegInUseSet,
}

struct VrState<I: Instr, B: Block<I>> {
    v: VReg,
    r: RealReg,
    def_instr: Option<I>,
    def_blk: Option<B>,
    lca: Option<B>,
    last_use: i32,
    last_use_updated_at_block_id: i32,
    spilled: bool,
    is_phi: bool,
    desired_loc: DesiredLoc,
    phi_def_inst_list: Option<NonNull<PhiDefInst<I>>>,
}

struct PhiDefInst<I: Instr> {
    instr: Option<I>,
    v: VReg,
    next: Option<NonNull<PhiDefInst<I>>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DesiredLoc {
    #[default]
    Unspecified,
    Stack,
    Reg(RealReg),
}

impl DesiredLoc {
    fn real_reg(self) -> RealReg {
        match self {
            Self::Reg(reg) => reg,
            _ => REAL_REG_INVALID,
        }
    }

    fn stack(self) -> bool {
        matches!(self, Self::Stack)
    }
}

impl<I: Instr, B: Block<I>> Default for VrState<I, B> {
    fn default() -> Self {
        Self {
            v: VREG_INVALID,
            r: REAL_REG_INVALID,
            def_instr: None,
            def_blk: None,
            lca: None,
            last_use: -1,
            last_use_updated_at_block_id: -1,
            spilled: false,
            is_phi: false,
            desired_loc: DesiredLoc::Unspecified,
            phi_def_inst_list: None,
        }
    }
}

impl<I: Instr> Default for PhiDefInst<I> {
    fn default() -> Self {
        Self {
            instr: None,
            v: VREG_INVALID,
            next: None,
        }
    }
}

impl<I: Instr, B: Block<I>> State<I, B> {
    fn new() -> Self {
        Self {
            arg_real_regs: Vec::new(),
            regs_in_use: RegInUseSet::new(),
            vr_states: IDedPool::new(Some(reset_vr_state::<I, B>)),
            current_block_id: -1,
            allocated_reg_set: RegSet::default(),
        }
    }

    fn reset(&mut self) {
        self.arg_real_regs.clear();
        self.regs_in_use.reset();
        self.vr_states.reset();
        self.current_block_id = -1;
        self.allocated_reg_set = RegSet::default();
    }

    fn get_or_allocate_vreg_state(&mut self, v: VReg) -> &mut VrState<I, B> {
        let state = self.vr_states.get_or_allocate(v.id() as usize);
        if state.v == VREG_INVALID {
            state.v = v;
        }
        state
    }

    fn get_vreg_state(&self, v: VRegId) -> Option<&VrState<I, B>> {
        self.vr_states.get(v as usize)
    }

    fn get_vreg_state_mut(&mut self, v: VRegId) -> &mut VrState<I, B> {
        self.vr_states.get_or_allocate(v as usize)
    }

    fn use_real_reg(&mut self, r: RealReg, vreg: VRegId) {
        self.regs_in_use.add(r, vreg);
        self.get_vreg_state_mut(vreg).r = r;
        self.allocated_reg_set = self.allocated_reg_set.add(r);
    }

    fn release_real_reg(&mut self, r: RealReg) {
        if let Some(vreg) = self.regs_in_use.get(r) {
            self.regs_in_use.remove(r);
            self.get_vreg_state_mut(vreg).r = REAL_REG_INVALID;
        }
    }

    fn reset_at(&mut self, bs: &BlockState) {
        let occupied: Vec<(RealReg, VRegId)> = self.regs_in_use.entries().collect();
        for (_, vreg) in occupied {
            self.get_vreg_state_mut(vreg).r = REAL_REG_INVALID;
        }
        self.regs_in_use.reset();
        let current_block_id = self.current_block_id;
        for (reg, vreg) in bs.end_regs.entries() {
            let live_in = {
                let state = self.get_vreg_state_mut(vreg);
                state.last_use_updated_at_block_id == current_block_id
                    && state.last_use == PROGRAM_COUNTER_LIVE_IN
            };
            if live_in {
                self.regs_in_use.add(reg, vreg);
                self.get_vreg_state_mut(vreg).r = reg;
            }
        }
    }
}

fn reset_vr_state<I: Instr, B: Block<I>>(state: &mut VrState<I, B>) {
    *state = VrState::default();
}

fn reset_block_state(state: &mut BlockState) {
    state.live_ins.clear();
    state.seen = false;
    state.visited = false;
    state.start_from_pred_index = -1;
    state.start_regs.reset();
    state.end_regs.reset();
}

fn reset_phi_def_inst_list<I: Instr>(node: &mut PhiDefInst<I>) {
    *node = PhiDefInst::default();
}

impl<I: Instr, B: Block<I>, F: Function<I, B>> Allocator<I, B, F> {
    pub fn new(reg_info: RegisterInfo) -> Self {
        let mut allocatable_set = RegSet::default();
        for regs in &reg_info.allocatable_registers {
            for &reg in regs {
                allocatable_set = allocatable_set.add(reg);
            }
        }

        Self {
            reg_info,
            allocatable_set,
            allocated_callee_saved_regs: Vec::new(),
            vs: Vec::new(),
            ss: Vec::new(),
            blks: Vec::new(),
            reals: Vec::new(),
            instrs: Vec::new(),
            copies: Vec::new(),
            phi_def_inst_list_pool: Pool::new(Some(reset_phi_def_inst_list::<I>)),
            block_states: IDedPool::new(Some(reset_block_state)),
            state: State::new(),
            _marker: std::marker::PhantomData,
        }
    }

    pub fn reset(&mut self) {
        self.state.reset();
        self.block_states.reset();
        self.phi_def_inst_list_pool.reset();
        self.vs.clear();
        self.ss.clear();
        self.blks.clear();
        self.reals.clear();
        self.instrs.clear();
        self.copies.clear();
        self.allocated_callee_saved_regs.clear();
    }

    pub fn do_allocation(&mut self, f: &mut F) {
        self.liveness_analysis(f);
        self.alloc(f);
        self.determine_callee_saved_real_regs(f);
    }

    fn determine_callee_saved_real_regs(&mut self, f: &mut F) {
        self.allocated_callee_saved_regs.clear();
        self.state.allocated_reg_set.for_each(|allocated| {
            if self.reg_info.callee_saved_registers.has(allocated) {
                if let Some(vreg) = self.reg_info.real_reg_to_vreg.get(allocated.0 as usize) {
                    self.allocated_callee_saved_regs.push(*vreg);
                }
            }
        });
        f.clobbered_registers(&self.allocated_callee_saved_regs);
    }

    fn get_or_allocate_block_state(&mut self, block_id: i32) -> &mut BlockState {
        self.block_states
            .get_or_allocate(Self::block_state_key(block_id))
    }

    fn block_state_key(block_id: i32) -> usize {
        block_id as u32 as usize
    }

    fn phi_blk(&self, vreg: VRegId) -> Option<B> {
        let state = self.state.get_vreg_state(vreg)?;
        if state.is_phi {
            state.def_blk.clone()
        } else {
            None
        }
    }

    fn update_live_in_vr_state(&mut self, live_ins: &[VRegId]) {
        let current_block_id = self.state.current_block_id;
        for &vreg in live_ins {
            let state = self.state.get_vreg_state_mut(vreg);
            state.last_use = PROGRAM_COUNTER_LIVE_IN;
            state.last_use_updated_at_block_id = current_block_id;
        }
    }

    fn record_reload(&mut self, f: &F, vreg: VRegId, blk: B) {
        let state = self.state.get_vreg_state_mut(vreg);
        state.spilled = true;
        match state.lca.clone() {
            None => state.lca = Some(blk),
            Some(lca) if lca != blk => state.lca = Some(f.lowest_common_ancestor(lca, blk)),
            _ => {}
        }
    }

    pub(crate) fn liveness_analysis(&mut self, f: &F) {
        self.state.reset();
        self.block_states.reset();
        self.phi_def_inst_list_pool.reset();

        for index in 0..VREG_ID_RESERVED_FOR_REAL_NUM {
            self.state
                .get_or_allocate_vreg_state(VReg::new(index).set_real_reg(RealReg(index as u8)));
        }

        self.blks.clear();
        f.post_order_blocks(&mut self.blks);
        let blocks = self.blks.clone();
        for blk in blocks {
            self.vs.clear();
            f.block_params(blk.clone(), &mut self.vs);
            for param in self.vs.iter().copied() {
                let vs = self.state.get_or_allocate_vreg_state(param);
                vs.is_phi = true;
                vs.def_blk = Some(blk.clone());
            }

            let blk_id = blk.id();
            self.get_or_allocate_block_state(blk_id);

            self.ss.clear();
            for succ_index in 0..blk.succs() {
                let succ = f.succ(blk.clone(), succ_index);
                let succ_state = match self.block_states.get(Self::block_state_key(succ.id())) {
                    Some(state) if state.seen => state,
                    _ => continue,
                };
                for &live_in in &succ_state.live_ins {
                    if self.phi_blk(live_in) != Some(succ.clone()) {
                        let state = self.state.get_vreg_state_mut(live_in);
                        if !state.spilled {
                            state.spilled = true;
                            self.ss.push(live_in);
                        }
                    }
                }
            }

            self.instrs.clear();
            blk.instructions_rev(&mut self.instrs);
            for instr in self.instrs.iter().cloned() {
                let mut def_is_phi = false;
                self.vs.clear();
                instr.defs(&mut self.vs);
                for def in self.vs.iter().copied() {
                    if !def.is_real_reg() {
                        let state = self.state.get_or_allocate_vreg_state(def);
                        def_is_phi = state.is_phi;
                        state.spilled = false;
                    }
                }

                self.vs.clear();
                instr.uses(&mut self.vs);
                for use_ in self.vs.iter().copied() {
                    if use_.is_real_reg() && !self.allocatable_set.has(use_.real_reg()) {
                        continue;
                    }
                    let state = self.state.get_or_allocate_vreg_state(use_);
                    if !state.spilled {
                        state.spilled = true;
                        self.ss.push(use_.id());
                    }
                }

                if def_is_phi {
                    if let Some(use_) = self.vs.last().copied() {
                        if use_.valid()
                            && use_.is_real_reg()
                            && self.allocatable_set.has(use_.real_reg())
                        {
                            self.state.arg_real_regs.push(use_);
                        }
                    }
                }
            }

            let live_ins: Vec<VRegId> = self
                .ss
                .iter()
                .copied()
                .filter(|&vreg| {
                    self.state
                        .get_vreg_state(vreg)
                        .is_some_and(|state| state.spilled)
                })
                .collect();
            for &vreg in &live_ins {
                self.state.get_vreg_state_mut(vreg).spilled = false;
            }
            let info = self.get_or_allocate_block_state(blk_id);
            info.live_ins.extend(live_ins);
            info.seen = true;
        }

        self.blks.clear();
        f.loop_nesting_forest_roots(&mut self.blks);
        for root in self.blks.clone() {
            self.loop_tree_dfs(f, root);
        }
    }

    fn loop_tree_dfs(&mut self, f: &F, entry: B) {
        self.blks.clear();
        self.blks.push(entry);

        while let Some(loop_blk) = self.blks.pop() {
            self.ss.clear();
            let info = self
                .block_states
                .get(Self::block_state_key(loop_blk.id()))
                .expect("loop block state must exist")
                .clone();
            for vreg in info.live_ins {
                if self.phi_blk(vreg) != Some(loop_blk.clone()) {
                    self.ss.push(vreg);
                    self.state.get_vreg_state_mut(vreg).spilled = true;
                }
            }

            let mut sibling_added = Vec::new();
            let child_count = loop_blk.loop_nesting_forest_children();
            for index in 0..child_count {
                let child = f.loop_nesting_forest_child(loop_blk.clone(), index);
                let child_id = child.id();
                if index == 0 {
                    let additions: Vec<VRegId> = self
                        .ss
                        .iter()
                        .copied()
                        .filter(|&vreg| {
                            self.state
                                .get_vreg_state(vreg)
                                .is_some_and(|state| state.spilled)
                        })
                        .collect();
                    for &vreg in &additions {
                        self.state.get_vreg_state_mut(vreg).spilled = false;
                    }
                    let child_state = self.get_or_allocate_block_state(child_id);
                    child_state.live_ins.extend(additions.iter().copied());
                    sibling_added.extend(additions);
                } else {
                    self.get_or_allocate_block_state(child_id)
                        .live_ins
                        .extend_from_slice(&sibling_added);
                }

                if child.loop_header() {
                    self.blks.push(child);
                }
            }

            if child_count == 0 {
                for &vreg in &self.ss {
                    self.state.get_vreg_state_mut(vreg).spilled = false;
                }
            }
        }
    }

    fn alloc(&mut self, f: &mut F) {
        self.blks.clear();
        f.reverse_post_order_blocks(&mut self.blks);
        let blocks = self.blks.clone();
        for blk in blocks {
            if blk.entry() {
                self.finalize_start_reg(f, blk.clone());
            }
            self.alloc_block(f, blk);
        }

        for blk in self.blks.clone() {
            self.fix_merge_state(f, blk);
        }
        self.schedule_spills(f);
    }

    fn finalize_start_reg(&mut self, f: &F, blk: B) {
        let block_id = blk.id();
        if self
            .block_states
            .get(Self::block_state_key(block_id))
            .is_some_and(|state| state.start_from_pred_index > -1)
        {
            return;
        }

        self.state.current_block_id = block_id;
        let live_ins = self
            .block_states
            .get(Self::block_state_key(block_id))
            .map(|state| state.live_ins.clone())
            .unwrap_or_default();
        self.update_live_in_vr_state(&live_ins);

        let mut start_from_pred_index = -1isize;
        let mut pred_state = None;
        match blk.preds() {
            0 => {}
            1 => {
                let pred = f.pred(blk.clone(), 0);
                pred_state = self
                    .block_states
                    .get(Self::block_state_key(pred.id()))
                    .cloned();
                start_from_pred_index = 0;
            }
            preds => {
                for index in 0..preds {
                    let pred = f.pred(blk.clone(), index);
                    if let Some(state) = self.block_states.get(Self::block_state_key(pred.id())) {
                        if state.visited {
                            pred_state = Some(state.clone());
                            start_from_pred_index = index as isize;
                            break;
                        }
                    }
                }
            }
        }

        if let Some(pred_state) = pred_state {
            self.state.reset_at(&pred_state);
        } else {
            assert!(blk.entry(), "at least one predecessor must be visited");
            for &vreg in &live_ins {
                if let Some(state) = self.state.get_vreg_state(vreg) {
                    if state.v.is_real_reg() {
                        self.state.use_real_reg(state.v.real_reg(), vreg);
                    }
                }
            }
            for arg in self.state.arg_real_regs.clone() {
                if !self.state.regs_in_use.has(arg.real_reg()) {
                    self.state.use_real_reg(arg.real_reg(), arg.id());
                }
            }
            start_from_pred_index = 0;
        }

        let start_regs: Vec<(RealReg, VRegId)> = self.state.regs_in_use.entries().collect();
        let current = self.get_or_allocate_block_state(block_id);
        current.start_from_pred_index = start_from_pred_index;
        current.start_regs.reset();
        for (reg, vreg) in start_regs {
            current.start_regs.add(reg, vreg);
        }
    }

    fn alloc_block(&mut self, f: &mut F, blk: B) {
        let block_id = blk.id();
        self.state.current_block_id = block_id;

        let current_state = self
            .block_states
            .get(Self::block_state_key(block_id))
            .cloned()
            .expect("block state must exist");
        assert!(
            current_state.start_from_pred_index >= 0,
            "start state must be finalized before allocation"
        );

        let occupied: Vec<(RealReg, VRegId)> = self.state.regs_in_use.entries().collect();
        for (_, vreg) in occupied {
            self.state.get_vreg_state_mut(vreg).r = REAL_REG_INVALID;
        }
        self.state.regs_in_use.reset();
        for (reg, vreg) in current_state.start_regs.entries() {
            self.state.use_real_reg(reg, vreg);
        }

        let mut desired_updated = Vec::new();
        self.copies.clear();
        self.instrs.clear();
        blk.instructions(&mut self.instrs);
        let instrs = self.instrs.clone();

        for (pc, instr) in instrs.iter().cloned().enumerate() {
            self.vs.clear();
            instr.uses(&mut self.vs);
            for use_ in self.vs.iter().copied() {
                if !use_.is_real_reg() {
                    continue;
                }
                let reg = use_.real_reg();
                if let Some(vreg) = self.state.regs_in_use.get(reg) {
                    self.state.get_vreg_state_mut(vreg).last_use = pc as i32;
                }
            }
        }

        for (pc, instr) in instrs.iter().cloned().enumerate() {
            self.vs.clear();
            instr.uses(&mut self.vs);
            for use_ in self.vs.iter().copied() {
                if !use_.is_real_reg() {
                    self.state.get_vreg_state_mut(use_.id()).last_use = pc as i32;
                }
            }

            if instr.is_copy() {
                self.vs.clear();
                instr.defs(&mut self.vs);
                let def = self.vs[0];
                let src = Self::inferred_copy_source(&instr)
                    .expect("copy instructions must have a non-real source");
                self.copies.push(CopyRecord {
                    src_id: src.id(),
                    dst_id: def.id(),
                });
                let r = def.real_reg();
                if r != REAL_REG_INVALID {
                    let src_state = self.state.get_vreg_state_mut(src.id());
                    if !src_state.is_phi {
                        src_state.desired_loc = DesiredLoc::Reg(r);
                        desired_updated.push(src.id());
                    }
                }
            }
        }

        for succ_index in 0..blk.succs() {
            let succ = f.succ(blk.clone(), succ_index);
            let succ_state = match self
                .block_states
                .get(Self::block_state_key(succ.id()))
                .cloned()
            {
                Some(state) => state,
                None => continue,
            };

            for vreg in succ_state.live_ins {
                if self.phi_blk(vreg) != Some(succ.clone()) {
                    self.state.get_vreg_state_mut(vreg).last_use = PROGRAM_COUNTER_LIVE_OUT;
                }
            }

            if succ_state.start_from_pred_index > -1 {
                for (reg, vreg) in succ_state.start_regs.entries() {
                    self.state.get_vreg_state_mut(vreg).desired_loc = DesiredLoc::Reg(reg);
                    desired_updated.push(vreg);
                }

                self.vs.clear();
                f.block_params(succ, &mut self.vs);
                for param in self.vs.iter().copied() {
                    let state = self.state.get_vreg_state_mut(param.id());
                    if state.desired_loc == DesiredLoc::Unspecified {
                        state.desired_loc = DesiredLoc::Stack;
                        desired_updated.push(param.id());
                    }
                }
            }
        }

        for copy in self.copies.clone() {
            let desired = self
                .state
                .get_vreg_state(copy.dst_id)
                .map(|state| state.desired_loc)
                .unwrap_or_default();
            let src_state = self.state.get_vreg_state_mut(copy.src_id);
            if !src_state.is_phi && src_state.desired_loc == DesiredLoc::Unspecified {
                src_state.desired_loc = desired;
                desired_updated.push(copy.src_id);
            }
        }

        for (pc, instr) in instrs.iter().cloned().enumerate() {
            let mut current_used = RegSet::default();
            let mut kill_set = Vec::new();

            self.vs.clear();
            instr.uses(&mut self.vs);
            let uses = self.vs.clone();

            for use_ in uses.iter().copied() {
                if use_.is_real_reg() {
                    let reg = use_.real_reg();
                    current_used = current_used.add(reg);
                    if self.allocatable_set.has(reg)
                        && self
                            .state
                            .regs_in_use
                            .get(reg)
                            .and_then(|vreg| self.state.get_vreg_state(vreg))
                            .is_some_and(|state| state.last_use == pc as i32)
                    {
                        kill_set.push(reg);
                    }
                } else if let Some(reg) = self.state.get_vreg_state(use_.id()).map(|vs| vs.r) {
                    if reg != REAL_REG_INVALID {
                        current_used = current_used.add(reg);
                    }
                }
            }

            for (index, use_) in uses.iter().copied().enumerate() {
                if use_.is_real_reg() {
                    continue;
                }

                let killed = self
                    .state
                    .get_vreg_state(use_.id())
                    .map(|state| state.last_use == pc as i32)
                    .unwrap_or(false);
                let mut reg = self
                    .state
                    .get_vreg_state(use_.id())
                    .map(|state| state.r)
                    .unwrap_or(REAL_REG_INVALID);

                if reg == REAL_REG_INVALID {
                    let preferred = self
                        .state
                        .get_vreg_state(use_.id())
                        .map(|state| state.desired_loc.real_reg())
                        .unwrap_or(REAL_REG_INVALID);
                    let allocatable =
                        self.reg_info.allocatable_registers[use_.reg_type().index()].clone();
                    reg = self.find_or_spill_allocatable(&allocatable, current_used, preferred);
                    self.record_reload(f, use_.id(), blk.clone());
                    f.reload_register_before(use_.set_real_reg(reg), Some(instr.clone()));
                    self.state.use_real_reg(reg, use_.id());
                }

                instr.assign_use(index, use_.set_real_reg(reg));
                current_used = current_used.add(reg);
                if killed {
                    kill_set.push(reg);
                }
            }

            if instr.is_call() || instr.is_indirect_call() {
                let addr = if instr.is_indirect_call() {
                    uses.first()
                        .map(|use_| use_.real_reg())
                        .unwrap_or(REAL_REG_INVALID)
                } else {
                    REAL_REG_INVALID
                };
                self.release_caller_saved_regs(addr);
            }

            let live_use_regs = current_used;
            for reg in &kill_set {
                self.state.release_real_reg(*reg);
            }

            self.vs.clear();
            instr.defs(&mut self.vs);
            let defs = self.vs.clone();
            match defs.len() {
                0 => {}
                1 => {
                    let def = defs[0];
                    let vreg_id = def.id();
                    if def.is_real_reg() {
                        let reg = def.real_reg();
                        if self.allocatable_set.has(reg) {
                            if self.state.regs_in_use.has(reg) {
                                self.state.release_real_reg(reg);
                            }
                            self.state.use_real_reg(reg, vreg_id);
                        }
                    } else {
                        let desired = self
                            .state
                            .get_vreg_state(vreg_id)
                            .map(|state| state.desired_loc.real_reg())
                            .unwrap_or(REAL_REG_INVALID);
                        let mut reg = self
                            .state
                            .get_vreg_state(vreg_id)
                            .map(|state| state.r)
                            .unwrap_or(REAL_REG_INVALID);

                        if reg != REAL_REG_INVALID && live_use_regs.has(reg) {
                            self.state.release_real_reg(reg);
                            reg = REAL_REG_INVALID;
                        }

                        if desired != REAL_REG_INVALID
                            && reg != desired
                            && !live_use_regs.has(desired)
                            && (!self.state.regs_in_use.has(desired) || reg == REAL_REG_INVALID)
                        {
                            if reg != REAL_REG_INVALID {
                                self.state.release_real_reg(reg);
                            }
                            self.state.release_real_reg(desired);
                            reg = desired;
                            self.state.use_real_reg(reg, vreg_id);
                        }

                        if reg == REAL_REG_INVALID {
                            if instr.is_copy() {
                                let copy_src =
                                    Self::inferred_copy_source(&instr).map(|src| src.real_reg());
                                if let Some(copy_src) = copy_src {
                                    if self.allocatable_set.has(copy_src)
                                        && !live_use_regs.has(copy_src)
                                        && !self.state.regs_in_use.has(copy_src)
                                    {
                                        reg = copy_src;
                                    }
                                }
                            }
                            if reg == REAL_REG_INVALID {
                                let allocatable = self.reg_info.allocatable_registers
                                    [def.reg_type().index()]
                                .clone();
                                reg = self.find_or_spill_allocatable(
                                    &allocatable,
                                    live_use_regs,
                                    REAL_REG_INVALID,
                                );
                            }
                            self.state.use_real_reg(reg, vreg_id);
                        }

                        let assigned = def.set_real_reg(reg);
                        instr.assign_def(assigned);

                        if self
                            .state
                            .get_vreg_state(vreg_id)
                            .map(|state| state.is_phi)
                            .unwrap_or(false)
                        {
                            if self
                                .state
                                .get_vreg_state(vreg_id)
                                .map(|state| state.desired_loc.stack())
                                .unwrap_or(false)
                            {
                                f.store_register_after(assigned, Some(instr.clone()));
                                self.state.release_real_reg(reg);
                            } else {
                                let next = self
                                    .state
                                    .get_vreg_state(vreg_id)
                                    .and_then(|state| state.phi_def_inst_list);
                                let node = self.phi_def_inst_list_pool.allocate();
                                node.instr = Some(instr.clone());
                                node.v = assigned;
                                node.next = next;
                                self.state.get_vreg_state_mut(vreg_id).phi_def_inst_list =
                                    Some(NonNull::from(node));
                            }
                        } else {
                            let state = self.state.get_vreg_state_mut(vreg_id);
                            state.def_instr = Some(instr);
                            state.def_blk = Some(blk.clone());
                        }
                    }
                }
                _ => {
                    for def in defs {
                        assert!(def.is_real_reg(), "multiple defs must be precolored");
                        let reg = def.real_reg();
                        if self.state.regs_in_use.has(reg) {
                            self.state.release_real_reg(reg);
                        }
                        self.state.use_real_reg(reg, def.id());
                    }
                }
            }
        }

        let end_regs: Vec<(RealReg, VRegId)> = self.state.regs_in_use.entries().collect();
        let end_state = self.get_or_allocate_block_state(block_id);
        end_state.end_regs.reset();
        for (reg, vreg) in end_regs {
            end_state.end_regs.add(reg, vreg);
        }
        end_state.visited = true;

        for vreg in desired_updated {
            self.state.get_vreg_state_mut(vreg).desired_loc = DesiredLoc::Unspecified;
        }

        for succ_index in 0..blk.succs() {
            let succ = f.succ(blk.clone(), succ_index);
            self.finalize_start_reg(f, succ);
        }
    }

    fn inferred_copy_source(instr: &I) -> Option<VReg> {
        let mut uses = Vec::new();
        instr.uses(&mut uses);
        uses.into_iter().find(|use_| !use_.is_real_reg())
    }

    pub(crate) fn find_or_spill_allocatable(
        &mut self,
        allocatable: &[RealReg],
        forbidden_mask: RegSet,
        preferred: RealReg,
    ) -> RealReg {
        if preferred != REAL_REG_INVALID
            && !forbidden_mask.has(preferred)
            && !self.state.regs_in_use.has(preferred)
        {
            return preferred;
        }

        let mut chosen = REAL_REG_INVALID;
        let mut chosen_last_use = -1;
        for &candidate in allocatable {
            if forbidden_mask.has(candidate) {
                continue;
            }

            let Some(using) = self.state.regs_in_use.get(candidate) else {
                return candidate;
            };

            let state = self
                .state
                .get_vreg_state(using)
                .expect("register occupant must exist");
            if state.v.is_real_reg() {
                continue;
            }

            let is_preferred = candidate == preferred;
            let last_use = state.last_use;
            if chosen == REAL_REG_INVALID
                || is_preferred
                || last_use == -1
                || (chosen_last_use != -1 && last_use > chosen_last_use)
            {
                chosen = candidate;
                chosen_last_use = last_use;
                if is_preferred {
                    break;
                }
            }
        }

        assert_ne!(chosen, REAL_REG_INVALID, "no allocatable register found");
        self.state.release_real_reg(chosen);
        chosen
    }

    fn find_allocatable(&self, allocatable: &[RealReg], forbidden_mask: RegSet) -> RealReg {
        for &reg in allocatable {
            if !self.state.regs_in_use.has(reg) && !forbidden_mask.has(reg) {
                return reg;
            }
        }
        REAL_REG_INVALID
    }

    fn release_caller_saved_regs(&mut self, addr_reg: RealReg) {
        let occupied: Vec<(RealReg, VRegId)> = self.state.regs_in_use.entries().collect();
        for (reg, vreg) in occupied {
            if reg == addr_reg {
                continue;
            }
            let state = self
                .state
                .get_vreg_state(vreg)
                .expect("register occupant must exist");
            if state.v.is_real_reg() || !self.reg_info.caller_saved_registers.has(reg) {
                continue;
            }
            self.state.release_real_reg(reg);
        }
    }

    fn fix_merge_state(&mut self, f: &mut F, blk: B) {
        if blk.preds() <= 1 {
            return;
        }

        let blk_state = self
            .block_states
            .get(Self::block_state_key(blk.id()))
            .cloned()
            .expect("block state must exist");
        let mut desired_occupants_set = RegSet::default();
        for (reg, _) in blk_state.start_regs.entries() {
            desired_occupants_set = desired_occupants_set.add(reg);
        }

        self.state.current_block_id = blk.id();
        self.update_live_in_vr_state(&blk_state.live_ins);

        for pred_index in 0..blk.preds() {
            if pred_index as isize == blk_state.start_from_pred_index {
                continue;
            }

            let pred = f.pred(blk.clone(), pred_index);
            let pred_state = self
                .block_states
                .get(Self::block_state_key(pred.id()))
                .cloned()
                .expect("predecessor state must exist");
            self.state.reset_at(&pred_state);

            let int_tmp = self.find_allocatable(
                &self.reg_info.allocatable_registers[RegType::Int.index()],
                desired_occupants_set,
            );
            let float_tmp = self.find_allocatable(
                &self.reg_info.allocatable_registers[RegType::Float.index()],
                desired_occupants_set,
            );

            for reg_index in 0..64u8 {
                let reg = RealReg(reg_index);
                let Some(desired_vreg) = blk_state.start_regs.get(reg) else {
                    continue;
                };
                if self.state.regs_in_use.get(reg) == Some(desired_vreg) {
                    continue;
                }
                let typ = self
                    .state
                    .get_vreg_state(desired_vreg)
                    .map(|state| state.v.reg_type())
                    .unwrap_or(RegType::Invalid);
                let tmp = if typ == RegType::Int {
                    if int_tmp == REAL_REG_INVALID {
                        VREG_INVALID
                    } else {
                        VReg::from_real_reg(int_tmp, RegType::Int)
                    }
                } else if float_tmp == REAL_REG_INVALID {
                    VREG_INVALID
                } else {
                    VReg::from_real_reg(float_tmp, RegType::Float)
                };

                self.reconcile_edge(
                    f,
                    reg,
                    pred.clone(),
                    self.state.regs_in_use.get(reg),
                    desired_vreg,
                    tmp,
                    typ,
                );
            }
        }
    }

    fn reconcile_edge(
        &mut self,
        f: &mut F,
        reg: RealReg,
        pred: B,
        current_state: Option<VRegId>,
        desired_state: VRegId,
        free_reg: VReg,
        typ: RegType,
    ) {
        let desired_vreg = self
            .state
            .get_vreg_state(desired_state)
            .expect("desired vreg must exist")
            .v;
        if desired_vreg.is_real_reg() && desired_vreg.real_reg() == reg {
            if let Some(current_vreg_id) = current_state.filter(|&id| id != desired_state) {
                let current_vreg = self
                    .state
                    .get_vreg_state(current_vreg_id)
                    .expect("current vreg must exist")
                    .v;
                if !current_vreg.is_real_reg() {
                    f.store_register_before(
                        current_vreg.set_real_reg(reg),
                        pred.last_instr_for_insertion(),
                    );
                }
                self.state.release_real_reg(reg);
            }
            self.state.use_real_reg(reg, desired_state);
            return;
        }
        if let Some(current_vreg_id) = current_state {
            let current_vreg = self
                .state
                .get_vreg_state(current_vreg_id)
                .expect("current vreg must exist")
                .v;
            let existing_reg = self
                .state
                .get_vreg_state(desired_state)
                .expect("desired vreg must exist")
                .r;
            if existing_reg == REAL_REG_INVALID {
                f.store_register_before(
                    current_vreg.set_real_reg(reg),
                    pred.last_instr_for_insertion(),
                );
                self.state.release_real_reg(reg);
                self.record_reload(f, desired_state, pred.clone());
                f.reload_register_before(
                    desired_vreg.set_real_reg(reg),
                    pred.last_instr_for_insertion(),
                );
                self.state.use_real_reg(reg, desired_state);
            } else {
                f.swap_before(
                    current_vreg.set_real_reg(reg),
                    desired_vreg.set_real_reg(existing_reg),
                    free_reg,
                    pred.last_instr_for_insertion(),
                );
                if free_reg.is_real_reg() {
                    self.state.allocated_reg_set =
                        self.state.allocated_reg_set.add(free_reg.real_reg());
                }
                self.state.release_real_reg(reg);
                self.state.release_real_reg(existing_reg);
                self.state.use_real_reg(reg, desired_state);
                self.state.use_real_reg(existing_reg, current_vreg_id);
            }
        } else {
            let existing_reg = self
                .state
                .get_vreg_state(desired_state)
                .expect("desired vreg must exist")
                .r;
            if existing_reg != REAL_REG_INVALID {
                f.insert_move_before(
                    VReg::from_real_reg(reg, typ),
                    desired_vreg.set_real_reg(existing_reg),
                    pred.last_instr_for_insertion(),
                );
                self.state.release_real_reg(existing_reg);
            } else {
                self.record_reload(f, desired_state, pred.clone());
                f.reload_register_before(
                    desired_vreg.set_real_reg(reg),
                    pred.last_instr_for_insertion(),
                );
            }
            self.state.use_real_reg(reg, desired_state);
        }
    }

    fn schedule_spills(&mut self, f: &mut F) {
        for index in 0..=self.state.vr_states.max_id_encountered().max(0) as usize {
            let spilled = self
                .state
                .vr_states
                .get(index)
                .map(|state| state.spilled && !state.v.is_real_reg())
                .unwrap_or(false);
            if spilled {
                self.schedule_spill(f, index as VRegId);
            }
        }
    }

    fn schedule_spill(&mut self, f: &mut F, vreg: VRegId) {
        let state = self
            .state
            .get_vreg_state(vreg)
            .expect("spilled vreg must exist");
        if state.is_phi {
            let mut current = state.phi_def_inst_list;
            while let Some(node) = current {
                let node_ref = unsafe { node.as_ref() };
                f.store_register_after(node_ref.v, node_ref.instr.clone());
                current = node_ref.next;
            }
            return;
        }

        let mut pos = state.lca.clone().expect("spilled vreg must have lca");
        let defining_blk = state
            .def_blk
            .clone()
            .expect("spilled vreg must have defining block");
        let mut reg = REAL_REG_INVALID;
        while pos != defining_blk {
            let block_state = self
                .block_states
                .get(Self::block_state_key(pos.id()))
                .expect("ancestor block state must exist");
            for (candidate, occupant) in block_state.start_regs.entries() {
                if occupant == vreg {
                    reg = candidate;
                    break;
                }
            }
            if reg != REAL_REG_INVALID {
                break;
            }
            pos = f.idom(pos);
        }

        if pos == defining_blk {
            let def_instr = self
                .state
                .get_vreg_state(vreg)
                .and_then(|state| state.def_instr.clone())
                .expect("spilled vreg must have defining instr");
            self.vs.clear();
            def_instr.defs(&mut self.vs);
            f.store_register_after(self.vs[0], Some(def_instr));
        } else {
            f.store_register_after(
                self.state
                    .get_vreg_state(vreg)
                    .expect("spilled vreg must exist")
                    .v
                    .set_real_reg(reg),
                pos.first_instr(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::fmt;
    use std::rc::Rc;

    use super::super::api::{Block, Function, Instr};
    use super::super::reg::{RealReg, RegType, VReg};
    use super::{Allocator, RegisterInfo};
    use crate::backend::regalloc::regset::RegSet;

    #[derive(Clone)]
    struct MockInstr(Rc<RefCell<MockInstrData>>);

    #[derive(Clone, Default)]
    struct MockInstrData {
        defs: Vec<VReg>,
        uses: Vec<VReg>,
        is_copy: bool,
        is_call: bool,
        is_indirect: bool,
    }

    impl PartialEq for MockInstr {
        fn eq(&self, other: &Self) -> bool {
            Rc::ptr_eq(&self.0, &other.0)
        }
    }

    impl Eq for MockInstr {}

    impl fmt::Display for MockInstr {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let data = self.0.borrow();
            write!(f, "mockInstr{{defs={:?}, uses={:?}}}", data.defs, data.uses)
        }
    }

    impl MockInstr {
        fn new() -> Self {
            Self(Rc::new(RefCell::new(MockInstrData::default())))
        }

        fn use_(self, uses: impl IntoIterator<Item = VReg>) -> Self {
            self.0.borrow_mut().uses = uses.into_iter().collect();
            self
        }

        fn def(self, defs: impl IntoIterator<Item = VReg>) -> Self {
            self.0.borrow_mut().defs = defs.into_iter().collect();
            self
        }

        fn copy(self) -> Self {
            self.0.borrow_mut().is_copy = true;
            self
        }
    }

    impl Instr for MockInstr {
        fn defs(&self, out: &mut Vec<VReg>) {
            let data = self.0.borrow();
            out.clear();
            out.extend(data.defs.iter().copied());
        }

        fn uses(&self, out: &mut Vec<VReg>) {
            let data = self.0.borrow();
            out.clear();
            out.extend(data.uses.iter().copied());
        }

        fn assign_use(&self, index: usize, v: VReg) {
            let mut data = self.0.borrow_mut();
            if index >= data.uses.len() {
                data.uses.resize(index + 1, VReg::default());
            }
            data.uses[index] = v;
        }

        fn assign_def(&self, v: VReg) {
            self.0.borrow_mut().defs = vec![v];
        }

        fn is_copy(&self) -> bool {
            self.0.borrow().is_copy
        }

        fn is_call(&self) -> bool {
            self.0.borrow().is_call
        }

        fn is_indirect_call(&self) -> bool {
            self.0.borrow().is_indirect
        }

        fn is_return(&self) -> bool {
            false
        }
    }

    #[derive(Clone)]
    struct MockBlock(Rc<RefCell<MockBlockData>>);

    #[derive(Clone, Default)]
    struct MockBlockData {
        id: i32,
        instructions: Vec<MockInstr>,
        preds: Vec<MockBlock>,
        succs: Vec<MockBlock>,
        entry: bool,
        loop_header: bool,
        lnf_children: Vec<MockBlock>,
        block_params: Vec<VReg>,
    }

    impl PartialEq for MockBlock {
        fn eq(&self, other: &Self) -> bool {
            self.0.borrow().id == other.0.borrow().id
        }
    }

    impl Eq for MockBlock {}

    impl MockBlock {
        fn new(id: i32, instructions: Vec<MockInstr>) -> Self {
            Self(Rc::new(RefCell::new(MockBlockData {
                id,
                instructions,
                ..Default::default()
            })))
        }

        fn entry(self) -> Self {
            self.0.borrow_mut().entry = true;
            self
        }

        fn loop_header(self, children: Vec<MockBlock>) -> Self {
            let mut data = self.0.borrow_mut();
            data.loop_header = true;
            data.lnf_children = children;
            drop(data);
            self
        }

        fn add_pred(&self, pred: MockBlock) {
            self.0.borrow_mut().preds.push(pred.clone());
            pred.0.borrow_mut().succs.push(self.clone());
        }

        fn add_block_param(&self, v: VReg) {
            self.0.borrow_mut().block_params.push(v);
        }
    }

    impl Block<MockInstr> for MockBlock {
        fn id(&self) -> i32 {
            self.0.borrow().id
        }

        fn instructions(&self, out: &mut Vec<MockInstr>) {
            let data = self.0.borrow();
            out.clear();
            out.extend(data.instructions.iter().cloned());
        }

        fn instructions_rev(&self, out: &mut Vec<MockInstr>) {
            let data = self.0.borrow();
            out.clear();
            out.extend(data.instructions.iter().rev().cloned());
        }

        fn first_instr(&self) -> Option<MockInstr> {
            self.0.borrow().instructions.first().cloned()
        }

        fn last_instr_for_insertion(&self) -> Option<MockInstr> {
            self.0.borrow().instructions.last().cloned()
        }

        fn preds(&self) -> usize {
            self.0.borrow().preds.len()
        }

        fn entry(&self) -> bool {
            self.0.borrow().entry
        }

        fn succs(&self) -> usize {
            self.0.borrow().succs.len()
        }

        fn loop_header(&self) -> bool {
            self.0.borrow().loop_header
        }

        fn loop_nesting_forest_children(&self) -> usize {
            self.0.borrow().lnf_children.len()
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct StoreOrReloadInfo {
        reload: bool,
        v: VReg,
    }

    struct MockFunction {
        blocks: Vec<MockBlock>,
        lnf_roots: Vec<MockBlock>,
        befores: Vec<StoreOrReloadInfo>,
        afters: Vec<StoreOrReloadInfo>,
        clobbered: Vec<VReg>,
        idoms: HashMap<i32, i32>,
        lcas: HashMap<(i32, i32), i32>,
    }

    impl MockFunction {
        fn new(blocks: Vec<MockBlock>) -> Self {
            Self {
                blocks,
                lnf_roots: Vec::new(),
                befores: Vec::new(),
                afters: Vec::new(),
                clobbered: Vec::new(),
                idoms: HashMap::new(),
                lcas: HashMap::new(),
            }
        }
    }

    impl Function<MockInstr, MockBlock> for MockFunction {
        fn post_order_blocks(&self, out: &mut Vec<MockBlock>) {
            out.clear();
            out.extend(self.blocks.iter().rev().cloned());
        }

        fn reverse_post_order_blocks(&self, out: &mut Vec<MockBlock>) {
            out.clear();
            out.extend(self.blocks.iter().cloned());
        }

        fn clobbered_registers(&mut self, regs: &[VReg]) {
            self.clobbered.clear();
            self.clobbered.extend_from_slice(regs);
        }

        fn loop_nesting_forest_roots(&self, out: &mut Vec<MockBlock>) {
            out.clear();
            out.extend(self.lnf_roots.iter().cloned());
        }

        fn loop_nesting_forest_child(&self, block: MockBlock, index: usize) -> MockBlock {
            block.0.borrow().lnf_children[index].clone()
        }

        fn lowest_common_ancestor(&self, blk1: MockBlock, blk2: MockBlock) -> MockBlock {
            let a = blk1.id();
            let b = blk2.id();
            let key = if a <= b { (a, b) } else { (b, a) };
            let lca = self.lcas.get(&key).copied().unwrap_or(a);
            self.blocks
                .iter()
                .find(|block| block.id() == lca)
                .cloned()
                .unwrap()
        }

        fn idom(&self, blk: MockBlock) -> MockBlock {
            let idom = self.idoms.get(&blk.id()).copied().unwrap_or(blk.id());
            self.blocks
                .iter()
                .find(|block| block.id() == idom)
                .cloned()
                .unwrap()
        }

        fn pred(&self, block: MockBlock, index: usize) -> MockBlock {
            block.0.borrow().preds[index].clone()
        }

        fn succ(&self, block: MockBlock, index: usize) -> MockBlock {
            block.0.borrow().succs[index].clone()
        }

        fn block_params(&self, block: MockBlock, out: &mut Vec<VReg>) {
            let data = block.0.borrow();
            out.clear();
            out.extend(data.block_params.iter().copied());
        }

        fn swap_before(&mut self, _x1: VReg, _x2: VReg, _tmp: VReg, _instr: Option<MockInstr>) {}

        fn store_register_before(&mut self, v: VReg, _instr: Option<MockInstr>) {
            self.befores.push(StoreOrReloadInfo { reload: false, v });
        }

        fn store_register_after(&mut self, v: VReg, _instr: Option<MockInstr>) {
            self.afters.push(StoreOrReloadInfo { reload: false, v });
        }

        fn reload_register_before(&mut self, v: VReg, _instr: Option<MockInstr>) {
            self.befores.push(StoreOrReloadInfo { reload: true, v });
        }

        fn reload_register_after(&mut self, v: VReg, _instr: Option<MockInstr>) {
            self.afters.push(StoreOrReloadInfo { reload: true, v });
        }

        fn insert_move_before(&mut self, _dst: VReg, _src: VReg, _instr: Option<MockInstr>) {}
    }

    fn int_vreg(id: u32) -> VReg {
        VReg::new(id).set_reg_type(RegType::Int)
    }

    fn real_vreg(reg: u8) -> VReg {
        VReg::from_real_reg(RealReg(reg), RegType::Int)
    }

    fn reg_info() -> RegisterInfo {
        let mut info = RegisterInfo::default();
        info.allocatable_registers[RegType::Int.index()] = vec![RealReg(1), RealReg(2)];
        info.callee_saved_registers = RegSet::from_regs(&[RealReg(1)]);
        info.caller_saved_registers = RegSet::from_regs(&[RealReg(1), RealReg(2)]);
        info.real_reg_to_vreg = vec![VReg::default(), real_vreg(1), real_vreg(2), real_vreg(3)];
        info.real_reg_name = |reg| reg.to_string();
        info.real_reg_type = |_| RegType::Int;
        info
    }

    #[test]
    fn liveness_analysis_tracks_branch_phi_and_loop_live_ins() {
        let phi = int_vreg(20);

        let b0 = MockBlock::new(0, vec![MockInstr::new().def([int_vreg(1), int_vreg(2)])]).entry();
        let b1 = MockBlock::new(
            1,
            vec![MockInstr::new().def([phi]).use_([int_vreg(1)]).copy()],
        );
        let b2 = MockBlock::new(
            2,
            vec![MockInstr::new().def([phi]).use_([int_vreg(2)]).copy()],
        );
        let b3 = MockBlock::new(3, vec![MockInstr::new().use_([phi])]);
        b3.add_block_param(phi);
        b1.add_pred(b0.clone());
        b2.add_pred(b0.clone());
        b3.add_pred(b1.clone());
        b3.add_pred(b2.clone());

        let loop_phi = int_vreg(30);
        let l0 = MockBlock::new(
            4,
            vec![
                MockInstr::new().def([int_vreg(100)]),
                MockInstr::new()
                    .def([loop_phi])
                    .use_([int_vreg(100)])
                    .copy(),
            ],
        )
        .entry();
        let l1 = MockBlock::new(5, vec![MockInstr::new().use_([loop_phi])]).loop_header(vec![]);
        l1.add_block_param(loop_phi);
        l1.add_pred(l0.clone());
        l1.add_pred(l1.clone());

        let mut func =
            MockFunction::new(vec![b0, b1.clone(), b2.clone(), b3.clone(), l0, l1.clone()]);
        func.lnf_roots = vec![l1.clone()];

        let mut alloc =
            Allocator::<MockInstr, MockBlock, MockFunction>::new(RegisterInfo::default());
        alloc.liveness_analysis(&func);

        let live_ins = |alloc: &Allocator<MockInstr, MockBlock, MockFunction>,
                        block: &MockBlock| {
            alloc
                .block_states
                .get(Allocator::<MockInstr, MockBlock, MockFunction>::block_state_key(block.id()))
                .unwrap()
                .live_ins
                .clone()
        };

        assert_eq!(live_ins(&alloc, &b1), vec![1]);
        assert_eq!(live_ins(&alloc, &b2), vec![2]);
        assert_eq!(live_ins(&alloc, &b3), vec![phi.id()]);
        assert_eq!(live_ins(&alloc, &l1), vec![loop_phi.id()]);
    }

    #[test]
    fn liveness_analysis_ignores_non_allocatable_real_register_uses() {
        let loop_phi = int_vreg(30);
        let reserved = real_vreg(3);

        let entry = MockBlock::new(
            0,
            vec![
                MockInstr::new().use_([reserved]),
                MockInstr::new()
                    .def([loop_phi])
                    .use_([int_vreg(100)])
                    .copy(),
            ],
        )
        .entry();
        let header = MockBlock::new(
            1,
            vec![
                MockInstr::new().use_([reserved]),
                MockInstr::new().use_([loop_phi]),
            ],
        )
        .loop_header(vec![]);
        header.add_block_param(loop_phi);
        header.add_pred(entry.clone());
        header.add_pred(header.clone());

        let mut func = MockFunction::new(vec![entry, header.clone()]);
        func.lnf_roots = vec![header.clone()];

        let mut alloc = Allocator::<MockInstr, MockBlock, MockFunction>::new(reg_info());
        alloc.liveness_analysis(&func);

        let live_ins = alloc
            .block_states
            .get(Allocator::<MockInstr, MockBlock, MockFunction>::block_state_key(header.id()))
            .unwrap()
            .live_ins
            .clone();
        assert_eq!(live_ins, vec![loop_phi.id()]);
    }

    #[test]
    fn find_or_spill_allocatable_prefers_requested_free_register() {
        let mut alloc = Allocator::<MockInstr, MockBlock, MockFunction>::new(reg_info());
        alloc
            .state
            .get_or_allocate_vreg_state(int_vreg(10))
            .last_use = 3;
        alloc.state.use_real_reg(RealReg(1), int_vreg(10).id());

        let got = alloc.find_or_spill_allocatable(
            &[RealReg(1), RealReg(2)],
            RegSet::default(),
            RealReg(2),
        );
        assert_eq!(got, RealReg(2));
    }

    #[test]
    fn do_allocation_assigns_registers_and_reports_clobbers() {
        let i1 = MockInstr::new().def([int_vreg(1)]);
        let i2 = MockInstr::new().use_([int_vreg(1)]).def([int_vreg(2)]);
        let i3 = MockInstr::new().use_([int_vreg(2)]);
        let b0 = MockBlock::new(0, vec![i1.clone(), i2.clone(), i3.clone()]).entry();
        let mut func = MockFunction::new(vec![b0]);

        let mut alloc = Allocator::<MockInstr, MockBlock, MockFunction>::new(reg_info());
        alloc.do_allocation(&mut func);

        let defs = {
            let data = i1.0.borrow();
            data.defs.clone()
        };
        let uses = {
            let data = i2.0.borrow();
            data.uses.clone()
        };
        assert!(defs[0].is_real_reg());
        assert!(uses[0].is_real_reg());
        assert!(func.befores.is_empty());
        assert!(func.afters.is_empty());
        assert_eq!(func.clobbered, vec![real_vreg(1)]);
    }

    #[test]
    fn do_allocation_keeps_single_def_distinct_from_last_use_register() {
        let i1 = MockInstr::new().def([int_vreg(1)]);
        let i2 = MockInstr::new().use_([int_vreg(1)]).def([int_vreg(2)]);
        let i3 = MockInstr::new().use_([int_vreg(2)]);
        let b0 = MockBlock::new(0, vec![i1.clone(), i2.clone(), i3.clone()]).entry();
        let mut func = MockFunction::new(vec![b0]);

        let mut alloc = Allocator::<MockInstr, MockBlock, MockFunction>::new(reg_info());
        alloc.do_allocation(&mut func);

        let i2_data = i2.0.borrow();
        assert!(i2_data.uses[0].is_real_reg());
        assert!(i2_data.defs[0].is_real_reg());
        assert_ne!(i2_data.uses[0].real_reg(), i2_data.defs[0].real_reg());
    }
}
