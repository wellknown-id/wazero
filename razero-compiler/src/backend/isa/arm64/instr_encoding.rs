use crate::backend::machine::BackendError;

use super::cond::{Cond, CondFlag, CondKind};
use super::instr::{AluOp, Arm64Instr, LoadKind, StoreKind};
use super::lower_mem::{AddressMode, AddressModeKind};
use super::reg::encoding_reg_number;

pub const MAX_SIGNED_INT26: i64 = (1 << 25) - 1;
pub const MIN_SIGNED_INT26: i64 = -(1 << 25);
pub const DUMMY_INSTRUCTION: u32 = 0;

fn encode_bits_64(bits: u8) -> u32 {
    u32::from(bits == 64)
}

pub fn encode_unconditional_branch(link: bool, offset: i64) -> u32 {
    if offset == 0 {
        return DUMMY_INSTRUCTION;
    }
    let imm26 = ((offset >> 2) as i32 as u32) & 0x03ff_ffff;
    ((if link { 0b100101 } else { 0b000101 }) << 26) | imm26
}

pub fn encode_unconditional_branch_reg(reg_num: u32, link: bool) -> u32 {
    let op = if link { 0xd63f_0000 } else { 0xd61f_0000 };
    op | (reg_num << 5)
}

pub fn encode_adr(rd: u32, offset: i32) -> u32 {
    let imm = offset as u32;
    let immlo = imm & 0b11;
    let immhi = (imm >> 2) & 0x7ffff;
    0x1000_0000 | (immlo << 29) | (immhi << 5) | rd
}

pub fn encode_mov_wide(opc: u32, bits: u8, rd: u32, imm: u16, shift: u8) -> u32 {
    let hw = ((shift / 16) as u32) & 0b11;
    (encode_bits_64(bits) << 31) | (opc << 29) | 0x1280_0000 | (hw << 21) | ((imm as u32) << 5) | rd
}

pub fn encode_movz(bits: u8, rd: u32, imm: u16, shift: u8) -> u32 {
    encode_mov_wide(0b10, bits, rd, imm, shift)
}

pub fn encode_movk(bits: u8, rd: u32, imm: u16, shift: u8) -> u32 {
    encode_mov_wide(0b11, bits, rd, imm, shift)
}

pub fn encode_movn(bits: u8, rd: u32, imm: u16, shift: u8) -> u32 {
    encode_mov_wide(0b00, bits, rd, imm, shift)
}

pub fn encode_alu_rrr(op: AluOp, rd: u32, rn: u32, rm: u32, bits: u8, set_flags: bool) -> u32 {
    let sf = encode_bits_64(bits) << 31;
    match op {
        AluOp::Add => sf | ((set_flags as u32) << 29) | 0x0b00_0000 | (rm << 16) | (rn << 5) | rd,
        AluOp::Sub => sf | (0b10 << 29) | ((set_flags as u32) << 29) | 0x4b00_0000 | (rm << 16) | (rn << 5) | rd,
        AluOp::Orr => sf | 0x2a00_0000 | (rm << 16) | (rn << 5) | rd,
        AluOp::And => sf | 0x0a00_0000 | (rm << 16) | (rn << 5) | rd,
        AluOp::Eor => sf | 0x4a00_0000 | (rm << 16) | (rn << 5) | rd,
        AluOp::Mul => sf | 0x1b00_7c00 | (rm << 16) | (rn << 5) | rd,
    }
}

pub fn encode_alu_rr_imm12(op: AluOp, rd: u32, rn: u32, imm12: u32, bits: u8, set_flags: bool) -> u32 {
    let sf = encode_bits_64(bits) << 31;
    let shift = ((imm12 >> 12) & 1) << 22;
    let imm = (imm12 & 0xfff) << 10;
    match op {
        AluOp::Add => sf | ((set_flags as u32) << 29) | 0x1100_0000 | shift | imm | (rn << 5) | rd,
        AluOp::Sub => sf | (0b10 << 29) | ((set_flags as u32) << 29) | 0x5100_0000 | shift | imm | (rn << 5) | rd,
        _ => panic!("immediate encoding only implemented for add/sub"),
    }
}

pub fn encode_cset(rd: u32, cond: CondFlag) -> u32 {
    0x9a9f_07e0 | (((cond.invert() as u32) & 0xf) << 12) | rd
}

pub fn encode_cond_br(cond: Cond, offset: i64, bits64: bool) -> u32 {
    match cond.kind() {
        CondKind::RegisterZero | CondKind::RegisterNotZero => {
            let imm19 = (((offset >> 2) as i32 as u32) & 0x7ffff) << 5;
            let rt = encoding_reg_number(cond.register().real_reg());
            let base = match (cond.kind(), bits64) {
                (CondKind::RegisterZero, false) => 0x3400_0000,
                (CondKind::RegisterZero, true) => 0xb400_0000,
                (CondKind::RegisterNotZero, false) => 0x3500_0000,
                (CondKind::RegisterNotZero, true) => 0xb500_0000,
                _ => unreachable!(),
            };
            base | imm19 | rt
        }
        CondKind::CondFlagSet => {
            let imm19 = (((offset >> 2) as i32 as u32) & 0x7ffff) << 5;
            0x5400_0000 | imm19 | (cond.flag() as u32)
        }
    }
}

pub fn encode_load_or_store(kind_load: bool, bits: u8, reg: u32, mem: AddressMode, signed: bool, fp: bool) -> u32 {
    match mem.kind {
        AddressModeKind::RegUnsignedImm12 => {
            let size_field = match bits {
                8 => 0,
                16 => 1,
                32 => 2,
                64 | 128 => 3,
                _ => panic!("unsupported arm64 memory width"),
            } << 30;
            let scale = (bits / 8) as i64;
            let imm12 = ((mem.imm / scale) as u32 & 0xfff) << 10;
            let rn = encoding_reg_number(mem.rn.real_reg()) << 5;
            let base = if fp {
                if kind_load { 0x3d40_0000 } else { 0x3d00_0000 }
            } else if signed && bits == 32 {
                0xb980_0000
            } else if kind_load {
                0x3940_0000
            } else {
                0x3900_0000
            };
            size_field | base | imm12 | rn | reg
        }
        _ => panic!("addressing mode {:?} not implemented in encoder", mem.kind),
    }
}

pub fn encode_instruction(instr: &Arm64Instr) -> Result<Vec<u32>, BackendError> {
    let word = match instr {
        Arm64Instr::Nop => 0xd503_201f,
        Arm64Instr::Label(_) => return Ok(Vec::new()),
        Arm64Instr::Adr { rd, offset } => encode_adr(encoding_reg_number(rd.real_reg()), *offset),
        Arm64Instr::MovZ { rd, imm, shift, bits } => {
            encode_movz(*bits, encoding_reg_number(rd.real_reg()), *imm, *shift)
        }
        Arm64Instr::MovK { rd, imm, shift, bits } => {
            encode_movk(*bits, encoding_reg_number(rd.real_reg()), *imm, *shift)
        }
        Arm64Instr::MovN { rd, imm, shift, bits } => {
            encode_movn(*bits, encoding_reg_number(rd.real_reg()), *imm, *shift)
        }
        Arm64Instr::Move { rd, rn, bits } => encode_alu_rrr(
            AluOp::Orr,
            encoding_reg_number(rd.real_reg()),
            31,
            encoding_reg_number(rn.real_reg()),
            *bits,
            false,
        ),
        Arm64Instr::FpuMove { .. } => {
            return Err(BackendError::new("arm64 FPU move encoding is not implemented yet"));
        }
        Arm64Instr::AluRRR { op, rd, rn, rm, bits, set_flags } => encode_alu_rrr(
            *op,
            encoding_reg_number(rd.real_reg()),
            encoding_reg_number(rn.real_reg()),
            encoding_reg_number(rm.real_reg()),
            *bits,
            *set_flags,
        ),
        Arm64Instr::AluRRImm12 { op, rd, rn, imm, bits, set_flags } => encode_alu_rr_imm12(
            *op,
            encoding_reg_number(rd.real_reg()),
            encoding_reg_number(rn.real_reg()),
            imm.encode(),
            *bits,
            *set_flags,
        ),
        Arm64Instr::Cmp { rn, rm, bits } => encode_alu_rrr(
            AluOp::Sub,
            31,
            encoding_reg_number(rn.real_reg()),
            encoding_reg_number(rm.real_reg()),
            *bits,
            true,
        ),
        Arm64Instr::Load { kind, rd, mem, bits } => encode_load_or_store(
            true,
            *bits,
            encoding_reg_number(rd.real_reg()),
            *mem,
            matches!(kind, LoadKind::SLoad),
            matches!(kind, LoadKind::FpuLoad),
        ),
        Arm64Instr::Store { kind, src, mem, bits } => encode_load_or_store(
            false,
            *bits,
            encoding_reg_number(src.real_reg()),
            *mem,
            false,
            matches!(kind, StoreKind::FpuStore),
        ),
        Arm64Instr::CSet { rd, flag } => encode_cset(encoding_reg_number(rd.real_reg()), *flag),
        Arm64Instr::Br { offset, link } => encode_unconditional_branch(*link, *offset),
        Arm64Instr::Call { offset, .. } => encode_unconditional_branch(true, *offset),
        Arm64Instr::BrReg { rn, link } => encode_unconditional_branch_reg(encoding_reg_number(rn.real_reg()), *link),
        Arm64Instr::CondBr { cond, offset, bits64 } => encode_cond_br(*cond, *offset, *bits64),
        Arm64Instr::CallReg { rn, tail, .. } => {
            encode_unconditional_branch_reg(encoding_reg_number(rn.real_reg()), !*tail)
        }
        Arm64Instr::Ret => 0xd65f_03c0,
        Arm64Instr::Udf { imm } => 0x0000_0000 | ((*imm as u32) << 5),
        Arm64Instr::LoadConstBlockArg { .. } => {
            return Err(BackendError::new("load-const-block-arg must be lowered before encoding"));
        }
        Arm64Instr::Raw32(word) => *word,
    };
    Ok(vec![word])
}

#[cfg(test)]
mod tests {
    use super::{
        encode_adr, encode_alu_rr_imm12, encode_alu_rrr, encode_cond_br, encode_cset,
        encode_instruction, encode_movk, encode_movn, encode_movz, encode_unconditional_branch,
        encode_unconditional_branch_reg, DUMMY_INSTRUCTION,
    };
    use crate::backend::isa::arm64::cond::{Cond, CondFlag};
    use crate::backend::isa::arm64::instr::{AluOp, Arm64Instr, LoadKind};
    use crate::backend::isa::arm64::lower_instr_operands::Imm12;
    use crate::backend::isa::arm64::lower_mem::AddressMode;
    use crate::backend::isa::arm64::reg::{vreg_for_real_reg, TMP, X0, X1, X11};

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn dummy_branch_matches_go_test() {
        assert_eq!(encode_unconditional_branch(false, 0), DUMMY_INSTRUCTION);
    }

    #[test]
    fn basic_encodings_match_arm64_reference_values() {
        assert_eq!(encode_unconditional_branch_reg(27, false), 0xd61f_0360);
        assert_eq!(encode_unconditional_branch_reg(12, false), 0xd61f_0180);
        assert_eq!(encode_unconditional_branch(true, 16), 0x9400_0004);
        assert_eq!(encode_adr(27, 16), 0x1000_009b);
        assert_eq!(encode_alu_rrr(AluOp::Add, 27, 27, 12, 64, false), 0x8b0c_037b);
        assert_eq!(encode_alu_rr_imm12(AluOp::Add, 31, 31, Imm12 { bits: 0x10, shift12: false }.encode(), 64, false), 0x9100_43ff);
        assert_eq!(encode_movz(64, 0, 0xa, 0), 0xd280_0140);
        assert_eq!(encode_movk(64, 0, 0xbeef, 16), 0xf2b7_dde0);
        assert_eq!(encode_movn(64, 0, 0, 0), 0x9280_0000);
        assert_eq!(encode_cset(0, CondFlag::Eq), 0x9a9f_17e0);
    }

    #[test]
    fn branch_condition_encodings_match_go_patterns() {
        let x0 = vreg_for_real_reg(X0);
        assert_eq!(encode_cond_br(Cond::from_flag(CondFlag::Ge), 16, true), 0x5400_008a);
        assert_eq!(encode_cond_br(Cond::from_reg_zero(x0), 16, true), 0xb400_0080);
        assert_eq!(encode_cond_br(Cond::from_reg_not_zero(x0), 16, true), 0xb500_0080);
    }

    #[test]
    fn instruction_encoder_handles_core_subset() {
        let x0 = vreg_for_real_reg(X0);
        let x1 = vreg_for_real_reg(X1);
        let word = encode_instruction(&Arm64Instr::Load {
            kind: LoadKind::ULoad,
            rd: x0,
            mem: AddressMode::reg_unsigned_imm12(x1, 8),
            bits: 64,
        })
        .unwrap();
        assert_eq!(word, vec![0xf940_0420]);
    }

    #[test]
    fn trampoline_words_match_go_fixture() {
        let tmp = vreg_for_real_reg(TMP);
        let x11 = vreg_for_real_reg(X11);
        let words = [
            encode_adr(27, 16),
            encode_instruction(&Arm64Instr::Load {
                kind: LoadKind::SLoad,
                rd: x11,
                mem: AddressMode::reg_unsigned_imm12(tmp, 0),
                bits: 32,
            })
            .unwrap()[0],
            encode_alu_rrr(AluOp::Add, 27, 27, 12, 64, false),
            encode_unconditional_branch_reg(27, false),
        ];
        let bytes = words
            .into_iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        assert_eq!(hex(&bytes), "9b0000106b0380b97b030c8b60031fd6");
    }
}
