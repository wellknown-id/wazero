use crate::backend::machine::BackendError;
use crate::backend::AbiArgKind;
use crate::ssa::Signature;

use super::abi::amd64_function_abi;
use super::instr::{AluRmiROpcode, Amd64Instr};
use super::machine::Amd64Machine;
use super::machine_pro_epi_logue::{append_epilogue, append_prologue};
use super::machine_vec::SseOpcode;
use super::operands::{AddressMode, Operand};
use super::reg::{vreg_for_real_reg, R12, R13, R14, R15, RAX, RDX, RSP, XMM15};

fn stack_slot_size(ty: crate::ssa::Type) -> u32 {
    if ty.bits() == 128 {
        16
    } else {
        8
    }
}

fn sse_move_opcode(ty: crate::ssa::Type) -> SseOpcode {
    match ty {
        crate::ssa::Type::F32 => SseOpcode::Movss,
        crate::ssa::Type::F64 => SseOpcode::Movsd,
        crate::ssa::Type::V128 => SseOpcode::Movdqu,
        crate::ssa::Type::I32 | crate::ssa::Type::I64 | crate::ssa::Type::Invalid => {
            unreachable!("SSE move requested for non-float type")
        }
    }
}

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
    let mut param_result_offset = 0u32;
    for arg in &abi.args {
        let slice = Operand::mem(AddressMode::imm_reg(
            param_result_offset,
            vreg_for_real_reg(R12),
        ));
        match arg.kind {
            AbiArgKind::Reg if arg.ty.is_int() => {
                machine.push(Amd64Instr::mov64_mr(slice, arg.reg));
            }
            AbiArgKind::Reg => {
                machine.push(Amd64Instr::xmm_unary_rm_r(
                    sse_move_opcode(arg.ty),
                    slice,
                    arg.reg,
                ));
            }
            AbiArgKind::Stack if arg.ty.is_int() => {
                machine.push(Amd64Instr::mov64_mr(slice, vreg_for_real_reg(RDX)));
                machine.push(Amd64Instr::mov_rm(
                    vreg_for_real_reg(RDX),
                    Operand::mem(AddressMode::imm_reg(
                        arg.offset as u32,
                        vreg_for_real_reg(RSP),
                    )),
                    8,
                ));
            }
            AbiArgKind::Stack => {
                machine.push(Amd64Instr::xmm_unary_rm_r(
                    sse_move_opcode(arg.ty),
                    slice,
                    vreg_for_real_reg(XMM15),
                ));
                machine.push(Amd64Instr::xmm_mov_rm(
                    sse_move_opcode(arg.ty),
                    vreg_for_real_reg(XMM15),
                    Operand::mem(AddressMode::imm_reg(
                        arg.offset as u32,
                        vreg_for_real_reg(RSP),
                    )),
                ));
            }
        }
        param_result_offset += stack_slot_size(arg.ty);
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
    let mut result_offset = 0u32;
    for ret in &abi.rets {
        let slice = Operand::mem(AddressMode::imm_reg(result_offset, vreg_for_real_reg(R12)));
        match ret.kind {
            AbiArgKind::Reg if ret.ty.is_int() => {
                machine.push(Amd64Instr::mov_rm(ret.reg, slice, 8));
            }
            AbiArgKind::Reg => {
                machine.push(Amd64Instr::xmm_mov_rm(
                    sse_move_opcode(ret.ty),
                    ret.reg,
                    slice,
                ));
            }
            AbiArgKind::Stack if ret.ty.is_int() => {
                machine.push(Amd64Instr::mov64_mr(
                    Operand::mem(AddressMode::imm_reg(
                        ret.offset as u32,
                        vreg_for_real_reg(RSP),
                    )),
                    vreg_for_real_reg(RDX),
                ));
                machine.push(Amd64Instr::mov_rm(vreg_for_real_reg(RDX), slice, 8));
            }
            AbiArgKind::Stack => {
                machine.push(Amd64Instr::xmm_unary_rm_r(
                    sse_move_opcode(ret.ty),
                    Operand::mem(AddressMode::imm_reg(
                        ret.offset as u32,
                        vreg_for_real_reg(RSP),
                    )),
                    vreg_for_real_reg(XMM15),
                ));
                machine.push(Amd64Instr::xmm_mov_rm(
                    sse_move_opcode(ret.ty),
                    vreg_for_real_reg(XMM15),
                    slice,
                ));
            }
        }
        result_offset += stack_slot_size(ret.ty);
    }
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

    #[test]
    fn preamble_handles_stack_args_and_results() {
        let sig = Signature::new(
            SignatureId(0),
            vec![
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
            ],
            vec![
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
                Type::I64,
            ],
        );
        let code = compile_entry_preamble(&sig, false);
        assert!(!code.is_empty());
        assert!(code.len() > 32);
    }
}
