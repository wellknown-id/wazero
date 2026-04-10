use crate::backend::machine::BackendError;
use crate::backend::{RealReg, RegType, VReg};

use super::cond::Cond;
use super::ext::ExtMode;
use super::instr::{AluRmiROpcode, Amd64Instr, InstructionKind, ShiftROpcode, UnaryRmROpcode};
use super::machine_vec::SseOpcode;
use super::operands::{AddressMode, Operand};
use super::reg::{R12, R13, RBP, RSP, XMM0};

pub fn encode_instruction(inst: &Amd64Instr, buf: &mut Vec<u8>) -> Result<(), BackendError> {
    let d = inst.0.borrow();
    match d.kind {
        InstructionKind::Nop0 => buf.push(0x90),
        InstructionKind::Ret => buf.push(0xC3),
        InstructionKind::Imm => encode_imm(buf, op2_reg(&d)?, d.u1, d.b1),
        InstructionKind::MovRR => encode_reg_reg(buf, 0x89, op1_reg(&d)?, op2_reg(&d)?, d.b1, None),
        InstructionKind::AluRmiR => encode_alu(buf, &d)?,
        InstructionKind::Div => {
            encode_unary_subopcode(buf, op1(&d)?, if d.u1 != 0 { 7 } else { 6 }, d.b1, None)
        }
        InstructionKind::MulHi => {
            encode_unary_subopcode(buf, op1(&d)?, if d.u1 != 0 { 5 } else { 4 }, d.b1, None)
        }
        InstructionKind::Not => encode_unary_subopcode(buf, op1(&d)?, 2, d.b1, None),
        InstructionKind::Neg => encode_unary_subopcode(buf, op1(&d)?, 3, d.b1, None),
        InstructionKind::SignExtendData => {
            if d.b1 {
                emit_rex(buf, false, false, false, false, true);
            }
            buf.push(0x99);
        }
        InstructionKind::UnaryRmR => encode_unary_rm_r(buf, &d)?,
        InstructionKind::ShiftR => encode_shift_r(buf, &d)?,
        InstructionKind::Lea => {
            encode_reg_mem_opcode(buf, 0x8D, op2_reg(&d)?, op1_mem(&d)?, true, None)
        }
        InstructionKind::Mov64MR => {
            encode_reg_mem_opcode(buf, 0x8B, op2_reg(&d)?, op1_mem(&d)?, true, None)
        }
        InstructionKind::MovRM => encode_mov_rm(buf, &d)?,
        InstructionKind::MovzxRmR => encode_movzx(buf, &d)?,
        InstructionKind::MovsxRmR => encode_movsx(buf, &d)?,
        InstructionKind::CmpRmiR => encode_cmp(buf, &d)?,
        InstructionKind::Setcc => encode_setcc(buf, op2_reg(&d)?, Cond::from_u8(d.u1 as u8)),
        InstructionKind::Cmove => encode_cmove(buf, &d)?,
        InstructionKind::Push64 => encode_push64(buf, op1(&d)?)?,
        InstructionKind::Pop64 => encode_pop64(buf, op1_reg_from_op1(&d)?),
        InstructionKind::Jmp => {
            buf.push(0xE9);
            buf.extend_from_slice(&0i32.to_le_bytes());
        }
        InstructionKind::JmpIf => {
            buf.extend_from_slice(&[0x0F, 0x80 | Cond::from_u8(d.u1 as u8).encoding()]);
            buf.extend_from_slice(&0i32.to_le_bytes());
        }
        InstructionKind::Call => {
            buf.push(0xE8);
            buf.extend_from_slice(&0i32.to_le_bytes());
        }
        InstructionKind::CallIndirect => encode_unary_subopcode(buf, op1(&d)?, 2, true, Some(0xFF)),
        InstructionKind::Xchg => encode_xchg(buf, &d)?,
        InstructionKind::XmmUnaryRmR => encode_xmm_unary_rm_r(buf, &d)?,
        InstructionKind::XmmCmpRmR => encode_xmm_cmp_rm_r(buf, &d)?,
        InstructionKind::XmmMovRM => encode_xmm_mov_rm(buf, &d)?,
        InstructionKind::XmmLoadConst => {
            return Err(BackendError::new("xmm const pools are not wired yet"));
        }
    }
    Ok(())
}

fn encode_imm(buf: &mut Vec<u8>, dst: VReg, value: u64, is_64: bool) {
    let enc = reg_encoding(dst.real_reg());
    if is_64 {
        if lower32will_sign_extend_to64(value) {
            emit_rex(buf, false, false, enc.rex, false, true);
            buf.push(0xC7);
            emit_modrm(buf, 0b11, 0, enc.low);
            buf.extend_from_slice(&(value as u32).to_le_bytes());
        } else {
            emit_rex(buf, false, false, enc.rex, false, true);
            buf.push(0xB8 | enc.low);
            buf.extend_from_slice(&value.to_le_bytes());
        }
    } else {
        emit_rex(buf, false, false, enc.rex, false, false);
        buf.push(0xB8 | enc.low);
        buf.extend_from_slice(&(value as u32).to_le_bytes());
    }
}

fn encode_alu(buf: &mut Vec<u8>, d: &super::instr::Amd64InstrData) -> Result<(), BackendError> {
    let src = op1(d)?;
    let dst = op2_reg(d)?;
    let (opc_r, opc_m, sub_imm) = match alu_opcode_from_u64(d.u1) {
        AluRmiROpcode::Add => (0x01, 0x03, 0),
        AluRmiROpcode::Sub => (0x29, 0x2B, 5),
        AluRmiROpcode::And => (0x21, 0x23, 4),
        AluRmiROpcode::Or => (0x09, 0x0B, 1),
        AluRmiROpcode::Xor => (0x31, 0x33, 6),
        AluRmiROpcode::Mul => {
            match src {
                Operand::Reg(reg) => encode_reg_reg(buf, 0x0FAF, *reg, dst, d.b1, None),
                Operand::Mem(amode) => encode_reg_mem_opcode(buf, 0x0FAF, dst, amode, d.b1, None),
                Operand::Imm32(imm32) => {
                    let imm8 = lower8will_sign_extend_to32(*imm32);
                    encode_reg_reg(buf, if imm8 { 0x6B } else { 0x69 }, dst, dst, d.b1, None);
                    if imm8 {
                        buf.push(*imm32 as u8);
                    } else {
                        buf.extend_from_slice(&imm32.to_le_bytes());
                    }
                }
                Operand::Label(_) => return Err(BackendError::new("invalid mul operand")),
            }
            return Ok(());
        }
    };
    match src {
        Operand::Reg(reg) => encode_reg_reg(buf, opc_r, *reg, dst, d.b1, None),
        Operand::Mem(amode) => encode_reg_mem_opcode(buf, opc_m, dst, amode, d.b1, None),
        Operand::Imm32(imm32) => {
            let imm8 = lower8will_sign_extend_to32(*imm32);
            encode_modrm_reg_operand(buf, if imm8 { 0x83 } else { 0x81 }, sub_imm, dst, d.b1);
            if imm8 {
                buf.push(*imm32 as u8);
            } else {
                buf.extend_from_slice(&imm32.to_le_bytes());
            }
        }
        Operand::Label(_) => return Err(BackendError::new("invalid alu operand")),
    }
    Ok(())
}

fn encode_unary_rm_r(
    buf: &mut Vec<u8>,
    d: &super::instr::Amd64InstrData,
) -> Result<(), BackendError> {
    let (prefix, opcode) = match unary_opcode_from_u64(d.u1) {
        UnaryRmROpcode::Bsr => (None, 0x0FBD),
        UnaryRmROpcode::Bsf => (None, 0x0FBC),
        UnaryRmROpcode::Tzcnt => (Some(0xF3), 0x0FBC),
        UnaryRmROpcode::Lzcnt => (Some(0xF3), 0x0FBD),
        UnaryRmROpcode::Popcnt => (Some(0xF3), 0x0FB8),
    };
    match op1(d)? {
        Operand::Reg(src) => {
            encode_reg_rm_opcode(buf, opcode, op2_reg(d)?, &Operand::Reg(*src), d.b1, prefix)?
        }
        Operand::Mem(amode) => encode_reg_mem_opcode(buf, opcode, op2_reg(d)?, amode, d.b1, prefix),
        _ => return Err(BackendError::new("invalid unary operand")),
    }
    Ok(())
}

fn encode_shift_r(buf: &mut Vec<u8>, d: &super::instr::Amd64InstrData) -> Result<(), BackendError> {
    let subopcode = match shift_opcode_from_u64(d.u1) {
        ShiftROpcode::Rol => 0,
        ShiftROpcode::Ror => 1,
        ShiftROpcode::Shl => 4,
        ShiftROpcode::Shr => 5,
        ShiftROpcode::Sar => 7,
    };
    encode_unary_subopcode(buf, op1(d)?, subopcode, d.b1, Some(0xD3));
    Ok(())
}

fn encode_mov_rm(buf: &mut Vec<u8>, d: &super::instr::Amd64InstrData) -> Result<(), BackendError> {
    let src = op1_reg(d)?;
    let dst = op2(d)?;
    let size = d.u1 as u8;
    match dst {
        Operand::Mem(amode) => {
            let prefix = if size == 2 { Some(0x66) } else { None };
            let opcode = if size == 1 { 0x88 } else { 0x89 };
            encode_mem_from_reg(buf, opcode, src, amode, size == 8, prefix);
            Ok(())
        }
        _ => Err(BackendError::new("movrm requires memory destination")),
    }
}

fn encode_movzx(buf: &mut Vec<u8>, d: &super::instr::Amd64InstrData) -> Result<(), BackendError> {
    let mode = ext_mode_from_u64(d.u1);
    match mode {
        ExtMode::BL => encode_reg_rm_opcode(buf, 0x0FB6, op2_reg(d)?, op1(d)?, false, None),
        ExtMode::BQ => encode_reg_rm_opcode(buf, 0x0FB6, op2_reg(d)?, op1(d)?, true, None),
        ExtMode::WL => encode_reg_rm_opcode(buf, 0x0FB7, op2_reg(d)?, op1(d)?, false, None),
        ExtMode::WQ => encode_reg_rm_opcode(buf, 0x0FB7, op2_reg(d)?, op1(d)?, true, None),
        ExtMode::LQ => encode_reg_rm_opcode(buf, 0x8B, op2_reg(d)?, op1(d)?, false, None),
    }
}

fn encode_movsx(buf: &mut Vec<u8>, d: &super::instr::Amd64InstrData) -> Result<(), BackendError> {
    let mode = ext_mode_from_u64(d.u1);
    match mode {
        ExtMode::BL => encode_reg_rm_opcode(buf, 0x0FBE, op2_reg(d)?, op1(d)?, false, None),
        ExtMode::BQ => encode_reg_rm_opcode(buf, 0x0FBE, op2_reg(d)?, op1(d)?, true, None),
        ExtMode::WL => encode_reg_rm_opcode(buf, 0x0FBF, op2_reg(d)?, op1(d)?, false, None),
        ExtMode::WQ => encode_reg_rm_opcode(buf, 0x0FBF, op2_reg(d)?, op1(d)?, true, None),
        ExtMode::LQ => encode_reg_rm_opcode(buf, 0x63, op2_reg(d)?, op1(d)?, true, None),
    }
}

fn encode_cmp(buf: &mut Vec<u8>, d: &super::instr::Amd64InstrData) -> Result<(), BackendError> {
    let is_cmp = d.u1 != 0;
    let src = op1(d)?;
    let dst = op2_reg(d)?;
    if is_cmp {
        match src {
            Operand::Reg(reg) => encode_reg_reg(buf, 0x39, *reg, dst, d.b1, None),
            Operand::Mem(amode) => encode_reg_mem_opcode(buf, 0x3B, dst, amode, d.b1, None),
            Operand::Imm32(imm32) => {
                let imm8 = lower8will_sign_extend_to32(*imm32);
                encode_modrm_reg_operand(buf, if imm8 { 0x83 } else { 0x81 }, 7, dst, d.b1);
                if imm8 {
                    buf.push(*imm32 as u8);
                } else {
                    buf.extend_from_slice(&imm32.to_le_bytes());
                }
            }
            Operand::Label(_) => return Err(BackendError::new("invalid cmp operand")),
        }
    } else {
        match src {
            Operand::Reg(reg) => encode_reg_reg(buf, 0x85, *reg, dst, d.b1, None),
            Operand::Mem(amode) => encode_reg_mem_opcode(buf, 0x85, dst, amode, d.b1, None),
            Operand::Imm32(imm32) => {
                encode_modrm_reg_operand(buf, if d.b1 { 0xF7 } else { 0xF7 }, 0, dst, d.b1);
                buf.extend_from_slice(&imm32.to_le_bytes());
            }
            Operand::Label(_) => return Err(BackendError::new("invalid test operand")),
        }
    }
    Ok(())
}

fn encode_setcc(buf: &mut Vec<u8>, dst: VReg, cond: Cond) {
    let enc = reg_encoding(dst.real_reg());
    emit_rex(buf, false, false, enc.rex, false, false);
    buf.extend_from_slice(&[0x0F, 0x90 | cond.encoding()]);
    emit_modrm(buf, 0b11, 0, enc.low);
}

fn encode_cmove(buf: &mut Vec<u8>, d: &super::instr::Amd64InstrData) -> Result<(), BackendError> {
    let cond = Cond::from_u8(d.u1 as u8);
    encode_reg_rm_opcode(
        buf,
        0x0F40 | cond.encoding() as u32,
        op2_reg(d)?,
        op1(d)?,
        d.b1,
        None,
    )
}

fn encode_push64(buf: &mut Vec<u8>, op: &Operand) -> Result<(), BackendError> {
    match op {
        Operand::Reg(reg) => {
            let enc = reg_encoding(reg.real_reg());
            emit_rex(buf, false, false, enc.rex, false, false);
            buf.push(0x50 | enc.low);
        }
        Operand::Imm32(imm32) => {
            buf.push(0x68);
            buf.extend_from_slice(&imm32.to_le_bytes());
        }
        _ => return Err(BackendError::new("unsupported push operand")),
    }
    Ok(())
}

fn encode_pop64(buf: &mut Vec<u8>, reg: VReg) {
    let enc = reg_encoding(reg.real_reg());
    emit_rex(buf, false, false, enc.rex, false, false);
    buf.push(0x58 | enc.low);
}

fn encode_xchg(buf: &mut Vec<u8>, d: &super::instr::Amd64InstrData) -> Result<(), BackendError> {
    let size = d.u1 as u8;
    let opcode = if size == 1 { 0x86 } else { 0x87 };
    let lhs = op1_reg(d)?;
    match op2(d)? {
        Operand::Reg(rhs) => encode_reg_reg(
            buf,
            opcode,
            lhs,
            *rhs,
            size == 8,
            if size == 2 { Some(0x66) } else { None },
        ),
        Operand::Mem(amode) => {
            let prefix = if size == 2 { Some(0x66) } else { None };
            encode_mem_from_reg(buf, opcode, lhs, amode, size == 8, prefix);
        }
        _ => return Err(BackendError::new("invalid xchg operand")),
    }
    Ok(())
}

fn encode_xmm_unary_rm_r(
    buf: &mut Vec<u8>,
    d: &super::instr::Amd64InstrData,
) -> Result<(), BackendError> {
    let op = sse_opcode_from_u64(d.u1);
    let enc = op.encoding();
    encode_reg_rm_opcode(
        buf,
        enc.load_opcode,
        op2_reg(d)?,
        op1(d)?,
        false,
        enc.prefix,
    )
}

fn encode_xmm_mov_rm(
    buf: &mut Vec<u8>,
    d: &super::instr::Amd64InstrData,
) -> Result<(), BackendError> {
    let op = sse_opcode_from_u64(d.u1);
    let enc = op.encoding();
    match op2(d)? {
        Operand::Mem(amode) => {
            let src = op1_reg(d)?;
            let src_enc = reg_encoding(src.real_reg());
            if let Some(prefix) = enc.prefix {
                buf.push(prefix);
            }
            emit_rex(buf, false, src_enc.rex, false, false, false);
            emit_opcode(buf, enc.store_opcode);
            encode_mem(buf, src_enc.low, amode);
            Ok(())
        }
        _ => Err(BackendError::new("xmm store requires memory destination")),
    }
}

fn encode_xmm_cmp_rm_r(
    buf: &mut Vec<u8>,
    d: &super::instr::Amd64InstrData,
) -> Result<(), BackendError> {
    let op = sse_opcode_from_u64(d.u1);
    let enc = op.encoding();
    encode_reg_rm_opcode(
        buf,
        enc.load_opcode,
        op2_reg(d)?,
        op1(d)?,
        false,
        enc.prefix,
    )
}

fn encode_reg_rm_opcode(
    buf: &mut Vec<u8>,
    opcode: u32,
    reg: VReg,
    rm: &Operand,
    rex_w: bool,
    prefix: Option<u8>,
) -> Result<(), BackendError> {
    match rm {
        Operand::Reg(src) => {
            let reg = reg_encoding(reg.real_reg());
            let rm = reg_encoding(src.real_reg());
            if let Some(prefix) = prefix {
                buf.push(prefix);
            }
            emit_rex(buf, reg.rex, false, rm.rex, false, rex_w);
            emit_opcode(buf, opcode);
            emit_modrm(buf, 0b11, reg.low, rm.low);
            Ok(())
        }
        Operand::Mem(amode) => {
            encode_reg_mem_opcode(buf, opcode, reg, amode, rex_w, prefix);
            Ok(())
        }
        _ => Err(BackendError::new("expected reg or mem operand")),
    }
}

fn encode_reg_reg(
    buf: &mut Vec<u8>,
    opcode: u32,
    src: VReg,
    dst: VReg,
    rex_w: bool,
    prefix: Option<u8>,
) {
    let src = reg_encoding(src.real_reg());
    let dst = reg_encoding(dst.real_reg());
    if let Some(prefix) = prefix {
        buf.push(prefix);
    }
    emit_rex(buf, src.rex, false, dst.rex, false, rex_w);
    emit_opcode(buf, opcode);
    emit_modrm(buf, 0b11, src.low, dst.low);
}

fn encode_reg_mem_opcode(
    buf: &mut Vec<u8>,
    opcode: u32,
    reg: VReg,
    amode: &AddressMode,
    rex_w: bool,
    prefix: Option<u8>,
) {
    let reg = reg_encoding(reg.real_reg());
    if let Some(prefix) = prefix {
        buf.push(prefix);
    }
    let (x, b) = rex_for_amode(amode);
    emit_rex(buf, reg.rex, x, b, false, rex_w);
    emit_opcode(buf, opcode);
    encode_mem(buf, reg.low, amode);
}

fn encode_mem_from_reg(
    buf: &mut Vec<u8>,
    opcode: u32,
    src: VReg,
    amode: &AddressMode,
    rex_w: bool,
    prefix: Option<u8>,
) {
    let src = reg_encoding(src.real_reg());
    if let Some(prefix) = prefix {
        buf.push(prefix);
    }
    let (x, b) = rex_for_amode(amode);
    emit_rex(buf, src.rex, x, b, false, rex_w);
    emit_opcode(buf, opcode);
    encode_mem(buf, src.low, amode);
}

fn encode_unary_subopcode(
    buf: &mut Vec<u8>,
    op: &Operand,
    subopcode: u8,
    rex_w: bool,
    override_opcode: Option<u8>,
) {
    match op {
        Operand::Reg(reg) => {
            let enc = reg_encoding(reg.real_reg());
            emit_rex(buf, false, false, enc.rex, false, rex_w);
            buf.push(override_opcode.unwrap_or(0xF7));
            emit_modrm(buf, 0b11, subopcode, enc.low);
        }
        Operand::Mem(amode) => {
            let (x, b) = rex_for_amode(amode);
            emit_rex(buf, false, x, b, false, rex_w);
            buf.push(override_opcode.unwrap_or(0xF7));
            encode_mem(buf, subopcode, amode);
        }
        _ => panic!("invalid rm operand"),
    }
}

fn shift_opcode_from_u64(raw: u64) -> ShiftROpcode {
    match raw {
        0 => ShiftROpcode::Shl,
        1 => ShiftROpcode::Shr,
        2 => ShiftROpcode::Sar,
        3 => ShiftROpcode::Rol,
        _ => ShiftROpcode::Ror,
    }
}

fn encode_modrm_reg_operand(buf: &mut Vec<u8>, opcode: u8, subopcode: u8, reg: VReg, rex_w: bool) {
    let enc = reg_encoding(reg.real_reg());
    emit_rex(buf, false, false, enc.rex, false, rex_w);
    buf.push(opcode);
    emit_modrm(buf, 0b11, subopcode, enc.low);
}

fn encode_mem(buf: &mut Vec<u8>, reg_bits: u8, amode: &AddressMode) {
    match amode {
        AddressMode::ImmReg { imm32, base } => {
            encode_base_disp(buf, reg_bits, *base, *imm32 as i32, None)
        }
        AddressMode::ImmRbp { imm32 } => encode_base_disp(
            buf,
            reg_bits,
            VReg::from_real_reg(RBP, RegType::Int),
            *imm32 as i32,
            None,
        ),
        AddressMode::RegRegShift {
            imm32,
            base,
            index,
            shift,
        } => encode_base_disp(buf, reg_bits, *base, *imm32 as i32, Some((*index, *shift))),
        AddressMode::RipRel { .. } => {
            emit_modrm(buf, 0, reg_bits, 0b101);
            buf.extend_from_slice(&0i32.to_le_bytes());
        }
    }
}

fn encode_base_disp(
    buf: &mut Vec<u8>,
    reg_bits: u8,
    base: VReg,
    disp: i32,
    index: Option<(VReg, u8)>,
) {
    let base_enc = reg_encoding(base.real_reg());
    let needs_sib = matches!(base.real_reg(), RSP | R12) || index.is_some();
    let disp_mode = if disp == 0 && !matches!(base.real_reg(), RBP | R13) {
        0
    } else if (-128..=127).contains(&disp) {
        1
    } else {
        2
    };

    emit_modrm(
        buf,
        disp_mode,
        reg_bits,
        if needs_sib { 0b100 } else { base_enc.low },
    );
    if needs_sib {
        let (index_bits, scale) = if let Some((index_reg, shift)) = index {
            (reg_encoding(index_reg.real_reg()).low, shift)
        } else {
            (0b100, 0)
        };
        emit_sib(buf, scale, index_bits, base_enc.low);
    }
    match disp_mode {
        1 => buf.push(disp as i8 as u8),
        2 => buf.extend_from_slice(&disp.to_le_bytes()),
        _ => {}
    }
}

fn emit_opcode(buf: &mut Vec<u8>, opcode: u32) {
    if opcode > 0xFFFF {
        buf.push((opcode >> 16) as u8);
    }
    if opcode > 0xFF {
        buf.push((opcode >> 8) as u8);
    }
    buf.push(opcode as u8);
}

fn emit_rex(buf: &mut Vec<u8>, r: bool, x: bool, b: bool, force: bool, w: bool) {
    let rex = 0x40 | ((w as u8) << 3) | ((r as u8) << 2) | ((x as u8) << 1) | (b as u8);
    if force || rex != 0x40 {
        buf.push(rex);
    }
}

fn emit_modrm(buf: &mut Vec<u8>, mode: u8, reg: u8, rm: u8) {
    buf.push((mode << 6) | ((reg & 0x7) << 3) | (rm & 0x7));
}

fn emit_sib(buf: &mut Vec<u8>, scale: u8, index: u8, base: u8) {
    buf.push(((scale & 0x3) << 6) | ((index & 0x7) << 3) | (base & 0x7));
}

#[derive(Clone, Copy)]
struct RegEnc {
    low: u8,
    rex: bool,
}

fn reg_encoding(reg: RealReg) -> RegEnc {
    let raw = if reg >= XMM0 { reg - XMM0 } else { reg - 1 };
    RegEnc {
        low: raw & 0x7,
        rex: raw >= 8,
    }
}

fn rex_for_amode(amode: &AddressMode) -> (bool, bool) {
    match amode {
        AddressMode::ImmReg { base, .. } => (false, reg_encoding(base.real_reg()).rex),
        AddressMode::ImmRbp { .. } => (false, false),
        AddressMode::RegRegShift { base, index, .. } => (
            reg_encoding(index.real_reg()).rex,
            reg_encoding(base.real_reg()).rex,
        ),
        AddressMode::RipRel { .. } => (false, false),
    }
}

fn op1<'a>(d: &'a super::instr::Amd64InstrData) -> Result<&'a Operand, BackendError> {
    d.op1
        .as_ref()
        .ok_or_else(|| BackendError::new("missing op1"))
}

fn op2<'a>(d: &'a super::instr::Amd64InstrData) -> Result<&'a Operand, BackendError> {
    d.op2
        .as_ref()
        .ok_or_else(|| BackendError::new("missing op2"))
}

fn op1_reg(d: &super::instr::Amd64InstrData) -> Result<VReg, BackendError> {
    match op1(d)? {
        Operand::Reg(reg) => Ok(*reg),
        _ => Err(BackendError::new("expected register op1")),
    }
}

fn op1_reg_from_op1(d: &super::instr::Amd64InstrData) -> Result<VReg, BackendError> {
    op1_reg(d)
}

fn op2_reg(d: &super::instr::Amd64InstrData) -> Result<VReg, BackendError> {
    match op2(d)? {
        Operand::Reg(reg) => Ok(*reg),
        _ => Err(BackendError::new("expected register op2")),
    }
}

fn op1_mem<'a>(d: &'a super::instr::Amd64InstrData) -> Result<&'a AddressMode, BackendError> {
    match op1(d)? {
        Operand::Mem(amode) => Ok(amode),
        _ => Err(BackendError::new("expected memory op1")),
    }
}

fn alu_opcode_from_u64(raw: u64) -> AluRmiROpcode {
    match raw {
        0 => AluRmiROpcode::Add,
        1 => AluRmiROpcode::Sub,
        2 => AluRmiROpcode::And,
        3 => AluRmiROpcode::Or,
        4 => AluRmiROpcode::Xor,
        _ => AluRmiROpcode::Mul,
    }
}

fn unary_opcode_from_u64(raw: u64) -> UnaryRmROpcode {
    match raw {
        0 => UnaryRmROpcode::Bsr,
        1 => UnaryRmROpcode::Bsf,
        2 => UnaryRmROpcode::Tzcnt,
        3 => UnaryRmROpcode::Lzcnt,
        _ => UnaryRmROpcode::Popcnt,
    }
}

fn ext_mode_from_u64(raw: u64) -> ExtMode {
    match raw {
        0 => ExtMode::BL,
        1 => ExtMode::BQ,
        2 => ExtMode::WL,
        3 => ExtMode::WQ,
        _ => ExtMode::LQ,
    }
}

fn sse_opcode_from_u64(raw: u64) -> SseOpcode {
    SseOpcode::from_u64(raw)
}

fn lower32will_sign_extend_to64(value: u64) -> bool {
    (value as i64) == (value as i32 as i64)
}

fn lower8will_sign_extend_to32(value: u32) -> bool {
    (value as i32) == (value as i8 as i32)
}

#[cfg(test)]
mod tests {
    use super::super::ext::ExtMode;
    use super::super::instr::{AluRmiROpcode, Amd64Instr, UnaryRmROpcode};
    use super::super::operands::{AddressMode, Operand};
    use crate::backend::{RegType, VReg};

    fn hex(bytes: Vec<u8>) -> String {
        bytes.into_iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn encodes_core_go_test_vectors() {
        let r14 = VReg::from_real_reg(15, RegType::Int);
        let rcx = VReg::from_real_reg(2, RegType::Int);
        let rax = VReg::from_real_reg(1, RegType::Int);
        let rdi = VReg::from_real_reg(8, RegType::Int);
        let r15 = VReg::from_real_reg(16, RegType::Int);
        let r11 = VReg::from_real_reg(12, RegType::Int);
        let r12 = VReg::from_real_reg(13, RegType::Int);
        let cases = [
            (Amd64Instr::ret(), "ret", "c3"),
            (
                Amd64Instr::imm(r14, 1_234_567, false),
                "movl $1234567, %r14d",
                "41be87d61200",
            ),
            (
                Amd64Instr::imm(r14, 0x2_0000_0000, true),
                "movabsq $8589934592, %r14",
                "49be0000000002000000",
            ),
            (
                Amd64Instr::div(Operand::reg(rax), true, true),
                "idivq %rax",
                "48f7f8",
            ),
            (
                Amd64Instr::mov_rr(rax, rdi, false),
                "movl %eax, %edi",
                "89c7",
            ),
            (
                Amd64Instr::mov_rr(r11, r12, true),
                "movq %r11, %r12",
                "4d89dc",
            ),
            (
                Amd64Instr::not(Operand::reg(rax), true),
                "notq %rax",
                "48f7d0",
            ),
            (
                Amd64Instr::neg(Operand::reg(rax), true),
                "negq %rax",
                "48f7d8",
            ),
            (
                Amd64Instr::mul_hi(Operand::reg(r15), true, true),
                "imulq %r15",
                "49f7ef",
            ),
            (
                Amd64Instr::unary_rm_r(UnaryRmROpcode::Bsr, Operand::reg(rax), rdi, true),
                "bsrq %rax, %rdi",
                "480fbdf8",
            ),
            (
                Amd64Instr::movzx_rm_r(
                    ExtMode::LQ,
                    Operand::mem(AddressMode::imm_reg(123, rax)),
                    rdi,
                ),
                "movzx.lq 123(%rax), %rdi",
                "8b787b",
            ),
            (
                Amd64Instr::alu_rmi_r(AluRmiROpcode::Add, Operand::imm32(123), rcx, false),
                "add $123, %ecx",
                "83c17b",
            ),
        ];

        for (inst, expected_text, expected_hex) in cases {
            assert_eq!(inst.to_string(), expected_text);
            assert_eq!(hex(inst.encode().unwrap()), expected_hex);
        }
    }
}
