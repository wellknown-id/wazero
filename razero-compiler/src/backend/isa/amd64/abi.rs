use crate::backend::regalloc::{RegSet, RegisterInfo};
use crate::backend::{FunctionAbi, VReg};
use crate::ssa::Signature;

use super::instr::Amd64Instr;
use super::machine_vec::SseOpcode;
use super::operands::{AddressMode, Operand};
use super::reg::{
    real_reg_name, R10, R11, R12, R13, R14, R15, R8, R9, RAX, RBX, RCX, RDI, RDX, RSI, XMM0, XMM1,
    XMM10, XMM11, XMM12, XMM13, XMM14, XMM15, XMM2, XMM3, XMM4, XMM5, XMM6, XMM7, XMM8, XMM9,
};

pub use super::reg::{FLOAT_ARG_RESULT_REGS, INT_ARG_RESULT_REGS};

pub fn amd64_function_abi(sig: &Signature) -> FunctionAbi {
    let mut abi = FunctionAbi::default();
    abi.init(sig, &INT_ARG_RESULT_REGS, &FLOAT_ARG_RESULT_REGS);
    abi
}

pub fn amd64_register_info() -> RegisterInfo {
    fn rr(raw: u8) -> crate::backend::regalloc::RealReg {
        crate::backend::regalloc::RealReg(raw)
    }
    fn vr(raw: u8, reg_type: crate::backend::regalloc::RegType) -> crate::backend::regalloc::VReg {
        crate::backend::regalloc::VReg::from_real_reg(rr(raw), reg_type)
    }
    let mut real_reg_to_vreg = vec![crate::backend::regalloc::VReg::default(); XMM15 as usize + 1];
    for raw in RAX..=R15 {
        real_reg_to_vreg[raw as usize] = vr(raw, crate::backend::regalloc::RegType::Int);
    }
    for raw in XMM0..=XMM15 {
        real_reg_to_vreg[raw as usize] = vr(raw, crate::backend::regalloc::RegType::Float);
    }
    RegisterInfo {
        allocatable_registers: [
            vec![
                rr(RAX),
                rr(RCX),
                rr(RDX),
                rr(RBX),
                rr(RSI),
                rr(RDI),
                rr(R8),
                rr(R9),
                rr(R10),
                rr(R11),
                rr(R12),
                rr(R13),
                rr(R14),
            ],
            vec![
                rr(XMM0),
                rr(XMM1),
                rr(XMM2),
                rr(XMM3),
                rr(XMM4),
                rr(XMM5),
                rr(XMM6),
                rr(XMM7),
                rr(XMM8),
                rr(XMM9),
                rr(XMM10),
                rr(XMM11),
                rr(XMM12),
                rr(XMM13),
                rr(XMM14),
                rr(XMM15),
            ],
            vec![],
        ],
        callee_saved_registers: RegSet::from_regs(&[
            rr(RDX),
            rr(R12),
            rr(R13),
            rr(R14),
            rr(R15),
            rr(XMM8),
            rr(XMM9),
            rr(XMM10),
            rr(XMM11),
            rr(XMM12),
            rr(XMM13),
            rr(XMM14),
            rr(XMM15),
        ]),
        caller_saved_registers: RegSet::from_regs(&[
            rr(RAX),
            rr(RCX),
            rr(RBX),
            rr(RSI),
            rr(RDI),
            rr(R8),
            rr(R9),
            rr(R10),
            rr(R11),
            rr(XMM0),
            rr(XMM1),
            rr(XMM2),
            rr(XMM3),
            rr(XMM4),
            rr(XMM5),
            rr(XMM6),
            rr(XMM7),
        ]),
        real_reg_to_vreg,
        real_reg_name: |r| real_reg_name(r.0),
        real_reg_type: |r| {
            if r.0 >= XMM0 {
                crate::backend::regalloc::RegType::Float
            } else {
                crate::backend::regalloc::RegType::Int
            }
        },
    }
}

pub fn lower_abi_params(abi: &FunctionAbi, params: &[VReg]) -> Vec<Amd64Instr> {
    let mut out = Vec::new();
    for (ssa_reg, arg) in params.iter().zip(&abi.args) {
        if arg.kind == crate::backend::AbiArgKind::Reg {
            out.push(Amd64Instr::mov_rr(arg.reg, *ssa_reg, arg.ty.is_int()));
        } else if arg.ty.is_int() {
            out.push(Amd64Instr::mov64_mr(
                Operand::mem(AddressMode::imm_rbp((arg.offset + 16) as u32)),
                *ssa_reg,
            ));
        } else {
            out.push(Amd64Instr::xmm_unary_rm_r(
                match arg.ty {
                    crate::ssa::Type::F32 => SseOpcode::Movss,
                    crate::ssa::Type::F64 => SseOpcode::Movsd,
                    _ => SseOpcode::Movdqu,
                },
                Operand::mem(AddressMode::imm_rbp((arg.offset + 16) as u32)),
                *ssa_reg,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{amd64_function_abi, amd64_register_info};
    use crate::ssa::{Signature, SignatureId, Type};

    #[test]
    fn abi_assigns_go_ordered_registers() {
        let sig = Signature::new(
            SignatureId(0),
            vec![Type::I64, Type::F64, Type::I32, Type::V128, Type::I64],
            vec![Type::I64, Type::F64],
        );
        let abi = amd64_function_abi(&sig);
        assert_eq!(abi.args[0].reg.real_reg(), 1);
        assert_eq!(abi.args[1].reg.real_reg(), 17);
        assert_eq!(abi.args[4].reg.real_reg(), 2);
    }

    #[test]
    fn regalloc_info_matches_saved_sets() {
        let info = amd64_register_info();
        assert!(info
            .callee_saved_registers
            .has(crate::backend::regalloc::RealReg(16)));
        assert!(info
            .caller_saved_registers
            .has(crate::backend::regalloc::RealReg(1)));
    }
}
