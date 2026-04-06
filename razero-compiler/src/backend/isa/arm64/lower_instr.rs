use crate::backend::VReg;
use crate::ssa::{Instruction, Opcode, Type};

use super::cond::{Cond, CondFlag};
use super::instr::{AluOp, Arm64Instr};

pub fn lower_branch_condition(opcode: Opcode, cond_reg: VReg) -> Arm64Instr {
    let cond = match opcode {
        Opcode::Brz => Cond::from_reg_zero(cond_reg),
        Opcode::Brnz => Cond::from_reg_not_zero(cond_reg),
        _ => Cond::from_flag(CondFlag::Nv),
    };
    Arm64Instr::CondBr {
        cond,
        offset: 0,
        bits64: true,
    }
}

pub fn lower_binary_arith(
    opcode: Opcode,
    ty: Type,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
) -> Option<Arm64Instr> {
    let op = match opcode {
        Opcode::Iadd => AluOp::Add,
        Opcode::Isub => AluOp::Sub,
        Opcode::Imul => AluOp::Mul,
        _ => return None,
    };
    Some(Arm64Instr::AluRRR {
        op,
        rd: dst,
        rn: lhs,
        rm: rhs,
        bits: ty.bits(),
        set_flags: false,
    })
}

pub fn supported_lowering_opcode(instruction: &Instruction) -> bool {
    matches!(
        instruction.opcode,
        Opcode::Iconst
            | Opcode::F32const
            | Opcode::F64const
            | Opcode::Vconst
            | Opcode::Iadd
            | Opcode::Isub
            | Opcode::Imul
            | Opcode::Brz
            | Opcode::Brnz
            | Opcode::Jump
            | Opcode::Return
            | Opcode::Call
            | Opcode::CallIndirect
    )
}

#[cfg(test)]
mod tests {
    use super::{lower_binary_arith, lower_branch_condition, supported_lowering_opcode};
    use crate::backend::{RegType, VReg};
    use crate::ssa::{Instruction, Opcode, Type};

    #[test]
    fn conditional_branch_lowering_uses_cbz_cbnz() {
        let cond = VReg(128).set_reg_type(RegType::Int);
        assert_eq!(
            lower_branch_condition(Opcode::Brz, cond).to_string(),
            "cbz x128?, #0x0"
        );
        assert_eq!(
            lower_branch_condition(Opcode::Brnz, cond).to_string(),
            "cbnz x128?, #0x0"
        );
    }

    #[test]
    fn binary_arith_lowering_emits_alu_instruction() {
        let x0 = VReg(128).set_reg_type(RegType::Int);
        let x1 = VReg(129).set_reg_type(RegType::Int);
        let x2 = VReg(130).set_reg_type(RegType::Int);
        assert_eq!(
            lower_binary_arith(Opcode::Iadd, Type::I64, x0, x1, x2)
                .unwrap()
                .to_string(),
            "add x128?, x129?, x130?"
        );
    }

    #[test]
    fn supported_opcode_list_is_explicit() {
        let instr = Instruction::new().with_opcode(Opcode::Iadd);
        assert!(supported_lowering_opcode(&instr));
        let unsupported = Instruction::new().with_opcode(Opcode::Band);
        assert!(!supported_lowering_opcode(&unsupported));
    }
}
