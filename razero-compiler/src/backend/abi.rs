use core::fmt;

use crate::ssa::{Signature, Type};

use super::{RealReg, RegType, VReg};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum AbiArgKind {
    #[default]
    Reg = 0,
    Stack,
}

impl fmt::Display for AbiArgKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Reg => "reg",
            Self::Stack => "stack",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AbiArg {
    pub index: usize,
    pub kind: AbiArgKind,
    pub reg: VReg,
    pub offset: i64,
    pub ty: Type,
}

impl fmt::Display for AbiArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "args[{}]: {}", self.index, self.kind)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FunctionAbi {
    pub initialized: bool,
    pub args: Vec<AbiArg>,
    pub rets: Vec<AbiArg>,
    pub arg_stack_size: i64,
    pub ret_stack_size: i64,
    pub arg_int_real_regs: u8,
    pub arg_float_real_regs: u8,
    pub ret_int_real_regs: u8,
    pub ret_float_real_regs: u8,
}

impl FunctionAbi {
    pub fn init(
        &mut self,
        sig: &Signature,
        arg_result_ints: &[RealReg],
        arg_result_floats: &[RealReg],
    ) {
        self.rets.resize(sig.results.len(), AbiArg::default());
        self.ret_stack_size = Self::set_abi_args(
            &mut self.rets,
            &sig.results,
            arg_result_ints,
            arg_result_floats,
        );

        self.args.resize(sig.params.len(), AbiArg::default());
        self.arg_stack_size = Self::set_abi_args(
            &mut self.args,
            &sig.params,
            arg_result_ints,
            arg_result_floats,
        );

        self.arg_int_real_regs = 0;
        self.arg_float_real_regs = 0;
        self.ret_int_real_regs = 0;
        self.ret_float_real_regs = 0;

        for ret in &self.rets {
            if matches!(ret.kind, AbiArgKind::Reg) {
                if ret.ty.is_int() {
                    self.ret_int_real_regs += 1;
                } else {
                    self.ret_float_real_regs += 1;
                }
            }
        }

        for arg in &self.args {
            if matches!(arg.kind, AbiArgKind::Reg) {
                if arg.ty.is_int() {
                    self.arg_int_real_regs += 1;
                } else {
                    self.arg_float_real_regs += 1;
                }
            }
        }

        self.initialized = true;
    }

    pub fn aligned_arg_result_stack_slot_size(&self) -> u32 {
        let mut stack_slot_size = self.ret_stack_size + self.arg_stack_size;
        stack_slot_size = (stack_slot_size + 15) & !15;
        assert!(
            stack_slot_size <= u32::MAX as i64,
            "ABI stack slot size overflow"
        );
        stack_slot_size as u32
    }

    pub fn abi_info_as_u64(&self) -> u64 {
        ((self.arg_int_real_regs as u64) << 56)
            | ((self.arg_float_real_regs as u64) << 48)
            | ((self.ret_int_real_regs as u64) << 40)
            | ((self.ret_float_real_regs as u64) << 32)
            | self.aligned_arg_result_stack_slot_size() as u64
    }

    fn set_abi_args(
        dst: &mut [AbiArg],
        types: &[Type],
        ints: &[RealReg],
        floats: &[RealReg],
    ) -> i64 {
        let mut stack_offset = 0;
        let mut int_param_index = 0usize;
        let mut float_param_index = 0usize;

        for (index, ty) in types.iter().copied().enumerate() {
            let arg = &mut dst[index];
            arg.index = index;
            arg.ty = ty;
            if ty.is_int() {
                if int_param_index >= ints.len() {
                    arg.kind = AbiArgKind::Stack;
                    arg.reg = VReg::INVALID;
                    arg.offset = stack_offset;
                    stack_offset += 8;
                } else {
                    arg.kind = AbiArgKind::Reg;
                    arg.reg = VReg::from_real_reg(ints[int_param_index], RegType::Int);
                    arg.offset = 0;
                    int_param_index += 1;
                }
            } else if float_param_index >= floats.len() {
                arg.kind = AbiArgKind::Stack;
                arg.reg = VReg::INVALID;
                arg.offset = stack_offset;
                stack_offset += if ty.bits() == 128 { 16 } else { 8 };
            } else {
                arg.kind = AbiArgKind::Reg;
                arg.reg = VReg::from_real_reg(floats[float_param_index], RegType::Float);
                arg.offset = 0;
                float_param_index += 1;
            }
        }

        stack_offset
    }
}

pub const fn abi_info_from_u64(info: u64) -> (u8, u8, u8, u8, u32) {
    (
        (info >> 56) as u8,
        (info >> 48) as u8,
        (info >> 40) as u8,
        (info >> 32) as u8,
        info as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::{abi_info_from_u64, AbiArg, AbiArgKind, FunctionAbi};
    use crate::backend::VReg;
    use crate::ssa::{Signature, SignatureId, Type};

    const X0: u8 = 1;
    const X1: u8 = 2;
    const X2: u8 = 3;
    const V0: u8 = 11;
    const V1: u8 = 12;

    #[test]
    fn function_abi_assigns_regs_and_stack_like_go() {
        let sig = Signature::new(
            SignatureId(0),
            vec![
                Type::I32,
                Type::F32,
                Type::I64,
                Type::F64,
                Type::I32,
                Type::V128,
            ],
            vec![Type::I64, Type::F64, Type::I32],
        );

        let mut abi = FunctionAbi::default();
        abi.init(&sig, &[X0, X1, X2], &[V0, V1]);

        assert_eq!(
            abi.args,
            vec![
                AbiArg {
                    index: 0,
                    kind: AbiArgKind::Reg,
                    reg: VReg::from_real_reg(X0, crate::backend::RegType::Int),
                    offset: 0,
                    ty: Type::I32,
                },
                AbiArg {
                    index: 1,
                    kind: AbiArgKind::Reg,
                    reg: VReg::from_real_reg(V0, crate::backend::RegType::Float),
                    offset: 0,
                    ty: Type::F32,
                },
                AbiArg {
                    index: 2,
                    kind: AbiArgKind::Reg,
                    reg: VReg::from_real_reg(X1, crate::backend::RegType::Int),
                    offset: 0,
                    ty: Type::I64,
                },
                AbiArg {
                    index: 3,
                    kind: AbiArgKind::Reg,
                    reg: VReg::from_real_reg(V1, crate::backend::RegType::Float),
                    offset: 0,
                    ty: Type::F64,
                },
                AbiArg {
                    index: 4,
                    kind: AbiArgKind::Reg,
                    reg: VReg::from_real_reg(X2, crate::backend::RegType::Int),
                    offset: 0,
                    ty: Type::I32,
                },
                AbiArg {
                    index: 5,
                    kind: AbiArgKind::Stack,
                    reg: VReg::INVALID,
                    offset: 0,
                    ty: Type::V128,
                },
            ]
        );
        assert_eq!(abi.arg_stack_size, 16);
        assert_eq!(abi.ret_stack_size, 0);
        assert_eq!(abi.arg_int_real_regs, 3);
        assert_eq!(abi.arg_float_real_regs, 2);
        assert_eq!(abi.ret_int_real_regs, 2);
        assert_eq!(abi.ret_float_real_regs, 1);
    }

    #[test]
    fn abi_info_round_trips() {
        let mut abi = FunctionAbi::default();
        abi.arg_int_real_regs = 2;
        abi.arg_float_real_regs = 3;
        abi.ret_int_real_regs = 4;
        abi.ret_float_real_regs = 5;
        abi.arg_stack_size = 24;
        abi.ret_stack_size = 8;

        let info = abi.abi_info_as_u64();
        assert_eq!(abi_info_from_u64(info), (2, 3, 4, 5, 32));
    }
}
