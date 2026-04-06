use crate::backend::go_function_call_required_stack_size;
use crate::ssa::Signature;
use crate::wazevoapi::ExitCode;

use super::instr::{AluRmiROpcode, Amd64Instr};
use super::machine::Amd64Machine;
use super::machine_pro_epi_logue::{append_epilogue, append_prologue};
use super::operands::Operand;
use super::reg::{vreg_for_real_reg, R12, RSP};

pub fn compile_host_function_trampoline(
    exit_code: ExitCode,
    sig: &Signature,
    need_module_context_ptr: bool,
) -> Vec<u8> {
    let arg_begin = if need_module_context_ptr { 2 } else { 1 };
    let (aligned, unaligned) = go_function_call_required_stack_size(sig, arg_begin);
    let mut machine = Amd64Machine::new();
    crate::backend::machine::Machine::start_lowering_function(
        &mut machine,
        crate::ssa::BasicBlockId(0),
    );
    crate::backend::machine::Machine::start_block(&mut machine, crate::ssa::BasicBlockId(0));
    append_prologue(&mut machine);
    if aligned > 0 {
        machine.push(Amd64Instr::alu_rmi_r(
            AluRmiROpcode::Sub,
            Operand::imm32(aligned as u32),
            vreg_for_real_reg(RSP),
            true,
        ));
    }
    machine.push(Amd64Instr::push64(Operand::imm32(unaligned as u32)));
    machine.push(Amd64Instr::imm(
        vreg_for_real_reg(R12),
        exit_code.raw() as u64,
        false,
    ));
    if aligned > 0 {
        machine.push(Amd64Instr::alu_rmi_r(
            AluRmiROpcode::Add,
            Operand::imm32(aligned as u32),
            vreg_for_real_reg(RSP),
            true,
        ));
    }
    append_epilogue(&mut machine);
    machine.encode_all().unwrap()
}

#[cfg(test)]
mod tests {
    use super::compile_host_function_trampoline;
    use crate::ssa::{Signature, SignatureId, Type};
    use crate::wazevoapi::ExitCode;

    #[test]
    fn host_call_trampoline_emits_exit_code_setup() {
        let sig = Signature::new(SignatureId(0), vec![Type::I64, Type::V128], vec![Type::I64]);
        let code = compile_host_function_trampoline(ExitCode::CALL_GO_FUNCTION, &sig, false);
        assert!(!code.is_empty());
        assert!(code.ends_with(&[0xC3]));
    }
}
