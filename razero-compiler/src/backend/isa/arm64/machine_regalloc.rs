use std::collections::{BTreeMap, BTreeSet};

use crate::backend::machine::Machine;
use crate::backend::{RegType, VReg};

use super::cond::{Cond, CondKind};
use super::instr::{Arm64Instr, LoadKind, StoreKind};
use super::lower_mem::{resolve_address_mode_for_offset, AddressMode, AddressModeKind};
use super::machine::Arm64Machine;
use super::reg::{
    vreg_for_real_reg, SP, TMP, V0, V1, V10, V11, V12, V13, V14, V15, V16, V17, V2, V3, V4, V5, V6,
    V7, V8, V9, X0, X1, X10, X11, X12, X13, X14, X15, X16, X17, X2, X3, X4, X5, X6, X7, X8, X9,
};

const EXEC_ALLOCATABLE_INT_REGS: &[u8] = &[
    X8, X9, X10, X11, X12, X13, X14, X15, X16, X17, X7, X6, X5, X4, X3, X2, X1, X0,
];
const EXEC_ALLOCATABLE_FLOAT_REGS: &[u8] = &[
    V8, V9, V10, V11, V12, V13, V14, V15, V16, V17, V7, V6, V5, V4, V3, V2, V1, V0,
];

impl Arm64Machine {
    pub fn ensure_spill_slot(&mut self, vreg: VReg) -> i64 {
        if let Some(offset) = self.spill_slots.get(&vreg.id()) {
            *offset
        } else {
            let offset = self.spill_slot_size;
            self.spill_slot_size += if matches!(vreg.reg_type(), RegType::Float) {
                16
            } else {
                8
            };
            self.spill_slots.insert(vreg.id(), offset);
            offset
        }
    }

    pub fn insert_store_register(&mut self, vreg: VReg) {
        let bits = if matches!(vreg.reg_type(), RegType::Float) {
            128
        } else {
            64
        };
        let offset = self.ensure_spill_slot(vreg);
        let mem = resolve_address_mode_for_offset(
            offset,
            bits,
            vreg_for_real_reg(SP),
            vreg_for_real_reg(TMP),
        );
        self.push(Arm64Instr::Store {
            kind: if matches!(vreg.reg_type(), RegType::Float) {
                StoreKind::FpuStore
            } else {
                StoreKind::Store
            },
            src: vreg,
            mem,
            bits,
        });
    }

    pub fn insert_reload_register(&mut self, vreg: VReg) {
        let bits = if matches!(vreg.reg_type(), RegType::Float) {
            128
        } else {
            64
        };
        let offset = self.ensure_spill_slot(vreg);
        let mem = resolve_address_mode_for_offset(
            offset,
            bits,
            vreg_for_real_reg(SP),
            vreg_for_real_reg(TMP),
        );
        self.push(Arm64Instr::Load {
            kind: if matches!(vreg.reg_type(), RegType::Float) {
                LoadKind::FpuLoad
            } else {
                LoadKind::ULoad
            },
            rd: vreg,
            mem,
            bits,
        });
    }

    pub fn swap(&mut self, x1: VReg, x2: VReg, tmp: Option<VReg>) {
        let ty = if x1.reg_type() == RegType::Int {
            crate::ssa::Type::I64
        } else {
            crate::ssa::Type::V128
        };
        let tmp = tmp.unwrap_or_else(|| VReg::from_real_reg(TMP, x1.reg_type()));
        self.insert_move(tmp, x1, ty);
        self.insert_move(x1, x2, ty);
        self.insert_move(x2, tmp, ty);
    }

    pub fn perform_reg_alloc(&mut self) {
        self.flush_pending_instructions();

        let mut last_use = BTreeMap::<u32, usize>::new();
        for (index, instr) in self.instructions.iter().enumerate() {
            for reg in instr.uses_vec().into_iter().chain(instr.defs_vec()) {
                if !reg.is_real_reg() && reg.valid() {
                    last_use.insert(reg.id(), index);
                }
            }
        }

        let mut map = BTreeMap::<u32, VReg>::new();
        let mut free_ints: Vec<_> = EXEC_ALLOCATABLE_INT_REGS
            .iter()
            .rev()
            .copied()
            .map(|raw| VReg::from_real_reg(raw, RegType::Int))
            .collect();
        let mut free_floats: Vec<_> = EXEC_ALLOCATABLE_FLOAT_REGS
            .iter()
            .rev()
            .copied()
            .map(|raw| VReg::from_real_reg(raw, RegType::Float))
            .collect();

        for (index, instr) in self.instructions.iter_mut().enumerate() {
            let mentioned: Vec<VReg> = instr
                .uses_vec()
                .into_iter()
                .chain(instr.defs_vec())
                .collect();
            for reg in &mentioned {
                if reg.is_real_reg() || !reg.valid() || map.contains_key(&reg.id()) {
                    continue;
                }
                let allocated = match reg.reg_type() {
                    RegType::Int => free_ints.pop(),
                    RegType::Float => free_floats.pop(),
                    RegType::Invalid => None,
                };
                if let Some(real) = allocated {
                    map.insert(reg.id(), real);
                }
            }

            rewrite_instr(instr, &map);

            let mut released = BTreeSet::new();
            for reg in mentioned {
                if reg.is_real_reg() || !reg.valid() {
                    continue;
                }
                if last_use.get(&reg.id()) == Some(&index) && released.insert(reg.id()) {
                    if let Some(real) = map.remove(&reg.id()) {
                        match real.reg_type() {
                            RegType::Int => free_ints.push(real),
                            RegType::Float => free_floats.push(real),
                            RegType::Invalid => {}
                        }
                    }
                }
            }
        }
    }

    pub fn finalize_post_reg_alloc(&mut self) {
        let arg0 = self.arg0_offset_from_sp();
        let ret0 = self.ret0_offset_from_sp();
        let mut rewritten = Vec::with_capacity(self.instructions.len());
        for mut instr in self.instructions.drain(..) {
            match &mut instr {
                Arm64Instr::Move { rd, rn, .. } | Arm64Instr::FpuMove { rd, rn, .. }
                    if rd == rn =>
                {
                    continue
                }
                Arm64Instr::Load { mem, bits, .. } | Arm64Instr::Store { mem, bits, .. } => {
                    match mem.kind {
                        AddressModeKind::ArgStackSpace => {
                            *mem = resolve_address_mode_for_offset(
                                arg0 + mem.imm,
                                *bits,
                                mem.rn,
                                vreg_for_real_reg(TMP),
                            );
                        }
                        AddressModeKind::ResultStackSpace => {
                            *mem = resolve_address_mode_for_offset(
                                ret0 + mem.imm,
                                *bits,
                                mem.rn,
                                vreg_for_real_reg(TMP),
                            );
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            rewritten.push(instr);
        }
        self.instructions = rewritten;
    }
}

fn rewrite_instr(instr: &mut Arm64Instr, map: &BTreeMap<u32, VReg>) {
    match instr {
        Arm64Instr::Adr { rd, .. }
        | Arm64Instr::MovZ { rd, .. }
        | Arm64Instr::MovK { rd, .. }
        | Arm64Instr::MovN { rd, .. }
        | Arm64Instr::CSet { rd, .. }
        | Arm64Instr::LoadConstBlockArg { dst: rd, .. } => rewrite_reg(rd, map),
        Arm64Instr::Move { rd, rn, .. } | Arm64Instr::FpuMove { rd, rn, .. } => {
            rewrite_reg(rd, map);
            rewrite_reg(rn, map);
        }
        Arm64Instr::AluRRR { rd, rn, rm, .. } => {
            rewrite_reg(rd, map);
            rewrite_reg(rn, map);
            rewrite_reg(rm, map);
        }
        Arm64Instr::AluRRImm12 { rd, rn, .. } => {
            rewrite_reg(rd, map);
            rewrite_reg(rn, map);
        }
        Arm64Instr::Cmp { rn, rm, .. } => {
            rewrite_reg(rn, map);
            rewrite_reg(rm, map);
        }
        Arm64Instr::Load { rd, mem, .. } => {
            rewrite_reg(rd, map);
            rewrite_mem(mem, map);
        }
        Arm64Instr::Store { src, mem, .. } => {
            rewrite_reg(src, map);
            rewrite_mem(mem, map);
        }
        Arm64Instr::BrReg { rn, .. } | Arm64Instr::CallReg { rn, .. } => rewrite_reg(rn, map),
        Arm64Instr::CondBr { cond, .. } => {
            *cond = match cond.kind() {
                CondKind::RegisterZero => {
                    Cond::from_reg_zero(rewrite_cond_reg(cond.register(), map))
                }
                CondKind::RegisterNotZero => {
                    Cond::from_reg_not_zero(rewrite_cond_reg(cond.register(), map))
                }
                CondKind::CondFlagSet => *cond,
            };
        }
        Arm64Instr::Nop
        | Arm64Instr::Label(_)
        | Arm64Instr::Br { .. }
        | Arm64Instr::Call { .. }
        | Arm64Instr::Ret
        | Arm64Instr::Udf { .. }
        | Arm64Instr::Raw32(_) => {}
    }
}

fn rewrite_cond_reg(reg: VReg, map: &BTreeMap<u32, VReg>) -> VReg {
    if reg.is_real_reg() {
        reg
    } else {
        map.get(&reg.id()).copied().unwrap_or(reg)
    }
}

fn rewrite_reg(reg: &mut VReg, map: &BTreeMap<u32, VReg>) {
    if !reg.is_real_reg() {
        if let Some(mapped) = map.get(&reg.id()) {
            *reg = *mapped;
        }
    }
}

fn rewrite_mem(mem: &mut AddressMode, map: &BTreeMap<u32, VReg>) {
    rewrite_reg(&mut mem.rn, map);
    rewrite_reg(&mut mem.rm, map);
}

#[cfg(test)]
mod tests {
    use crate::backend::Machine;
    use crate::backend::{RegType, VReg};

    use super::Arm64Machine;
    use crate::backend::isa::arm64::instr::{AluOp, Arm64Instr, LoadKind};
    use crate::backend::isa::arm64::lower_instr_operands::ExtendOp;
    use crate::backend::isa::arm64::lower_mem::{AddressMode, AddressModeKind};
    use crate::backend::isa::arm64::reg::{SP, X0, X1};

    #[test]
    fn spill_and_reload_allocate_slots() {
        let mut machine = Arm64Machine::new();
        let reg = VReg(128).set_reg_type(RegType::Int);
        machine.insert_store_register(reg);
        machine.insert_reload_register(reg);
        machine.flush_pending_instructions();
        assert_eq!(machine.spill_slots[&128], 0);
        assert!(machine.format().contains("str x128?"));
        assert!(machine.format().contains("ldr x128?"));
    }

    #[test]
    fn swap_emits_three_moves() {
        let mut machine = Arm64Machine::new();
        let x1 = VReg(128).set_reg_type(RegType::Int);
        let x2 = VReg(129).set_reg_type(RegType::Int);
        machine.swap(x1, x2, None);
        machine.flush_pending_instructions();
        assert_eq!(machine.instructions().len(), 3);
    }

    #[test]
    fn regalloc_rewrites_virtual_registers() {
        let mut machine = Arm64Machine::new();
        let dst = VReg(128).set_reg_type(RegType::Int);
        let src = VReg(129).set_reg_type(RegType::Int);
        machine.push(Arm64Instr::Move {
            rd: dst,
            rn: VReg::from_real_reg(X0, RegType::Int),
            bits: 64,
        });
        machine.push(Arm64Instr::AluRRR {
            op: AluOp::Add,
            rd: src,
            rn: dst,
            rm: VReg::from_real_reg(X1, RegType::Int),
            bits: 64,
            set_flags: false,
        });
        machine.flush_pending_instructions();
        machine.perform_reg_alloc();
        assert!(!machine.format().contains('?'));
    }

    #[test]
    fn post_regalloc_resolves_stack_space_modes() {
        let mut machine = Arm64Machine::new();
        machine.current_abi.arg_stack_size = 16;
        machine.current_abi.ret_stack_size = 8;
        machine.push(Arm64Instr::Load {
            kind: LoadKind::ULoad,
            rd: VReg(128).set_reg_type(RegType::Int),
            mem: AddressMode {
                kind: AddressModeKind::ArgStackSpace,
                rn: VReg::from_real_reg(SP, RegType::Int),
                rm: VReg::INVALID,
                ext_op: ExtendOp::Uxtx,
                imm: 0,
            },
            bits: 64,
        });
        machine.flush_pending_instructions();
        machine.finalize_post_reg_alloc();
        match &machine.instructions()[0] {
            Arm64Instr::Load { mem, .. } => assert_ne!(mem.kind, AddressModeKind::ArgStackSpace),
            _ => panic!("expected load"),
        }
    }
}
