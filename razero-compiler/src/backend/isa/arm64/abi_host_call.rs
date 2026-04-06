use crate::ssa::Signature;
use crate::wazevoapi::ExitCode;

use super::instr::Arm64Instr;
use super::instr_encoding::encode_instruction;
use super::reg::{vreg_for_real_reg, X0};

pub fn compile_host_function_trampoline(
    exit_code: ExitCode,
    _sig: &Signature,
    _need_module_context_ptr: bool,
) -> Vec<u8> {
    let x0 = vreg_for_real_reg(X0);
    let seq = [
        Arm64Instr::MovZ {
            rd: x0,
            imm: exit_code.raw() as u16,
            shift: 0,
            bits: 64,
        },
        Arm64Instr::Ret,
    ];
    let mut buf = Vec::new();
    for instr in seq {
        for word in encode_instruction(&instr).expect("host trampoline subset should encode") {
            buf.extend_from_slice(&word.to_le_bytes());
        }
    }
    buf
}

pub fn compile_stack_grow_call_sequence() -> Vec<u8> {
    let mut buf = Vec::new();
    for word in encode_instruction(&Arm64Instr::Ret).expect("ret must encode") {
        buf.extend_from_slice(&word.to_le_bytes());
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::{compile_host_function_trampoline, compile_stack_grow_call_sequence};
    use crate::ssa::{Signature, SignatureId};
    use crate::wazevoapi::ExitCode;

    #[test]
    fn host_function_trampoline_emits_ret_sequence() {
        let sig = Signature::new(SignatureId(0), vec![], vec![]);
        let bytes = compile_host_function_trampoline(ExitCode::GROW_STACK, &sig, false);
        assert_eq!(bytes.len(), 8);
    }

    #[test]
    fn stack_grow_sequence_is_minimal_scaffold() {
        assert_eq!(compile_stack_grow_call_sequence().len(), 4);
    }
}
