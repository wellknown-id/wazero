use crate::ssa::Signature;

use super::abi::Arm64Abi;
use super::instr::{Arm64Instr, LoadKind, StoreKind};
use super::instr_encoding::encode_instruction;
use super::lower_mem::AddressMode;
use super::reg::{vreg_for_real_reg, SP, X0, X19, X20, X24, X26};

pub fn compile_entry_preamble(signature: &Signature, use_host_stack: bool) -> Vec<u8> {
    let abi = Arm64Abi::from_signature(signature);
    let mut instructions = Vec::new();
    let exec_ctx = vreg_for_real_reg(X0);
    let saved_exec_ctx = vreg_for_real_reg(X20);
    instructions.push(Arm64Instr::Move {
        rd: saved_exec_ctx,
        rn: exec_ctx,
        bits: 64,
    });
    if !use_host_stack {
        instructions.push(Arm64Instr::Move {
            rd: vreg_for_real_reg(SP),
            rn: vreg_for_real_reg(X26),
            bits: 64,
        });
    }

    let pr = vreg_for_real_reg(X19);
    for arg in abi.function.args.iter().skip(2) {
        if matches!(arg.kind, crate::backend::AbiArgKind::Reg) {
            instructions.push(Arm64Instr::Load {
                kind: if arg.ty.is_int() { LoadKind::ULoad } else { LoadKind::FpuLoad },
                rd: arg.reg,
                mem: AddressMode::reg_unsigned_imm12(pr, 0),
                bits: arg.ty.bits(),
            });
        }
    }

    instructions.push(Arm64Instr::BrReg {
        rn: vreg_for_real_reg(X24),
        link: true,
    });

    for ret in &abi.function.rets {
        instructions.push(Arm64Instr::Store {
            kind: if ret.ty.is_int() { StoreKind::Store } else { StoreKind::FpuStore },
            src: ret.reg,
            mem: AddressMode::reg_unsigned_imm12(pr, 0),
            bits: ret.ty.bits(),
        });
    }
    instructions.push(Arm64Instr::Ret);

    let mut buf = Vec::new();
    for instr in instructions {
        if let Ok(words) = encode_instruction(&instr) {
            for word in words {
                buf.extend_from_slice(&word.to_le_bytes());
            }
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::compile_entry_preamble;
    use crate::ssa::{Signature, SignatureId, Type};

    #[test]
    fn entry_preamble_scaffold_is_non_empty() {
        let sig = Signature::new(
            SignatureId(0),
            vec![Type::I64, Type::I64, Type::F64],
            vec![Type::I64],
        );
        assert!(!compile_entry_preamble(&sig, false).is_empty());
    }
}
