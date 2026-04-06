use crate::backend::VReg;
use crate::ssa::Type;

use super::instr::{AluRmiROpcode, Amd64Instr};
use super::reg::vreg_for_real_reg;
use super::SseOpcode;
use super::{Operand, RAX};

pub fn lower_constant(dst: VReg, ty: Type, value_bits: u64) -> Vec<Amd64Instr> {
    match ty {
        Type::I32 => {
            if value_bits as u32 == 0 {
                vec![Amd64Instr::alu_rmi_r(
                    AluRmiROpcode::Xor,
                    Operand::reg(dst),
                    dst,
                    false,
                )]
            } else {
                vec![Amd64Instr::imm(dst, value_bits, false)]
            }
        }
        Type::I64 => {
            if value_bits == 0 {
                vec![Amd64Instr::alu_rmi_r(
                    AluRmiROpcode::Xor,
                    Operand::reg(dst),
                    dst,
                    true,
                )]
            } else {
                vec![Amd64Instr::imm(dst, value_bits, true)]
            }
        }
        Type::F32 | Type::F64 | Type::V128 => {
            let tmp = vreg_for_real_reg(RAX);
            vec![
                Amd64Instr::imm(tmp, value_bits, true),
                Amd64Instr::xmm_load_const(dst, value_bits),
                Amd64Instr::xmm_unary_rm_r(SseOpcode::Movdqu, Operand::reg(tmp), dst),
            ]
        }
        Type::Invalid => panic!("invalid constant type"),
    }
}

#[cfg(test)]
mod tests {
    use super::lower_constant;
    use crate::backend::{RegType, VReg};
    use crate::ssa::Type;

    #[test]
    fn integer_zero_prefers_xor() {
        let dst = VReg::from_real_reg(1, RegType::Int);
        let lowered = lower_constant(dst, Type::I64, 0);
        assert_eq!(lowered.len(), 1);
        assert_eq!(lowered[0].to_string(), "xor %rax, %rax");
    }
}
