use core::fmt;

use crate::backend::{abi_info_from_u64, VReg};

use super::cond::{Cond, CondFlag, CondKind};
use super::lower_instr_operands::Imm12;
use super::lower_mem::AddressMode;
use super::reg::format_vreg_sized;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AluOp {
    Add,
    Sub,
    Orr,
    And,
    Eor,
    Mul,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadKind {
    ULoad,
    SLoad,
    FpuLoad,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreKind {
    Store,
    FpuStore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arm64Instr {
    Nop,
    Label(u32),
    Adr {
        rd: VReg,
        offset: i32,
    },
    MovZ {
        rd: VReg,
        imm: u16,
        shift: u8,
        bits: u8,
    },
    MovK {
        rd: VReg,
        imm: u16,
        shift: u8,
        bits: u8,
    },
    MovN {
        rd: VReg,
        imm: u16,
        shift: u8,
        bits: u8,
    },
    Move {
        rd: VReg,
        rn: VReg,
        bits: u8,
    },
    FpuMove {
        rd: VReg,
        rn: VReg,
        bits: u8,
    },
    AluRRR {
        op: AluOp,
        rd: VReg,
        rn: VReg,
        rm: VReg,
        bits: u8,
        set_flags: bool,
    },
    AluRRImm12 {
        op: AluOp,
        rd: VReg,
        rn: VReg,
        imm: Imm12,
        bits: u8,
        set_flags: bool,
    },
    Cmp {
        rn: VReg,
        rm: VReg,
        bits: u8,
    },
    Load {
        kind: LoadKind,
        rd: VReg,
        mem: AddressMode,
        bits: u8,
    },
    Store {
        kind: StoreKind,
        src: VReg,
        mem: AddressMode,
        bits: u8,
    },
    CSet {
        rd: VReg,
        flag: CondFlag,
    },
    Br {
        offset: i64,
        link: bool,
    },
    BrReg {
        rn: VReg,
        link: bool,
    },
    CondBr {
        cond: Cond,
        offset: i64,
        bits64: bool,
    },
    Call {
        offset: i64,
        abi: u64,
    },
    CallReg {
        rn: VReg,
        abi: u64,
        tail: bool,
    },
    Ret,
    Udf {
        imm: u16,
    },
    LoadConstBlockArg {
        dst: VReg,
        value: u64,
    },
    Raw32(u32),
}

impl Arm64Instr {
    pub fn defs_vec(&self) -> Vec<VReg> {
        let mut defs = Vec::new();
        self.defs(&mut defs);
        defs
    }

    pub fn uses_vec(&self) -> Vec<VReg> {
        let mut uses = Vec::new();
        self.uses(&mut uses);
        uses
    }

    pub fn defs(&self, out: &mut Vec<VReg>) {
        out.clear();
        match self {
            Self::Adr { rd, .. }
            | Self::MovZ { rd, .. }
            | Self::MovK { rd, .. }
            | Self::MovN { rd, .. }
            | Self::Move { rd, .. }
            | Self::FpuMove { rd, .. }
            | Self::AluRRR { rd, .. }
            | Self::AluRRImm12 { rd, .. }
            | Self::Load { rd, .. }
            | Self::CSet { rd, .. }
            | Self::LoadConstBlockArg { dst: rd, .. } => out.push(*rd),
            Self::Call { abi, .. } | Self::CallReg { abi, .. } => {
                let (_, _, ret_ints, ret_floats, _) = abi_info_from_u64(*abi);
                for index in 0..ret_ints as usize {
                    out.push(super::reg::vreg_for_real_reg(
                        super::reg::ARG_RESULT_INT_REGS[index],
                    ));
                }
                for index in 0..ret_floats as usize {
                    out.push(super::reg::vreg_for_real_reg(
                        super::reg::ARG_RESULT_FLOAT_REGS[index],
                    ));
                }
            }
            Self::Nop
            | Self::Label(_)
            | Self::Cmp { .. }
            | Self::Store { .. }
            | Self::Br { .. }
            | Self::BrReg { .. }
            | Self::CondBr { .. }
            | Self::Ret
            | Self::Udf { .. }
            | Self::Raw32(_) => {}
        }
    }

    pub fn uses(&self, out: &mut Vec<VReg>) {
        out.clear();
        match self {
            Self::Move { rn, .. } | Self::FpuMove { rn, .. } => out.push(*rn),
            Self::AluRRR { rn, rm, .. } | Self::Cmp { rn, rm, .. } => {
                out.push(*rn);
                out.push(*rm);
            }
            Self::AluRRImm12 { rn, .. } => out.push(*rn),
            Self::Load { mem, .. } => {
                out.push(mem.rn);
                if mem.rm.valid() {
                    out.push(mem.rm);
                }
            }
            Self::Store { src, mem, .. } => {
                out.push(*src);
                out.push(mem.rn);
                if mem.rm.valid() {
                    out.push(mem.rm);
                }
            }
            Self::BrReg { rn, .. } => out.push(*rn),
            Self::CondBr { cond, .. } => match cond.kind() {
                CondKind::RegisterZero | CondKind::RegisterNotZero => out.push(cond.register()),
                CondKind::CondFlagSet => {}
            },
            Self::CallReg { rn, abi, .. } => {
                out.push(*rn);
                let (arg_ints, arg_floats, _, _, _) = abi_info_from_u64(*abi);
                for index in 0..arg_ints as usize {
                    out.push(super::reg::vreg_for_real_reg(
                        super::reg::ARG_RESULT_INT_REGS[index],
                    ));
                }
                for index in 0..arg_floats as usize {
                    out.push(super::reg::vreg_for_real_reg(
                        super::reg::ARG_RESULT_FLOAT_REGS[index],
                    ));
                }
            }
            Self::Call { abi, .. } => {
                let (arg_ints, arg_floats, _, _, _) = abi_info_from_u64(*abi);
                for index in 0..arg_ints as usize {
                    out.push(super::reg::vreg_for_real_reg(
                        super::reg::ARG_RESULT_INT_REGS[index],
                    ));
                }
                for index in 0..arg_floats as usize {
                    out.push(super::reg::vreg_for_real_reg(
                        super::reg::ARG_RESULT_FLOAT_REGS[index],
                    ));
                }
            }
            Self::Nop
            | Self::Label(_)
            | Self::Adr { .. }
            | Self::MovZ { .. }
            | Self::MovK { .. }
            | Self::MovN { .. }
            | Self::CSet { .. }
            | Self::Br { .. }
            | Self::Ret
            | Self::Udf { .. }
            | Self::LoadConstBlockArg { .. }
            | Self::Raw32(_) => {}
        }
    }

    pub const fn is_copy_instr(&self) -> bool {
        matches!(self, Self::Move { .. } | Self::FpuMove { .. })
    }
}

impl fmt::Display for Arm64Instr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nop => f.write_str("nop"),
            Self::Label(label) => write!(f, "L{label}:"),
            Self::Adr { rd, offset } => {
                write!(f, "adr {}, #0x{offset:x}", format_vreg_sized(*rd, 64))
            }
            Self::MovZ {
                rd,
                imm,
                shift,
                bits,
            } => write!(
                f,
                "movz {}, #0x{:x}, lsl {}",
                format_vreg_sized(*rd, *bits),
                imm,
                shift
            ),
            Self::MovK {
                rd,
                imm,
                shift,
                bits,
            } => write!(
                f,
                "movk {}, #0x{:x}, lsl {}",
                format_vreg_sized(*rd, *bits),
                imm,
                shift
            ),
            Self::MovN {
                rd,
                imm,
                shift,
                bits,
            } => write!(
                f,
                "movn {}, #0x{:x}, lsl {}",
                format_vreg_sized(*rd, *bits),
                imm,
                shift
            ),
            Self::Move { rd, rn, bits } => write!(
                f,
                "mov {}, {}",
                format_vreg_sized(*rd, *bits),
                format_vreg_sized(*rn, *bits)
            ),
            Self::FpuMove { rd, rn, bits } => write!(
                f,
                "mov {}, {}",
                format_vreg_sized(*rd, *bits),
                format_vreg_sized(*rn, *bits)
            ),
            Self::AluRRR {
                op,
                rd,
                rn,
                rm,
                bits,
                ..
            } => write!(
                f,
                "{} {}, {}, {}",
                op,
                format_vreg_sized(*rd, *bits),
                format_vreg_sized(*rn, *bits),
                format_vreg_sized(*rm, *bits)
            ),
            Self::AluRRImm12 {
                op,
                rd,
                rn,
                imm,
                bits,
                ..
            } => write!(
                f,
                "{} {}, {}, #0x{:x}",
                op,
                format_vreg_sized(*rd, *bits),
                format_vreg_sized(*rn, *bits),
                imm.value()
            ),
            Self::Cmp { rn, rm, bits } => write!(
                f,
                "cmp {}, {}",
                format_vreg_sized(*rn, *bits),
                format_vreg_sized(*rm, *bits)
            ),
            Self::Load {
                kind,
                rd,
                mem,
                bits,
            } => write!(
                f,
                "{} {}, {}",
                kind,
                format_vreg_sized(*rd, *bits),
                mem.format(*bits)
            ),
            Self::Store {
                kind,
                src,
                mem,
                bits,
            } => write!(
                f,
                "{} {}, {}",
                kind,
                format_vreg_sized(*src, *bits),
                mem.format(*bits)
            ),
            Self::CSet { rd, flag } => write!(f, "cset {}, {flag}", format_vreg_sized(*rd, 64)),
            Self::Br { offset, link } => {
                if *link {
                    write!(f, "bl #0x{offset:x}")
                } else {
                    write!(f, "b #0x{offset:x}")
                }
            }
            Self::BrReg { rn, link } => {
                if *link {
                    write!(f, "blr {}", format_vreg_sized(*rn, 64))
                } else {
                    write!(f, "br {}", format_vreg_sized(*rn, 64))
                }
            }
            Self::CondBr { cond, offset, .. } => match cond.kind() {
                CondKind::RegisterZero => write!(
                    f,
                    "cbz {}, #0x{offset:x}",
                    format_vreg_sized(cond.register(), 64)
                ),
                CondKind::RegisterNotZero => {
                    write!(
                        f,
                        "cbnz {}, #0x{offset:x}",
                        format_vreg_sized(cond.register(), 64)
                    )
                }
                CondKind::CondFlagSet => write!(f, "b.{} #0x{offset:x}", cond.flag()),
            },
            Self::Call { offset, .. } => write!(f, "bl #0x{offset:x}"),
            Self::CallReg { rn, tail, .. } => {
                if *tail {
                    write!(f, "br {}", format_vreg_sized(*rn, 64))
                } else {
                    write!(f, "blr {}", format_vreg_sized(*rn, 64))
                }
            }
            Self::Ret => f.write_str("ret"),
            Self::Udf { imm } => write!(f, "udf #0x{imm:x}"),
            Self::LoadConstBlockArg { dst, value } => {
                write!(
                    f,
                    "load-const-block-arg {}, #0x{value:x}",
                    format_vreg_sized(*dst, 64)
                )
            }
            Self::Raw32(word) => write!(f, ".word 0x{word:08x}"),
        }
    }
}

impl fmt::Display for AluOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Orr => "orr",
            Self::And => "and",
            Self::Eor => "eor",
            Self::Mul => "mul",
        })
    }
}

impl fmt::Display for LoadKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ULoad => "ldr",
            Self::SLoad => "ldrsw",
            Self::FpuLoad => "ldr",
        })
    }
}

impl fmt::Display for StoreKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Store => "str",
            Self::FpuStore => "str",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{AluOp, Arm64Instr, LoadKind, StoreKind};
    use crate::backend::isa::arm64::cond::{Cond, CondFlag};
    use crate::backend::isa::arm64::lower_mem::AddressMode;
    use crate::backend::isa::arm64::reg::{vreg_for_real_reg, V0, X0, X1, X2};
    use crate::backend::{FunctionAbi, RegType};
    use crate::ssa::{Signature, SignatureId, Type};

    #[test]
    fn instruction_display_is_stable() {
        let x0 = vreg_for_real_reg(X0);
        let x1 = vreg_for_real_reg(X1);
        let x2 = vreg_for_real_reg(X2);
        let v0 = vreg_for_real_reg(V0);
        assert_eq!(
            Arm64Instr::AluRRR {
                op: AluOp::Add,
                rd: x0,
                rn: x1,
                rm: x2,
                bits: 64,
                set_flags: false
            }
            .to_string(),
            "add x0, x1, x2"
        );
        assert_eq!(
            Arm64Instr::Load {
                kind: LoadKind::ULoad,
                rd: x0,
                mem: AddressMode::reg_unsigned_imm12(x1, 8),
                bits: 64
            }
            .to_string(),
            "ldr x0, [x1, #0x8]"
        );
        assert_eq!(
            Arm64Instr::Store {
                kind: StoreKind::FpuStore,
                src: v0,
                mem: AddressMode::reg_unsigned_imm12(x1, 16),
                bits: 128
            }
            .to_string(),
            "str q0, [x1, #0x10]"
        );
        assert_eq!(
            Arm64Instr::CondBr {
                cond: Cond::from_flag(CondFlag::Ge),
                offset: 12,
                bits64: true
            }
            .to_string(),
            "b.ge #0xc"
        );
    }

    #[test]
    fn call_defs_and_uses_follow_backend_abi_info() {
        let sig = Signature::new(
            SignatureId(0),
            vec![Type::I64, Type::F64],
            vec![Type::I64, Type::F64],
        );
        let mut abi = FunctionAbi::default();
        abi.init(
            &sig,
            &super::super::reg::ARG_RESULT_INT_REGS,
            &super::super::reg::ARG_RESULT_FLOAT_REGS,
        );
        let call = Arm64Instr::Call {
            offset: 0,
            abi: abi.abi_info_as_u64(),
        };
        assert_eq!(call.uses_vec().len(), 2);
        assert_eq!(call.defs_vec().len(), 2);
    }

    #[test]
    fn copy_detection_matches_move_instructions() {
        let x0 = vreg_for_real_reg(X0);
        let x1 = vreg_for_real_reg(X1);
        assert!(Arm64Instr::Move {
            rd: x0,
            rn: x1,
            bits: 64
        }
        .is_copy_instr());
        assert!(Arm64Instr::FpuMove {
            rd: x0.set_reg_type(RegType::Float),
            rn: x1.set_reg_type(RegType::Float),
            bits: 128
        }
        .is_copy_instr());
        assert!(!Arm64Instr::Ret.is_copy_instr());
    }
}
