use crate::backend::machine::BackendError;
use crate::ssa::Signature;

use super::abi::amd64_function_abi;
use super::instr::{AluRmiROpcode, Amd64Instr};
use super::machine::Amd64Machine;
use super::machine_pro_epi_logue::{append_epilogue, append_prologue};
use super::operands::Operand;
use super::reg::{vreg_for_real_reg, R13, R14, R15, RAX, RDX, RSP};

pub fn compile_entry_preamble(sig: &Signature, use_host_stack: bool) -> Vec<u8> {
    let abi = amd64_function_abi(sig);
    let mut machine = Amd64Machine::new();
    crate::backend::machine::Machine::start_lowering_function(
        &mut machine,
        crate::ssa::BasicBlockId(0),
    );
    crate::backend::machine::Machine::start_block(&mut machine, crate::ssa::BasicBlockId(0));
    append_prologue(&mut machine);
    machine.push(Amd64Instr::mov_rr(
        vreg_for_real_reg(RAX),
        vreg_for_real_reg(RDX),
        true,
    ));
    if !use_host_stack {
        machine.push(Amd64Instr::mov_rr(
            vreg_for_real_reg(R13),
            vreg_for_real_reg(RSP),
            true,
        ));
    }
    let stack_size = abi.aligned_arg_result_stack_slot_size();
    if stack_size > 0 {
        machine.push(Amd64Instr::alu_rmi_r(
            AluRmiROpcode::Sub,
            Operand::imm32(stack_size),
            vreg_for_real_reg(RSP),
            true,
        ));
    }
    machine.push(Amd64Instr::mov_rr(
        vreg_for_real_reg(RDX),
        vreg_for_real_reg(R15),
        true,
    ));
    machine.push(Amd64Instr::call_indirect(
        Operand::reg(vreg_for_real_reg(R14)),
        abi.abi_info_as_u64(),
    ));
    append_epilogue(&mut machine);
    machine
        .encode_all()
        .unwrap_or_else(|e: BackendError| panic!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::compile_entry_preamble;
    use crate::ssa::{Signature, SignatureId, Type};

    #[test]
    fn preamble_emits_stack_setup_and_call() {
        let sig = Signature::new(SignatureId(0), vec![Type::I64, Type::I64], vec![Type::I64]);
        let code = compile_entry_preamble(&sig, false);
        assert!(!code.is_empty());
        assert!(code.ends_with(&[0xC3]));
    }
}
