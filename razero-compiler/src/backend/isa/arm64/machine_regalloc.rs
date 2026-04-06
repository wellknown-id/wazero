use crate::backend::machine::Machine;
use crate::backend::{RegType, VReg};

use super::instr::{Arm64Instr, LoadKind, StoreKind};
use super::lower_mem::resolve_address_mode_for_offset;
use super::machine::Arm64Machine;
use super::reg::{vreg_for_real_reg, SP, TMP};

impl Arm64Machine {
    pub fn ensure_spill_slot(&mut self, vreg: VReg) -> i64 {
        if let Some(offset) = self.spill_slots.get(&vreg.id()) {
            *offset
        } else {
            let offset = self.spill_slot_size;
            self.spill_slot_size += if matches!(vreg.reg_type(), RegType::Float) { 16 } else { 8 };
            self.spill_slots.insert(vreg.id(), offset);
            offset
        }
    }

    pub fn insert_store_register(&mut self, vreg: VReg) {
        let bits = if matches!(vreg.reg_type(), RegType::Float) { 128 } else { 64 };
        let offset = self.ensure_spill_slot(vreg);
        let mem = resolve_address_mode_for_offset(offset, bits, vreg_for_real_reg(SP), vreg_for_real_reg(TMP));
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
        let bits = if matches!(vreg.reg_type(), RegType::Float) { 128 } else { 64 };
        let offset = self.ensure_spill_slot(vreg);
        let mem = resolve_address_mode_for_offset(offset, bits, vreg_for_real_reg(SP), vreg_for_real_reg(TMP));
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
}

#[cfg(test)]
mod tests {
    use crate::backend::Machine;
    use crate::backend::{RegType, VReg};

    use super::Arm64Machine;

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
}
