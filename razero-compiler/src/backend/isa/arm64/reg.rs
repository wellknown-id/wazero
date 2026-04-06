use core::fmt;

use crate::backend::{RealReg, RegType, VReg};

pub const X0: RealReg = 1;
pub const X1: RealReg = 2;
pub const X2: RealReg = 3;
pub const X3: RealReg = 4;
pub const X4: RealReg = 5;
pub const X5: RealReg = 6;
pub const X6: RealReg = 7;
pub const X7: RealReg = 8;
pub const X8: RealReg = 9;
pub const X9: RealReg = 10;
pub const X10: RealReg = 11;
pub const X11: RealReg = 12;
pub const X12: RealReg = 13;
pub const X13: RealReg = 14;
pub const X14: RealReg = 15;
pub const X15: RealReg = 16;
pub const X16: RealReg = 17;
pub const X17: RealReg = 18;
pub const X18: RealReg = 19;
pub const X19: RealReg = 20;
pub const X20: RealReg = 21;
pub const X21: RealReg = 22;
pub const X22: RealReg = 23;
pub const X23: RealReg = 24;
pub const X24: RealReg = 25;
pub const X25: RealReg = 26;
pub const X26: RealReg = 27;
pub const X27: RealReg = 28;
pub const X28: RealReg = 29;
pub const X29: RealReg = 30;
pub const X30: RealReg = 31;

pub const V0: RealReg = 32;
pub const V1: RealReg = 33;
pub const V2: RealReg = 34;
pub const V3: RealReg = 35;
pub const V4: RealReg = 36;
pub const V5: RealReg = 37;
pub const V6: RealReg = 38;
pub const V7: RealReg = 39;
pub const V8: RealReg = 40;
pub const V9: RealReg = 41;
pub const V10: RealReg = 42;
pub const V11: RealReg = 43;
pub const V12: RealReg = 44;
pub const V13: RealReg = 45;
pub const V14: RealReg = 46;
pub const V15: RealReg = 47;
pub const V16: RealReg = 48;
pub const V17: RealReg = 49;
pub const V18: RealReg = 50;
pub const V19: RealReg = 51;
pub const V20: RealReg = 52;
pub const V21: RealReg = 53;
pub const V22: RealReg = 54;
pub const V23: RealReg = 55;
pub const V24: RealReg = 56;
pub const V25: RealReg = 57;
pub const V26: RealReg = 58;
pub const V27: RealReg = 59;
pub const V28: RealReg = 60;
pub const V29: RealReg = 61;
pub const V30: RealReg = 62;
pub const V31: RealReg = 63;

pub const XZR: RealReg = 64;
pub const SP: RealReg = 65;
pub const LR: RealReg = X30;
pub const FP: RealReg = X29;
pub const TMP: RealReg = X27;

pub const ARG_RESULT_INT_REGS: [RealReg; 8] = [X0, X1, X2, X3, X4, X5, X6, X7];
pub const ARG_RESULT_FLOAT_REGS: [RealReg; 8] = [V0, V1, V2, V3, V4, V5, V6, V7];

pub const CALLEE_SAVED_INT_REGS: [RealReg; 9] = [X19, X20, X21, X22, X23, X24, X25, X26, X28];
pub const CALLEE_SAVED_FLOAT_REGS: [RealReg; 14] = [
    V18, V19, V20, V21, V22, V23, V24, V25, V26, V27, V28, V29, V30, V31,
];
pub const CALLER_SAVED_INT_REGS: [RealReg; 20] = [
    X0, X1, X2, X3, X4, X5, X6, X7, X8, X9, X10, X11, X12, X13, X14, X15, X16, X17, X29, X30,
];
pub const CALLER_SAVED_FLOAT_REGS: [RealReg; 18] = [
    V0, V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11, V12, V13, V14, V15, V16, V17,
];

pub const ALLOCATABLE_INT_REGS: [RealReg; 27] = [
    X8, X9, X10, X11, X12, X13, X14, X15, X16, X17, X19, X20, X21, X22, X23, X24, X26, X29,
    X30, X7, X6, X5, X4, X3, X2, X1, X0,
];
pub const ALLOCATABLE_FLOAT_REGS: [RealReg; 31] = [
    V8, V9, V10, V11, V12, V13, V14, V15, V16, V17, V18, V19, V20, V21, V22, V23, V24, V25,
    V26, V27, V28, V29, V30, V7, V6, V5, V4, V3, V2, V1, V0,
];

pub fn reg_type(reg: RealReg) -> RegType {
    if reg < V0 || reg == XZR || reg == SP {
        RegType::Int
    } else {
        RegType::Float
    }
}

pub fn vreg_for_real_reg(reg: RealReg) -> VReg {
    VReg::from_real_reg(reg, reg_type(reg))
}

pub fn reg_name(reg: RealReg) -> &'static str {
    match reg {
        X0 => "x0",
        X1 => "x1",
        X2 => "x2",
        X3 => "x3",
        X4 => "x4",
        X5 => "x5",
        X6 => "x6",
        X7 => "x7",
        X8 => "x8",
        X9 => "x9",
        X10 => "x10",
        X11 => "x11",
        X12 => "x12",
        X13 => "x13",
        X14 => "x14",
        X15 => "x15",
        X16 => "x16",
        X17 => "x17",
        X18 => "x18",
        X19 => "x19",
        X20 => "x20",
        X21 => "x21",
        X22 => "x22",
        X23 => "x23",
        X24 => "x24",
        X25 => "x25",
        X26 => "x26",
        X27 => "x27",
        X28 => "x28",
        X29 => "x29",
        X30 => "x30",
        XZR => "xzr",
        SP => "sp",
        V0 => "v0",
        V1 => "v1",
        V2 => "v2",
        V3 => "v3",
        V4 => "v4",
        V5 => "v5",
        V6 => "v6",
        V7 => "v7",
        V8 => "v8",
        V9 => "v9",
        V10 => "v10",
        V11 => "v11",
        V12 => "v12",
        V13 => "v13",
        V14 => "v14",
        V15 => "v15",
        V16 => "v16",
        V17 => "v17",
        V18 => "v18",
        V19 => "v19",
        V20 => "v20",
        V21 => "v21",
        V22 => "v22",
        V23 => "v23",
        V24 => "v24",
        V25 => "v25",
        V26 => "v26",
        V27 => "v27",
        V28 => "v28",
        V29 => "v29",
        V30 => "v30",
        V31 => "v31",
        _ => panic!("unknown arm64 register {reg}"),
    }
}

pub fn encoding_reg_number(reg: RealReg) -> u32 {
    match reg {
        X0 | V0 => 0,
        X1 | V1 => 1,
        X2 | V2 => 2,
        X3 | V3 => 3,
        X4 | V4 => 4,
        X5 | V5 => 5,
        X6 | V6 => 6,
        X7 | V7 => 7,
        X8 | V8 => 8,
        X9 | V9 => 9,
        X10 | V10 => 10,
        X11 | V11 => 11,
        X12 | V12 => 12,
        X13 | V13 => 13,
        X14 | V14 => 14,
        X15 | V15 => 15,
        X16 | V16 => 16,
        X17 | V17 => 17,
        X18 | V18 => 18,
        X19 | V19 => 19,
        X20 | V20 => 20,
        X21 | V21 => 21,
        X22 | V22 => 22,
        X23 | V23 => 23,
        X24 | V24 => 24,
        X25 | V25 => 25,
        X26 | V26 => 26,
        X27 | V27 => 27,
        X28 | V28 => 28,
        X29 | V29 => 29,
        X30 | V30 => 30,
        V31 | XZR | SP => 31,
        _ => panic!("unknown arm64 register {reg}"),
    }
}

pub fn format_vreg_sized(reg: VReg, size: u8) -> String {
    let is_real = reg.is_real_reg();
    let base = if is_real {
        reg_name(reg.real_reg()).to_string()
    } else {
        match reg.reg_type() {
            RegType::Int => format!("x{}?", reg.id()),
            RegType::Float => format!("v{}?", reg.id()),
            RegType::Invalid => panic!("invalid register type"),
        }
    };

    match (base.as_bytes()[0], size) {
        (b'x', 32) => base.replacen('x', "w", 1),
        (b'x', 64) => base,
        (b's', 64) => base,
        (b'v', 32) => base.replacen('v', "s", 1),
        (b'v', 64) => base.replacen('v', "d", 1),
        (b'v', 128) => base.replacen('v', "q", 1),
        _ => panic!("invalid arm64 register formatting size {size}"),
    }
}

pub fn format_vreg_vec(reg: VReg, arrangement: impl fmt::Display, index: Option<u8>) -> String {
    let id = if reg.is_real_reg() {
        reg_name(reg.real_reg()).to_string()
    } else {
        format!("v{}?", reg.id())
    };
    match index {
        Some(index) => format!("{id}.{arrangement}[{index}]"),
        None => format!("{id}.{arrangement}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        encoding_reg_number, format_vreg_sized, reg_name, reg_type, vreg_for_real_reg,
        ARG_RESULT_FLOAT_REGS, ARG_RESULT_INT_REGS, FP, LR, SP, TMP, V0, V31, X0, X27, XZR,
    };
    use crate::backend::{RegType, VReg};

    #[test]
    fn arm64_register_identity_matches_go_layout() {
        assert_eq!(X0, 1);
        assert_eq!(V0, 32);
        assert_eq!(V31, 63);
        assert_eq!(XZR, 64);
        assert_eq!(SP, 65);
        assert_eq!(LR, 31);
        assert_eq!(FP, 30);
        assert_eq!(TMP, X27);
    }

    #[test]
    fn argument_and_result_register_sets_match_go_abi() {
        assert_eq!(ARG_RESULT_INT_REGS, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(ARG_RESULT_FLOAT_REGS, [32, 33, 34, 35, 36, 37, 38, 39]);
    }

    #[test]
    fn formatting_follows_arm64_register_classes() {
        assert_eq!(format_vreg_sized(vreg_for_real_reg(X0), 64), "x0");
        assert_eq!(format_vreg_sized(vreg_for_real_reg(X0), 32), "w0");
        assert_eq!(format_vreg_sized(vreg_for_real_reg(V0), 32), "s0");
        assert_eq!(format_vreg_sized(vreg_for_real_reg(V0), 64), "d0");
        assert_eq!(format_vreg_sized(vreg_for_real_reg(V0), 128), "q0");

        let int_vreg = VReg(128).set_reg_type(RegType::Int);
        let float_vreg = VReg(129).set_reg_type(RegType::Float);
        assert_eq!(format_vreg_sized(int_vreg, 32), "w128?");
        assert_eq!(format_vreg_sized(float_vreg, 128), "q129?");
    }

    #[test]
    fn encoding_numbers_match_hardware_register_indices() {
        assert_eq!(encoding_reg_number(X0), 0);
        assert_eq!(encoding_reg_number(XZR), 31);
        assert_eq!(encoding_reg_number(SP), 31);
        assert_eq!(encoding_reg_number(V31), 31);
    }

    #[test]
    fn register_names_and_types_are_stable() {
        assert_eq!(reg_name(X0), "x0");
        assert_eq!(reg_name(V0), "v0");
        assert_eq!(reg_type(X0), RegType::Int);
        assert_eq!(reg_type(V0), RegType::Float);
    }
}
