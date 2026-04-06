use crate::backend::{RealReg, RegType, VReg};

pub const RAX: RealReg = 1;
pub const RCX: RealReg = 2;
pub const RDX: RealReg = 3;
pub const RBX: RealReg = 4;
pub const RSP: RealReg = 5;
pub const RBP: RealReg = 6;
pub const RSI: RealReg = 7;
pub const RDI: RealReg = 8;
pub const R8: RealReg = 9;
pub const R9: RealReg = 10;
pub const R10: RealReg = 11;
pub const R11: RealReg = 12;
pub const R12: RealReg = 13;
pub const R13: RealReg = 14;
pub const R14: RealReg = 15;
pub const R15: RealReg = 16;

pub const XMM0: RealReg = 17;
pub const XMM1: RealReg = 18;
pub const XMM2: RealReg = 19;
pub const XMM3: RealReg = 20;
pub const XMM4: RealReg = 21;
pub const XMM5: RealReg = 22;
pub const XMM6: RealReg = 23;
pub const XMM7: RealReg = 24;
pub const XMM8: RealReg = 25;
pub const XMM9: RealReg = 26;
pub const XMM10: RealReg = 27;
pub const XMM11: RealReg = 28;
pub const XMM12: RealReg = 29;
pub const XMM13: RealReg = 30;
pub const XMM14: RealReg = 31;
pub const XMM15: RealReg = 32;

const REG_NAMES: [&str; 33] = [
    "invalid", "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11",
    "r12", "r13", "r14", "r15", "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5", "xmm6", "xmm7",
    "xmm8", "xmm9", "xmm10", "xmm11", "xmm12", "xmm13", "xmm14", "xmm15",
];

pub const INT_ARG_RESULT_REGS: [RealReg; 9] = [RAX, RBX, RCX, RDI, RSI, R8, R9, R10, R11];
pub const FLOAT_ARG_RESULT_REGS: [RealReg; 8] = [XMM0, XMM1, XMM2, XMM3, XMM4, XMM5, XMM6, XMM7];

#[inline]
pub fn vreg_for_real_reg(r: RealReg) -> VReg {
    VReg::from_real_reg(r, real_reg_type(r))
}

pub fn real_reg_name(r: RealReg) -> String {
    REG_NAMES.get(r as usize).unwrap_or(&"invalid").to_string()
}

pub fn real_reg_type(r: RealReg) -> RegType {
    if r >= XMM0 {
        RegType::Float
    } else if r >= RAX {
        RegType::Int
    } else {
        RegType::Invalid
    }
}

pub fn format_vreg_sized(r: VReg, is_64: bool) -> String {
    if r.is_real_reg() {
        let rr = r.real_reg();
        let name = real_reg_name(rr);
        if r.reg_type() == RegType::Int {
            if rr <= RDI {
                if is_64 {
                    format!("%{name}")
                } else {
                    format!("%e{}", &name[1..])
                }
            } else if is_64 {
                format!("%{name}")
            } else {
                format!("%{name}d")
            }
        } else {
            format!("%{name}")
        }
    } else if r.reg_type() == RegType::Int {
        if is_64 {
            format!("%r{}?", r.id())
        } else {
            format!("%r{}d?", r.id())
        }
    } else {
        format!("%xmm{}?", r.id())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        format_vreg_sized, real_reg_name, real_reg_type, vreg_for_real_reg, R15, RAX, XMM0, XMM15,
    };
    use crate::backend::{RegType, VReg};

    #[test]
    fn format_matches_go_conventions() {
        assert_eq!(format_vreg_sized(vreg_for_real_reg(RAX), true), "%rax");
        assert_eq!(format_vreg_sized(vreg_for_real_reg(RAX), false), "%eax");
        assert_eq!(format_vreg_sized(vreg_for_real_reg(R15), true), "%r15");
        assert_eq!(format_vreg_sized(vreg_for_real_reg(R15), false), "%r15d");
        assert_eq!(format_vreg_sized(vreg_for_real_reg(XMM0), true), "%xmm0");
        assert_eq!(format_vreg_sized(vreg_for_real_reg(XMM15), false), "%xmm15");
        assert_eq!(
            format_vreg_sized(VReg(5555).set_reg_type(RegType::Int), false),
            "%r5555d?"
        );
        assert_eq!(
            format_vreg_sized(VReg(123).set_reg_type(RegType::Float), true),
            "%xmm123?"
        );
    }

    #[test]
    fn real_register_metadata_is_stable() {
        assert_eq!(real_reg_name(RAX), "rax");
        assert_eq!(real_reg_name(XMM15), "xmm15");
        assert_eq!(real_reg_type(RAX), RegType::Int);
        assert_eq!(real_reg_type(XMM15), RegType::Float);
    }
}
