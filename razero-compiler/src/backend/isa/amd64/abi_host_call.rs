use crate::backend::go_function_call_required_stack_size;
use crate::backend::{AbiArg, AbiArgKind};
use crate::ssa::{Signature, Type};
use crate::wazevoapi::ExitCode;

use super::abi::amd64_function_abi;
use super::ext::ExtMode;
use super::instr::{AluRmiROpcode, Amd64Instr};
use super::machine_vec::SseOpcode;
use super::operands::{AddressMode, Label, Operand};
use super::reg::{
    vreg_for_real_reg, R12, R13, R14, R15, RAX, RBP, RBX, RDX, RSP, XMM10, XMM11, XMM12, XMM13,
    XMM14, XMM15, XMM8, XMM9,
};

const EXECUTION_CONTEXT_OFFSET_EXIT_CODE: u32 = 0;
const EXECUTION_CONTEXT_OFFSET_GO_CALL_RETURN_ADDRESS: u32 = 48;
const EXECUTION_CONTEXT_OFFSET_STACK_POINTER_BEFORE_GO_CALL: u32 = 56;
const EXECUTION_CONTEXT_OFFSET_SAVED_REGISTERS_BEGIN: u32 = 96;
const EXECUTION_CONTEXT_OFFSET_GO_FUNCTION_CALL_CALLEE_MODULE_CONTEXT_OPAQUE: u32 = 1120;
const EXECUTION_CONTEXT_OFFSET_FRAME_POINTER_BEFORE_GO_CALL: u32 = 1152;
const EXECUTION_CONTEXT_OFFSET_ORIGINAL_FRAME_POINTER: u32 = 16;
const EXECUTION_CONTEXT_OFFSET_ORIGINAL_STACK_POINTER: u32 = 24;
const SAVED_REGISTER_SLOT_SIZE: u32 = 16;

fn stack_slot_size(ty: Type) -> u32 {
    if ty.bits() == 128 {
        16
    } else {
        8
    }
}

fn sse_move_opcode(ty: Type) -> SseOpcode {
    match ty {
        Type::F32 => SseOpcode::Movss,
        Type::F64 => SseOpcode::Movsd,
        Type::V128 => SseOpcode::Movdqu,
        Type::I32 | Type::I64 | Type::Invalid => unreachable!("SSE move requested for non-float"),
    }
}

fn exec_ctx_mem(offset: u32) -> Operand {
    Operand::mem(AddressMode::imm_reg(offset, vreg_for_real_reg(RAX)))
}

fn rbp_mem(offset: u32) -> Operand {
    Operand::mem(AddressMode::imm_reg(offset, vreg_for_real_reg(RBP)))
}

fn rsp_mem(offset: u32) -> Operand {
    Operand::mem(AddressMode::imm_reg(offset, vreg_for_real_reg(RSP)))
}

fn encode_with_rip_patch(
    instructions: &[Amd64Instr],
    rip_patch_index: usize,
    continuation_index: usize,
) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(instructions.len());
    let mut offsets = Vec::with_capacity(instructions.len());
    let mut cursor = 0usize;
    for instr in instructions {
        offsets.push(cursor);
        let bytes = instr
            .encode()
            .unwrap_or_else(|e| panic!("failed to encode amd64 host trampoline: {e}"));
        cursor += bytes.len();
        encoded.push(bytes);
    }

    let lea_offset = offsets[rip_patch_index];
    let target_offset = offsets[continuation_index];
    let lea_bytes = &mut encoded[rip_patch_index];
    let lea_end = lea_offset + lea_bytes.len();
    let disp = (target_offset as isize - lea_end as isize) as i32;
    let patch_at = lea_bytes.len() - 4;
    lea_bytes[patch_at..].copy_from_slice(&disp.to_le_bytes());

    let mut out = Vec::with_capacity(cursor);
    for bytes in encoded {
        out.extend_from_slice(&bytes);
    }
    out
}

fn save_registers(instructions: &mut Vec<Amd64Instr>, regs: &[(crate::backend::RealReg, bool)]) {
    let mut offset = EXECUTION_CONTEXT_OFFSET_SAVED_REGISTERS_BEGIN;
    for &(reg, is_float) in regs {
        if is_float {
            instructions.push(Amd64Instr::xmm_mov_rm(
                SseOpcode::Movdqu,
                vreg_for_real_reg(reg),
                exec_ctx_mem(offset),
            ));
        } else {
            instructions.push(Amd64Instr::mov_rm(
                vreg_for_real_reg(reg),
                exec_ctx_mem(offset),
                8,
            ));
        }
        offset += SAVED_REGISTER_SLOT_SIZE;
    }
}

fn restore_registers(instructions: &mut Vec<Amd64Instr>, regs: &[(crate::backend::RealReg, bool)]) {
    let mut offset = EXECUTION_CONTEXT_OFFSET_SAVED_REGISTERS_BEGIN;
    for &(reg, is_float) in regs {
        if is_float {
            instructions.push(Amd64Instr::xmm_unary_rm_r(
                SseOpcode::Movdqu,
                exec_ctx_mem(offset),
                vreg_for_real_reg(reg),
            ));
        } else {
            instructions.push(Amd64Instr::mov64_mr(
                exec_ctx_mem(offset),
                vreg_for_real_reg(reg),
            ));
        }
        offset += SAVED_REGISTER_SLOT_SIZE;
    }
}

fn load_stack_arg(
    instructions: &mut Vec<Amd64Instr>,
    arg: &AbiArg,
    tmp_int: u8,
    tmp_float: u8,
) -> u8 {
    let tmp = if arg.ty.is_int() { tmp_int } else { tmp_float };
    let mem = rbp_mem((arg.offset + 16) as u32);
    match arg.ty {
        Type::I32 => instructions.push(Amd64Instr::movzx_rm_r(
            ExtMode::LQ,
            mem,
            vreg_for_real_reg(tmp),
        )),
        Type::I64 => instructions.push(Amd64Instr::mov64_mr(mem, vreg_for_real_reg(tmp))),
        Type::F32 | Type::F64 | Type::V128 => instructions.push(Amd64Instr::xmm_unary_rm_r(
            sse_move_opcode(arg.ty),
            mem,
            vreg_for_real_reg(tmp),
        )),
        Type::Invalid => unreachable!("invalid arg type"),
    }
    tmp
}

pub fn compile_host_function_trampoline(
    exit_code: ExitCode,
    sig: &Signature,
    need_module_context_ptr: bool,
) -> Vec<u8> {
    let arg_begin = if need_module_context_ptr { 2 } else { 1 };
    let abi = amd64_function_abi(sig);
    let (slice_size_aligned, slice_size_unaligned) =
        go_function_call_required_stack_size(sig, arg_begin);

    let callee_saved = &[
        (RDX, false),
        (R12, false),
        (R13, false),
        (R14, false),
        (R15, false),
        (XMM8, true),
        (XMM9, true),
        (XMM10, true),
        (XMM11, true),
        (XMM12, true),
        (XMM13, true),
        (XMM14, true),
        (XMM15, true),
    ];

    let mut instructions = Vec::new();

    instructions.push(Amd64Instr::push64(Operand::reg(vreg_for_real_reg(RBP))));
    instructions.push(Amd64Instr::mov_rr(
        vreg_for_real_reg(RSP),
        vreg_for_real_reg(RBP),
        true,
    ));

    if slice_size_aligned > 0 {
        instructions.push(Amd64Instr::alu_rmi_r(
            AluRmiROpcode::Sub,
            Operand::imm32(slice_size_aligned as u32),
            vreg_for_real_reg(RSP),
            true,
        ));
    }

    save_registers(&mut instructions, callee_saved);

    if need_module_context_ptr {
        instructions.push(Amd64Instr::mov_rm(
            vreg_for_real_reg(RBX),
            exec_ctx_mem(EXECUTION_CONTEXT_OFFSET_GO_FUNCTION_CALL_CALLEE_MODULE_CONTEXT_OPAQUE),
            8,
        ));
    }

    let mut offset_in_go_slice = 0u32;
    for arg in abi.args.iter().skip(arg_begin) {
        let src = if arg.kind == AbiArgKind::Reg {
            arg.reg
        } else {
            vreg_for_real_reg(load_stack_arg(&mut instructions, arg, R15, XMM15))
        };
        match arg.ty {
            Type::I32 => {
                instructions.push(Amd64Instr::mov_rm(src, rsp_mem(offset_in_go_slice), 4));
                offset_in_go_slice += 8;
            }
            Type::I64 => {
                instructions.push(Amd64Instr::mov_rm(src, rsp_mem(offset_in_go_slice), 8));
                offset_in_go_slice += 8;
            }
            Type::F32 | Type::F64 | Type::V128 => {
                instructions.push(Amd64Instr::xmm_mov_rm(
                    sse_move_opcode(arg.ty),
                    src,
                    rsp_mem(offset_in_go_slice),
                ));
                offset_in_go_slice += stack_slot_size(arg.ty);
            }
            Type::Invalid => unreachable!("invalid arg type"),
        }
    }

    instructions.push(Amd64Instr::push64(Operand::imm32(
        slice_size_unaligned as u32,
    )));
    instructions.push(Amd64Instr::imm(
        vreg_for_real_reg(R12),
        exit_code.raw() as u64,
        false,
    ));
    instructions.push(Amd64Instr::mov_rm(
        vreg_for_real_reg(R12),
        exec_ctx_mem(EXECUTION_CONTEXT_OFFSET_EXIT_CODE),
        4,
    ));
    instructions.push(Amd64Instr::mov_rm(
        vreg_for_real_reg(RSP),
        exec_ctx_mem(EXECUTION_CONTEXT_OFFSET_STACK_POINTER_BEFORE_GO_CALL),
        8,
    ));
    instructions.push(Amd64Instr::mov_rm(
        vreg_for_real_reg(RBP),
        exec_ctx_mem(EXECUTION_CONTEXT_OFFSET_FRAME_POINTER_BEFORE_GO_CALL),
        8,
    ));

    let rip_patch_index = instructions.len();
    instructions.push(Amd64Instr::lea(
        Operand::mem(AddressMode::rip_rel(Label(0))),
        vreg_for_real_reg(R12),
    ));
    instructions.push(Amd64Instr::mov_rm(
        vreg_for_real_reg(R12),
        exec_ctx_mem(EXECUTION_CONTEXT_OFFSET_GO_CALL_RETURN_ADDRESS),
        8,
    ));
    instructions.push(Amd64Instr::mov64_mr(
        exec_ctx_mem(EXECUTION_CONTEXT_OFFSET_ORIGINAL_FRAME_POINTER),
        vreg_for_real_reg(RBP),
    ));
    instructions.push(Amd64Instr::mov64_mr(
        exec_ctx_mem(EXECUTION_CONTEXT_OFFSET_ORIGINAL_STACK_POINTER),
        vreg_for_real_reg(RSP),
    ));
    instructions.push(Amd64Instr::ret());

    let continuation_index = instructions.len();
    instructions.push(Amd64Instr::alu_rmi_r(
        AluRmiROpcode::Add,
        Operand::imm32(8),
        vreg_for_real_reg(RSP),
        true,
    ));

    let mut overlap_offset = None;
    offset_in_go_slice = 0;
    for ret in &abi.rets {
        let dst = if ret.kind == AbiArgKind::Reg {
            if ret.ty.is_int() && ret.reg.real_reg() == RAX {
                overlap_offset = Some(offset_in_go_slice);
                offset_in_go_slice += stack_slot_size(ret.ty);
                continue;
            }
            ret.reg
        } else if ret.ty.is_int() {
            vreg_for_real_reg(R15)
        } else {
            vreg_for_real_reg(XMM15)
        };
        let mem = rsp_mem(offset_in_go_slice);
        match ret.ty {
            Type::I32 => instructions.push(Amd64Instr::movzx_rm_r(ExtMode::LQ, mem, dst)),
            Type::I64 => instructions.push(Amd64Instr::mov64_mr(mem, dst)),
            Type::F32 | Type::F64 | Type::V128 => instructions.push(Amd64Instr::xmm_unary_rm_r(
                sse_move_opcode(ret.ty),
                mem,
                dst,
            )),
            Type::Invalid => unreachable!("invalid ret type"),
        }
        if ret.kind == AbiArgKind::Stack {
            let dst_mem = rbp_mem((abi.arg_stack_size + ret.offset + 16) as u32);
            if ret.ty.is_int() {
                instructions.push(Amd64Instr::mov_rm(
                    dst,
                    dst_mem,
                    if ret.ty.bits() == 32 { 4 } else { 8 },
                ));
            } else {
                instructions.push(Amd64Instr::xmm_mov_rm(
                    sse_move_opcode(ret.ty),
                    dst,
                    dst_mem,
                ));
            }
        }
        offset_in_go_slice += stack_slot_size(ret.ty);
    }

    restore_registers(&mut instructions, callee_saved);

    if let Some(offset) = overlap_offset {
        instructions.push(Amd64Instr::mov64_mr(
            rsp_mem(offset),
            vreg_for_real_reg(RAX),
        ));
    }

    instructions.push(Amd64Instr::mov_rr(
        vreg_for_real_reg(RBP),
        vreg_for_real_reg(RSP),
        true,
    ));
    instructions.push(Amd64Instr::pop64(vreg_for_real_reg(RBP)));
    instructions.push(Amd64Instr::ret());

    encode_with_rip_patch(&instructions, rip_patch_index, continuation_index)
}

#[cfg(test)]
mod tests {
    use super::compile_host_function_trampoline;
    use crate::ssa::{Signature, SignatureId, Type};
    use crate::wazevoapi::ExitCode;

    #[test]
    fn host_call_trampoline_emits_exit_code_setup() {
        let sig = Signature::new(SignatureId(0), vec![Type::I64, Type::I64], vec![Type::I64]);
        let code = compile_host_function_trampoline(ExitCode::CALL_GO_FUNCTION, &sig, true);
        assert!(!code.is_empty());
        assert!(code.ends_with(&[0xC3]));
    }
}
