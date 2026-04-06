use crate::backend::VReg;
use crate::ssa::Type;

use super::instr::Arm64Instr;

pub fn lower_constant_u64(dst: VReg, value: u64) -> Vec<Arm64Instr> {
    let mut seq = Vec::new();
    if value == 0 {
        seq.push(Arm64Instr::MovZ {
            rd: dst,
            imm: 0,
            shift: 0,
            bits: 64,
        });
        return seq;
    }

    let chunks = [
        (value & 0xffff) as u16,
        ((value >> 16) & 0xffff) as u16,
        ((value >> 32) & 0xffff) as u16,
        ((value >> 48) & 0xffff) as u16,
    ];
    let mut first = true;
    for (index, imm) in chunks.into_iter().enumerate() {
        if imm == 0 {
            continue;
        }
        let shift = (index * 16) as u8;
        if first {
            seq.push(Arm64Instr::MovZ {
                rd: dst,
                imm,
                shift,
                bits: 64,
            });
            first = false;
        } else {
            seq.push(Arm64Instr::MovK {
                rd: dst,
                imm,
                shift,
                bits: 64,
            });
        }
    }
    seq
}

pub fn lower_constant(dst: VReg, ty: Type, lo: u64, hi: u64) -> Vec<Arm64Instr> {
    match ty {
        Type::I32 => lower_constant_u64(dst, lo as u32 as u64),
        Type::I64 | Type::F32 | Type::F64 => lower_constant_u64(dst, lo),
        Type::V128 => vec![
            Arm64Instr::MovZ {
                rd: dst,
                imm: (lo & 0xffff) as u16,
                shift: 0,
                bits: 64,
            },
            Arm64Instr::MovK {
                rd: dst,
                imm: ((lo >> 16) & 0xffff) as u16,
                shift: 16,
                bits: 64,
            },
            Arm64Instr::MovK {
                rd: dst,
                imm: (hi & 0xffff) as u16,
                shift: 32,
                bits: 64,
            },
        ],
        Type::Invalid => panic!("invalid constant type"),
    }
}

#[cfg(test)]
mod tests {
    use super::{lower_constant, lower_constant_u64};
    use crate::backend::{RegType, VReg};
    use crate::ssa::Type;

    #[test]
    fn integer_constant_lowering_uses_movz_movk_sequences() {
        let dst = VReg(128).set_reg_type(RegType::Int);
        let seq = lower_constant_u64(dst, 0x1234_5678_90ab_cdef);
        assert_eq!(seq.len(), 4);
        assert_eq!(seq[0].to_string(), "movz x128?, #0xcdef, lsl 0");
        assert_eq!(seq[3].to_string(), "movk x128?, #0x1234, lsl 48");
    }

    #[test]
    fn zero_constant_uses_single_movz() {
        let dst = VReg(128).set_reg_type(RegType::Int);
        let seq = lower_constant_u64(dst, 0);
        assert_eq!(seq.len(), 1);
        assert_eq!(seq[0].to_string(), "movz x128?, #0x0, lsl 0");
    }

    #[test]
    fn type_driven_lowering_preserves_scalar_shapes() {
        let dst = VReg(128).set_reg_type(RegType::Int);
        assert_eq!(lower_constant(dst, Type::I32, 10, 0).len(), 1);
        assert_eq!(lower_constant(dst, Type::F64, 0x3ff0_0000_0000_0000, 0).len(), 1);
    }
}
