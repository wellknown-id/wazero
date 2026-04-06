use crate::backend::{RegType, VReg};

use super::instr::{AluOp, Arm64Instr};
use super::lower_constant::lower_constant_u64;
use super::lower_instr_operands::as_imm12;
use super::machine::Arm64Machine;
use super::reg::{vreg_for_real_reg, SP, TMP};

impl Arm64Machine {
    pub fn clobbered_reg_slot_size(&self) -> i64 {
        (self.clobbered_regs.len() as i64) * 16
    }

    pub fn frame_size(&self) -> i64 {
        self.spill_slot_size + self.clobbered_reg_slot_size()
    }

    pub fn required_stack_size(&self) -> i64 {
        self.frame_size() + self.max_required_stack_size_for_calls + 32
    }

    pub fn arg0_offset_from_sp(&self) -> i64 {
        self.frame_size() + 16
    }

    pub fn ret0_offset_from_sp(&self) -> i64 {
        self.arg0_offset_from_sp() + self.current_abi.arg_stack_size
    }

    pub fn get_vreg_spill_slot_offset_from_sp(&self, vreg: VReg) -> Option<i64> {
        self.spill_slots.get(&vreg.id()).copied()
    }

    pub fn insert_add_or_sub_stack_pointer(&mut self, amount: i64, add: bool) {
        let sp = vreg_for_real_reg(SP);
        if let Some(imm12) = as_imm12(amount as u64) {
            self.push(Arm64Instr::AluRRImm12 {
                op: if add { AluOp::Add } else { AluOp::Sub },
                rd: sp,
                rn: sp,
                imm: imm12,
                bits: 64,
                set_flags: false,
            });
        } else {
            let tmp = VReg::from_real_reg(TMP, RegType::Int);
            self.pending_instructions
                .extend(lower_constant_u64(tmp, amount as u64));
            self.push(Arm64Instr::AluRRR {
                op: if add { AluOp::Add } else { AluOp::Sub },
                rd: sp,
                rn: sp,
                rm: tmp,
                bits: 64,
                set_flags: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::backend::Machine;

    use super::Arm64Machine;

    #[test]
    fn insert_add_or_sub_stack_pointer_matches_go_shape() {
        let mut machine = Arm64Machine::new();
        machine.insert_add_or_sub_stack_pointer(0x10, true);
        machine.flush_pending_instructions();
        assert_eq!(machine.format(), "add sp, sp, #0x10");

        let mut machine = Arm64Machine::new();
        machine.insert_add_or_sub_stack_pointer(0xffff_ffff8, false);
        machine.flush_pending_instructions();
        assert!(machine.format().contains("movz x27"));
        assert!(machine.format().contains("sub sp, sp, x27"));
    }
}
