use crate::backend::VReg;

use super::instr::Amd64Instr;
use super::{AddressMode, Operand};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Addend {
    pub reg: Option<VReg>,
    pub imm: u64,
    pub shift: u8,
}

pub fn as_imm32(val: u64, allow_sign_ext: bool) -> Option<u32> {
    let low = val as u32;
    if u64::from(low) != val {
        return None;
    }
    if !allow_sign_ext && (low & 0x8000_0000) != 0 {
        return None;
    }
    Some(low)
}

pub fn lower_addends_to_amode(
    x: Addend,
    y: Addend,
    offset: u32,
    tmp: VReg,
) -> (Vec<Amd64Instr>, AddressMode) {
    let mut imm = offset as u64 + x.imm + y.imm;
    match (x.reg, y.reg) {
        (Some(base), Some(index)) => (
            vec![],
            AddressMode::reg_reg_shift(imm as u32, base, index, x.shift.max(y.shift)),
        ),
        (Some(base), None) => (vec![], AddressMode::imm_reg(imm as u32, base)),
        (None, Some(base)) => (vec![], AddressMode::imm_reg(imm as u32, base)),
        (None, None) => {
            let load = Amd64Instr::imm(tmp, imm, true);
            imm = 0;
            (vec![load], AddressMode::imm_reg(imm as u32, tmp))
        }
    }
}

pub fn mem_operand_from_base(base: VReg, offset: u32) -> Operand {
    Operand::mem(AddressMode::imm_reg(offset, base))
}

#[cfg(test)]
mod tests {
    use super::{as_imm32, lower_addends_to_amode, Addend};
    use crate::backend::{RegType, VReg};

    #[test]
    fn imm32_checks_follow_go_rules() {
        assert_eq!(as_imm32(123, false), Some(123));
        assert_eq!(as_imm32(0x8000_0000, false), None);
        assert_eq!(as_imm32(0x8000_0000, true), Some(0x8000_0000));
    }

    #[test]
    fn addends_lower_to_scaled_amode() {
        let rax = VReg::from_real_reg(1, RegType::Int);
        let rcx = VReg::from_real_reg(2, RegType::Int);
        let tmp = VReg::from_real_reg(9, RegType::Int);
        let (insts, amode) = lower_addends_to_amode(
            Addend {
                reg: Some(rax),
                imm: 0,
                shift: 0,
            },
            Addend {
                reg: Some(rcx),
                imm: 0,
                shift: 2,
            },
            12,
            tmp,
        );
        assert!(insts.is_empty());
        assert_eq!(amode.to_string(), "12(%rax,%rcx,4)");
    }
}
