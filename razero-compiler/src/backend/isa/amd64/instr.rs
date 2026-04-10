use core::fmt;
use std::cell::RefCell;
use std::rc::Rc;

use crate::backend::abi_info_from_u64;
use crate::backend::machine::BackendError;
use crate::backend::VReg;

use super::cond::Cond;
use super::ext::ExtMode;
use super::instr_encoding::encode_instruction;
use super::machine_vec::SseOpcode;
use super::operands::Operand;
use super::reg::{
    format_vreg_sized, vreg_for_real_reg, FLOAT_ARG_RESULT_REGS, INT_ARG_RESULT_REGS, RAX, RCX, RDX,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AluRmiROpcode {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Mul,
}

impl fmt::Display for AluRmiROpcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Add => "add",
            Self::Sub => "sub",
            Self::And => "and",
            Self::Or => "or",
            Self::Xor => "xor",
            Self::Mul => "imul",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryRmROpcode {
    Bsr,
    Bsf,
    Tzcnt,
    Lzcnt,
    Popcnt,
}

impl fmt::Display for UnaryRmROpcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bsr => "bsr",
            Self::Bsf => "bsf",
            Self::Tzcnt => "tzcnt",
            Self::Lzcnt => "lzcnt",
            Self::Popcnt => "popcnt",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShiftROpcode {
    Shl,
    Shr,
    Sar,
    Rol,
    Ror,
}

impl fmt::Display for ShiftROpcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Shl => "shl",
            Self::Shr => "shr",
            Self::Sar => "sar",
            Self::Rol => "rol",
            Self::Ror => "ror",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InstructionKind {
    Nop0,
    Ret,
    Imm,
    AluRmiR,
    MovRR,
    UnaryRmR,
    ShiftR,
    Not,
    Neg,
    Div,
    MulHi,
    SignExtendData,
    MovzxRmR,
    MovsxRmR,
    Mov64MR,
    MovRM,
    Lea,
    CmpRmiR,
    Setcc,
    Cmove,
    Push64,
    Pop64,
    Jmp,
    JmpIf,
    Call,
    CallIndirect,
    Xchg,
    XmmUnaryRmR,
    XmmMovRM,
    XmmLoadConst,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Amd64InstrData {
    pub kind: InstructionKind,
    pub op1: Option<Operand>,
    pub op2: Option<Operand>,
    pub u1: u64,
    pub u2: u64,
    pub b1: bool,
}

impl Default for Amd64InstrData {
    fn default() -> Self {
        Self {
            kind: InstructionKind::Nop0,
            op1: None,
            op2: None,
            u1: 0,
            u2: 0,
            b1: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Amd64Instr(pub(crate) Rc<RefCell<Amd64InstrData>>);

impl PartialEq for Amd64Instr {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Amd64Instr {}

impl Default for Amd64Instr {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(Amd64InstrData::default())))
    }
}

impl Amd64Instr {
    pub fn new(kind: InstructionKind) -> Self {
        let inst = Self::default();
        inst.0.borrow_mut().kind = kind;
        inst
    }

    pub fn ret() -> Self {
        Self::new(InstructionKind::Ret)
    }

    pub fn imm(dst: VReg, value: u64, is_64: bool) -> Self {
        let inst = Self::new(InstructionKind::Imm);
        {
            let mut d = inst.0.borrow_mut();
            d.op2 = Some(Operand::reg(dst));
            d.u1 = value;
            d.b1 = is_64;
        }
        inst
    }

    pub fn alu_rmi_r(opcode: AluRmiROpcode, src: Operand, dst: VReg, is_64: bool) -> Self {
        let inst = Self::new(InstructionKind::AluRmiR);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(src);
            d.op2 = Some(Operand::reg(dst));
            d.u1 = opcode as u64;
            d.b1 = is_64;
        }
        inst
    }

    pub fn mov_rr(src: VReg, dst: VReg, is_64: bool) -> Self {
        let inst = Self::new(InstructionKind::MovRR);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(Operand::reg(src));
            d.op2 = Some(Operand::reg(dst));
            d.b1 = is_64;
        }
        inst
    }

    pub fn unary_rm_r(opcode: UnaryRmROpcode, src: Operand, dst: VReg, is_64: bool) -> Self {
        let inst = Self::new(InstructionKind::UnaryRmR);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(src);
            d.op2 = Some(Operand::reg(dst));
            d.u1 = opcode as u64;
            d.b1 = is_64;
        }
        inst
    }

    pub fn not(src_dst: Operand, is_64: bool) -> Self {
        let inst = Self::new(InstructionKind::Not);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(src_dst);
            d.b1 = is_64;
        }
        inst
    }

    pub fn shift_r(opcode: ShiftROpcode, src_dst: VReg, is_64: bool) -> Self {
        let inst = Self::new(InstructionKind::ShiftR);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(Operand::reg(src_dst));
            d.u1 = opcode as u64;
            d.b1 = is_64;
        }
        inst
    }

    pub fn neg(src_dst: Operand, is_64: bool) -> Self {
        let inst = Self::new(InstructionKind::Neg);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(src_dst);
            d.b1 = is_64;
        }
        inst
    }

    pub fn div(divisor: Operand, signed: bool, is_64: bool) -> Self {
        let inst = Self::new(InstructionKind::Div);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(divisor);
            d.u1 = u64::from(signed);
            d.b1 = is_64;
        }
        inst
    }

    pub fn mul_hi(src: Operand, signed: bool, is_64: bool) -> Self {
        let inst = Self::new(InstructionKind::MulHi);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(src);
            d.u1 = u64::from(signed);
            d.b1 = is_64;
        }
        inst
    }

    pub fn sign_extend_data(is_64: bool) -> Self {
        let inst = Self::new(InstructionKind::SignExtendData);
        inst.0.borrow_mut().b1 = is_64;
        inst
    }

    pub fn movzx_rm_r(mode: ExtMode, src: Operand, dst: VReg) -> Self {
        let inst = Self::new(InstructionKind::MovzxRmR);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(src);
            d.op2 = Some(Operand::reg(dst));
            d.u1 = mode as u64;
        }
        inst
    }

    pub fn movsx_rm_r(mode: ExtMode, src: Operand, dst: VReg) -> Self {
        let inst = Self::new(InstructionKind::MovsxRmR);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(src);
            d.op2 = Some(Operand::reg(dst));
            d.u1 = mode as u64;
        }
        inst
    }

    pub fn mov64_mr(src: Operand, dst: VReg) -> Self {
        let inst = Self::new(InstructionKind::Mov64MR);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(src);
            d.op2 = Some(Operand::reg(dst));
            d.b1 = true;
        }
        inst
    }

    pub fn mov_rm(src: VReg, dst: Operand, size: u8) -> Self {
        let inst = Self::new(InstructionKind::MovRM);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(Operand::reg(src));
            d.op2 = Some(dst);
            d.u1 = size as u64;
        }
        inst
    }

    pub fn lea(src: Operand, dst: VReg) -> Self {
        let inst = Self::new(InstructionKind::Lea);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(src);
            d.op2 = Some(Operand::reg(dst));
        }
        inst
    }

    pub fn cmp_rmi_r(src: Operand, dst: VReg, is_cmp: bool, is_64: bool) -> Self {
        let inst = Self::new(InstructionKind::CmpRmiR);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(src);
            d.op2 = Some(Operand::reg(dst));
            d.u1 = u64::from(is_cmp);
            d.b1 = is_64;
        }
        inst
    }

    pub fn setcc(cond: Cond, dst: VReg) -> Self {
        let inst = Self::new(InstructionKind::Setcc);
        {
            let mut d = inst.0.borrow_mut();
            d.op2 = Some(Operand::reg(dst));
            d.u1 = cond as u64;
        }
        inst
    }

    pub fn cmove(cond: Cond, src: Operand, dst: VReg, is_64: bool) -> Self {
        let inst = Self::new(InstructionKind::Cmove);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(src);
            d.op2 = Some(Operand::reg(dst));
            d.u1 = cond as u64;
            d.b1 = is_64;
        }
        inst
    }

    pub fn push64(src: Operand) -> Self {
        let inst = Self::new(InstructionKind::Push64);
        inst.0.borrow_mut().op1 = Some(src);
        inst
    }

    pub fn pop64(dst: VReg) -> Self {
        let inst = Self::new(InstructionKind::Pop64);
        inst.0.borrow_mut().op1 = Some(Operand::reg(dst));
        inst
    }

    pub fn jmp(target: Operand) -> Self {
        let inst = Self::new(InstructionKind::Jmp);
        inst.0.borrow_mut().op1 = Some(target);
        inst
    }

    pub fn jmp_if(cond: Cond, target: Operand) -> Self {
        let inst = Self::new(InstructionKind::JmpIf);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(target);
            d.u1 = cond as u64;
        }
        inst
    }

    pub fn call(func_ref: u64, abi_info: u64) -> Self {
        let inst = Self::new(InstructionKind::Call);
        {
            let mut d = inst.0.borrow_mut();
            d.u1 = func_ref;
            d.u2 = abi_info;
        }
        inst
    }

    pub fn call_indirect(target: Operand, abi_info: u64) -> Self {
        let inst = Self::new(InstructionKind::CallIndirect);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(target);
            d.u2 = abi_info;
        }
        inst
    }

    pub fn xchg(lhs: Operand, rhs: Operand, size: u8) -> Self {
        let inst = Self::new(InstructionKind::Xchg);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(lhs);
            d.op2 = Some(rhs);
            d.u1 = size as u64;
        }
        inst
    }

    pub fn xmm_unary_rm_r(op: SseOpcode, src: Operand, dst: VReg) -> Self {
        let inst = Self::new(InstructionKind::XmmUnaryRmR);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(src);
            d.op2 = Some(Operand::reg(dst));
            d.u1 = op as u64;
        }
        inst
    }

    pub fn xmm_mov_rm(op: SseOpcode, src: VReg, dst: Operand) -> Self {
        let inst = Self::new(InstructionKind::XmmMovRM);
        {
            let mut d = inst.0.borrow_mut();
            d.op1 = Some(Operand::reg(src));
            d.op2 = Some(dst);
            d.u1 = op as u64;
        }
        inst
    }

    pub fn xmm_load_const(dst: VReg, bits: u64) -> Self {
        let inst = Self::new(InstructionKind::XmmLoadConst);
        {
            let mut d = inst.0.borrow_mut();
            d.op2 = Some(Operand::reg(dst));
            d.u1 = bits;
        }
        inst
    }

    pub fn kind(&self) -> InstructionKind {
        self.0.borrow().kind
    }

    pub fn defs(&self, out: &mut Vec<VReg>) {
        out.clear();
        let d = self.0.borrow();
        match d.kind {
            InstructionKind::Ret
            | InstructionKind::CmpRmiR
            | InstructionKind::Push64
            | InstructionKind::Jmp
            | InstructionKind::JmpIf
            | InstructionKind::Nop0 => {}
            InstructionKind::Call | InstructionKind::CallIndirect => {
                let (_, _, ret_int, ret_float, _) = abi_info_from_u64(d.u2);
                for &reg in INT_ARG_RESULT_REGS.iter().take(ret_int as usize) {
                    out.push(vreg_for_real_reg(reg));
                }
                for &reg in FLOAT_ARG_RESULT_REGS.iter().take(ret_float as usize) {
                    out.push(vreg_for_real_reg(reg));
                }
            }
            InstructionKind::Div | InstructionKind::SignExtendData => {
                out.push(vreg_for_real_reg(RDX));
                if matches!(d.kind, InstructionKind::Div) {
                    out.push(vreg_for_real_reg(RAX));
                }
            }
            InstructionKind::Pop64 => {
                if let Some(Operand::Reg(reg)) = d.op1 {
                    out.push(reg);
                }
            }
            InstructionKind::Not | InstructionKind::Neg | InstructionKind::ShiftR => {
                if let Some(Operand::Reg(reg)) = d.op1 {
                    out.push(reg);
                }
            }
            InstructionKind::Xchg => {
                if let Some(Operand::Reg(reg)) = d.op1 {
                    out.push(reg);
                }
                if let Some(Operand::Reg(reg)) = d.op2 {
                    out.push(reg);
                }
            }
            _ => {
                if let Some(Operand::Reg(reg)) = d.op2 {
                    out.push(reg);
                }
            }
        }
    }

    pub fn uses(&self, out: &mut Vec<VReg>) {
        out.clear();
        let d = self.0.borrow();
        match d.kind {
            InstructionKind::Ret
            | InstructionKind::Jmp
            | InstructionKind::JmpIf
            | InstructionKind::Setcc
            | InstructionKind::Nop0 => {}
            InstructionKind::Call => {
                let (arg_int, arg_float, _, _, _) = abi_info_from_u64(d.u2);
                for &reg in INT_ARG_RESULT_REGS.iter().take(arg_int as usize) {
                    out.push(vreg_for_real_reg(reg));
                }
                for &reg in FLOAT_ARG_RESULT_REGS.iter().take(arg_float as usize) {
                    out.push(vreg_for_real_reg(reg));
                }
            }
            InstructionKind::CallIndirect => {
                if let Some(op1) = &d.op1 {
                    op1.uses(out);
                }
                let (arg_int, arg_float, _, _, _) = abi_info_from_u64(d.u2);
                for &reg in INT_ARG_RESULT_REGS.iter().take(arg_int as usize) {
                    out.push(vreg_for_real_reg(reg));
                }
                for &reg in FLOAT_ARG_RESULT_REGS.iter().take(arg_float as usize) {
                    out.push(vreg_for_real_reg(reg));
                }
            }
            InstructionKind::Div => {
                out.push(vreg_for_real_reg(RAX));
                out.push(vreg_for_real_reg(RDX));
                if let Some(op1) = &d.op1 {
                    op1.uses(out);
                }
            }
            InstructionKind::SignExtendData => {
                out.push(vreg_for_real_reg(RAX));
            }
            InstructionKind::AluRmiR
            | InstructionKind::CmpRmiR
            | InstructionKind::Cmove
            | InstructionKind::Xchg => {
                if let Some(op1) = &d.op1 {
                    op1.uses(out);
                }
                if let Some(op2) = &d.op2 {
                    op2.uses(out);
                }
            }
            InstructionKind::MovRM | InstructionKind::XmmMovRM => {
                if let Some(op1) = &d.op1 {
                    op1.uses(out);
                }
                if let Some(op2) = &d.op2 {
                    op2.uses(out);
                }
            }
            InstructionKind::ShiftR => {
                if let Some(op1) = &d.op1 {
                    op1.uses(out);
                }
                out.push(vreg_for_real_reg(RCX));
            }
            _ => {
                if let Some(op1) = &d.op1 {
                    op1.uses(out);
                }
            }
        }
    }

    pub fn assign_use(&self, index: usize, reg: VReg) {
        let mut d = self.0.borrow_mut();
        let mut seen = 0usize;
        if let Some(op1) = d.op1.as_mut() {
            match op1 {
                Operand::Reg(r) => {
                    if seen == index {
                        *r = reg;
                        return;
                    }
                    seen += 1;
                }
                Operand::Mem(amode) => {
                    if index < seen + amode.nregs() {
                        amode.assign_use(index - seen, reg);
                        return;
                    }
                    seen += amode.nregs();
                }
                Operand::Imm32(_) | Operand::Label(_) => {}
            }
        }
        if let Some(op2) = d.op2.as_mut() {
            match op2 {
                Operand::Reg(r) => {
                    if seen == index {
                        *r = reg;
                        return;
                    }
                }
                Operand::Mem(amode) => {
                    if index < seen + amode.nregs() {
                        amode.assign_use(index - seen, reg);
                        return;
                    }
                }
                Operand::Imm32(_) | Operand::Label(_) => {}
            }
        }
        panic!("invalid use index {index}");
    }

    pub fn assign_def(&self, reg: VReg) {
        let mut d = self.0.borrow_mut();
        if let Some(Operand::Reg(dst)) = d.op2.as_mut() {
            *dst = reg;
        } else if let Some(Operand::Reg(dst)) = d.op1.as_mut() {
            *dst = reg;
        } else {
            panic!("instruction has no register def");
        }
    }

    pub fn is_copy(&self) -> bool {
        matches!(self.kind(), InstructionKind::MovRR)
    }

    pub fn is_call(&self) -> bool {
        matches!(self.kind(), InstructionKind::Call)
    }

    pub fn is_indirect_call(&self) -> bool {
        matches!(self.kind(), InstructionKind::CallIndirect)
    }

    pub fn is_return(&self) -> bool {
        matches!(self.kind(), InstructionKind::Ret)
    }

    pub fn encode(&self) -> Result<Vec<u8>, BackendError> {
        let mut buf = Vec::new();
        encode_instruction(self, &mut buf)?;
        Ok(buf)
    }
}

impl fmt::Display for Amd64Instr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.0.borrow();
        let op1 = d.op1.as_ref();
        let op2 = d.op2.as_ref();
        match d.kind {
            InstructionKind::Nop0 => f.write_str("nop"),
            InstructionKind::Ret => f.write_str("ret"),
            InstructionKind::Imm => {
                let dst = op2.expect("imm dst");
                if d.b1 {
                    write!(f, "movabsq ${}, {}", d.u1 as i64, dst.format(true))
                } else {
                    write!(f, "movl ${}, {}", d.u1 as i32, dst.format(false))
                }
            }
            InstructionKind::AluRmiR => write!(
                f,
                "{} {}, {}",
                match d.u1 {
                    0 => AluRmiROpcode::Add,
                    1 => AluRmiROpcode::Sub,
                    2 => AluRmiROpcode::And,
                    3 => AluRmiROpcode::Or,
                    4 => AluRmiROpcode::Xor,
                    _ => AluRmiROpcode::Mul,
                },
                op1.expect("alu src").format(d.b1),
                op2.expect("alu dst").format(d.b1)
            ),
            InstructionKind::MovRR => {
                write!(
                    f,
                    "mov{} {}, {}",
                    if d.b1 { "q" } else { "l" },
                    op1.expect("mov src").format(d.b1),
                    op2.expect("mov dst").format(d.b1)
                )
            }
            InstructionKind::UnaryRmR => write!(
                f,
                "{}{} {}, {}",
                match d.u1 {
                    0 => UnaryRmROpcode::Bsr,
                    1 => UnaryRmROpcode::Bsf,
                    2 => UnaryRmROpcode::Tzcnt,
                    3 => UnaryRmROpcode::Lzcnt,
                    _ => UnaryRmROpcode::Popcnt,
                },
                if d.b1 { "q" } else { "l" },
                op1.expect("unary src").format(d.b1),
                op2.expect("unary dst").format(d.b1)
            ),
            InstructionKind::ShiftR => write!(
                f,
                "{}{} %cl, {}",
                match d.u1 {
                    0 => ShiftROpcode::Shl,
                    1 => ShiftROpcode::Shr,
                    2 => ShiftROpcode::Sar,
                    3 => ShiftROpcode::Rol,
                    _ => ShiftROpcode::Ror,
                },
                if d.b1 { "q" } else { "l" },
                op1.expect("shift").format(d.b1)
            ),
            InstructionKind::Not => write!(
                f,
                "not{} {}",
                if d.b1 { "q" } else { "l" },
                op1.expect("not").format(d.b1)
            ),
            InstructionKind::Neg => write!(
                f,
                "neg{} {}",
                if d.b1 { "q" } else { "l" },
                op1.expect("neg").format(d.b1)
            ),
            InstructionKind::Div => write!(
                f,
                "{}div{} {}",
                if d.u1 != 0 { "i" } else { "" },
                if d.b1 { "q" } else { "l" },
                op1.expect("div").format(d.b1)
            ),
            InstructionKind::MulHi => {
                let op = match (d.u1 != 0, d.b1) {
                    (true, true) => "imulq",
                    (true, false) => "imull",
                    (false, true) => "mulq",
                    (false, false) => "mull",
                };
                write!(f, "{op} {}", op1.expect("mulhi").format(d.b1))
            }
            InstructionKind::SignExtendData => f.write_str(if d.b1 { "cqo" } else { "cdq" }),
            InstructionKind::MovzxRmR => write!(
                f,
                "movzx.{} {}, {}",
                match d.u1 {
                    0 => ExtMode::BL,
                    1 => ExtMode::BQ,
                    2 => ExtMode::WL,
                    3 => ExtMode::WQ,
                    _ => ExtMode::LQ,
                },
                op1.expect("movzx src").format(true),
                op2.expect("movzx dst").format(true)
            ),
            InstructionKind::MovsxRmR => write!(
                f,
                "movsx.{} {}, {}",
                match d.u1 {
                    0 => ExtMode::BL,
                    1 => ExtMode::BQ,
                    2 => ExtMode::WL,
                    3 => ExtMode::WQ,
                    _ => ExtMode::LQ,
                },
                op1.expect("movsx src").format(true),
                op2.expect("movsx dst").format(true)
            ),
            InstructionKind::Mov64MR => write!(
                f,
                "movq {}, {}",
                op1.expect("mov64 src").format(true),
                op2.expect("mov64 dst").format(true)
            ),
            InstructionKind::MovRM => write!(
                f,
                "mov.{} {}, {}",
                match d.u1 {
                    1 => "b",
                    2 => "w",
                    4 => "l",
                    _ => "q",
                },
                op1.expect("movrm src").format(true),
                op2.expect("movrm dst").format(true)
            ),
            InstructionKind::Lea => write!(
                f,
                "lea {}, {}",
                op1.expect("lea src").format(true),
                op2.expect("lea dst").format(true)
            ),
            InstructionKind::CmpRmiR => write!(
                f,
                "{}{} {}, {}",
                if d.u1 != 0 { "cmp" } else { "test" },
                if d.b1 { "q" } else { "l" },
                op1.expect("cmp src").format(d.b1),
                op2.expect("cmp dst").format(d.b1)
            ),
            InstructionKind::Setcc => write!(
                f,
                "set{} {}",
                Cond::from_u8(d.u1 as u8),
                op2.expect("setcc dst").format(true)
            ),
            InstructionKind::Cmove => write!(
                f,
                "cmov{}{} {}, {}",
                Cond::from_u8(d.u1 as u8),
                if d.b1 { "q" } else { "l" },
                op1.expect("cmove src").format(d.b1),
                op2.expect("cmove dst").format(d.b1)
            ),
            InstructionKind::Push64 => write!(f, "pushq {}", op1.expect("push").format(true)),
            InstructionKind::Pop64 => write!(f, "popq {}", op1.expect("pop").format(true)),
            InstructionKind::Jmp => write!(f, "jmp {}", op1.expect("jmp").format(true)),
            InstructionKind::JmpIf => write!(
                f,
                "j{} {}",
                Cond::from_u8(d.u1 as u8),
                op1.expect("jmpif").format(true)
            ),
            InstructionKind::Call => write!(f, "call {}", d.u1),
            InstructionKind::CallIndirect => {
                write!(f, "callq *{}", op1.expect("callind").format(true))
            }
            InstructionKind::Xchg => write!(
                f,
                "xchg.{} {}, {}",
                match d.u1 {
                    1 => "b",
                    2 => "w",
                    4 => "l",
                    _ => "q",
                },
                op1.expect("xchg lhs").format(true),
                op2.expect("xchg rhs").format(true)
            ),
            InstructionKind::XmmUnaryRmR => write!(
                f,
                "{} {}, {}",
                SseOpcode::from_u64(d.u1),
                op1.expect("xmm src").format(false),
                op2.expect("xmm dst").format(false)
            ),
            InstructionKind::XmmMovRM => write!(
                f,
                "{} {}, {}",
                SseOpcode::from_u64(d.u1),
                op1.expect("xmmmov src").format(true),
                op2.expect("xmmmov dst").format(true)
            ),
            InstructionKind::XmmLoadConst => write!(
                f,
                "xmm_load_const ${:#x}, {}",
                d.u1,
                format_vreg_sized(
                    match op2.expect("xmm const dst") {
                        Operand::Reg(reg) => *reg,
                        _ => unreachable!(),
                    },
                    false
                )
            ),
        }
    }
}

impl Cond {
    pub(crate) const fn from_u8(raw: u8) -> Self {
        match raw {
            0 => Self::O,
            1 => Self::NO,
            2 => Self::B,
            3 => Self::NB,
            4 => Self::Z,
            5 => Self::NZ,
            6 => Self::BE,
            7 => Self::NBE,
            8 => Self::S,
            9 => Self::NS,
            10 => Self::P,
            11 => Self::NP,
            12 => Self::L,
            13 => Self::NL,
            14 => Self::LE,
            _ => Self::NLE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::operands::{AddressMode, Operand};
    use super::{AluRmiROpcode, Amd64Instr, UnaryRmROpcode};
    use crate::backend::{RegType, VReg};

    #[test]
    fn uses_can_be_assigned_like_go() {
        let vr0 = VReg(0).set_reg_type(RegType::Int);
        let vr1 = VReg(1).set_reg_type(RegType::Int);
        let inst = Amd64Instr::alu_rmi_r(AluRmiROpcode::Add, Operand::reg(vr0), vr1, false);
        let mut uses = Vec::new();
        inst.uses(&mut uses);
        inst.assign_use(0, VReg::from_real_reg(1, RegType::Int));
        inst.assign_use(1, VReg::from_real_reg(2, RegType::Int));
        assert_eq!(inst.to_string(), "add %eax, %ecx");
    }

    #[test]
    fn mem_uses_can_be_rewritten() {
        let vr0 = VReg(0).set_reg_type(RegType::Int);
        let vr1 = VReg(1).set_reg_type(RegType::Int);
        let inst = Amd64Instr::alu_rmi_r(
            AluRmiROpcode::Add,
            Operand::mem(AddressMode::imm_reg(123, vr0)),
            vr1,
            false,
        );
        inst.assign_use(0, VReg::from_real_reg(1, RegType::Int));
        inst.assign_use(1, VReg::from_real_reg(2, RegType::Int));
        assert_eq!(inst.to_string(), "add 123(%rax), %ecx");
    }

    #[test]
    fn unary_format_is_stable() {
        let src = VReg::from_real_reg(1, RegType::Int);
        let dst = VReg::from_real_reg(8, RegType::Int);
        let inst = Amd64Instr::unary_rm_r(UnaryRmROpcode::Bsr, Operand::reg(src), dst, true);
        assert_eq!(inst.to_string(), "bsrq %rax, %rdi");
    }
}
