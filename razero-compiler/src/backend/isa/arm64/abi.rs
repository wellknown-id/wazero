use crate::backend::{FunctionAbi, RealReg};
use crate::ssa::Signature;

use super::reg::{
    vreg_for_real_reg, ARG_RESULT_FLOAT_REGS, ARG_RESULT_INT_REGS, CALLEE_SAVED_FLOAT_REGS,
    CALLEE_SAVED_INT_REGS, CALLER_SAVED_FLOAT_REGS, CALLER_SAVED_INT_REGS,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Arm64Abi {
    pub function: FunctionAbi,
}

impl Arm64Abi {
    pub fn from_signature(sig: &Signature) -> Self {
        let mut function = FunctionAbi::default();
        function.init(sig, &ARG_RESULT_INT_REGS, &ARG_RESULT_FLOAT_REGS);
        Self { function }
    }

    pub const fn int_arg_result_regs() -> &'static [RealReg] {
        &ARG_RESULT_INT_REGS
    }

    pub const fn float_arg_result_regs() -> &'static [RealReg] {
        &ARG_RESULT_FLOAT_REGS
    }

    pub fn callee_saved_vregs() -> Vec<crate::backend::VReg> {
        CALLEE_SAVED_INT_REGS
            .into_iter()
            .chain(CALLEE_SAVED_FLOAT_REGS)
            .map(vreg_for_real_reg)
            .collect()
    }

    pub fn caller_saved_vregs() -> Vec<crate::backend::VReg> {
        CALLER_SAVED_INT_REGS
            .into_iter()
            .chain(CALLER_SAVED_FLOAT_REGS)
            .map(vreg_for_real_reg)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Arm64Abi;
    use crate::ssa::{Signature, SignatureId, Type};

    #[test]
    fn arm64_abi_assigns_registers_like_go() {
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
        let abi = Arm64Abi::from_signature(&sig);
        assert_eq!(abi.function.arg_int_real_regs, 3);
        assert_eq!(abi.function.arg_float_real_regs, 3);
        assert_eq!(abi.function.ret_int_real_regs, 2);
        assert_eq!(abi.function.ret_float_real_regs, 1);
        assert_eq!(abi.function.arg_stack_size, 0);
    }

    #[test]
    fn caller_and_callee_saved_sets_are_exposed() {
        assert!(!Arm64Abi::callee_saved_vregs().is_empty());
        assert!(!Arm64Abi::caller_saved_vregs().is_empty());
    }
}
