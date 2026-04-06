use crate::ssa::Signature;
use crate::wazevoapi::offsetdata::{
    EXECUTION_CONTEXT_OFFSET_GO_RETURN_ADDRESS, EXECUTION_CONTEXT_OFFSET_ORIGINAL_FRAME_POINTER,
    EXECUTION_CONTEXT_OFFSET_ORIGINAL_STACK_POINTER,
};

use super::abi::Arm64Abi;
use super::instr::{AluOp, Arm64Instr, LoadKind, StoreKind};
use super::instr_encoding::encode_instruction;
use super::lower_instr_operands::as_imm12;
use super::lower_mem::{resolve_address_mode_for_offset, AddressMode};
use super::reg::{vreg_for_real_reg, FP, LR, SP, TMP, V15, X0, X15, X17, X19, X20, X24, X26};

fn store_exec_ctx(
    instructions: &mut Vec<Arm64Instr>,
    exec_ctx: crate::backend::VReg,
    src: crate::backend::VReg,
    offset: i64,
) {
    instructions.push(Arm64Instr::Store {
        kind: StoreKind::Store,
        src,
        mem: AddressMode::reg_unsigned_imm12(exec_ctx, offset),
        bits: 64,
    });
}

fn load_exec_ctx(
    instructions: &mut Vec<Arm64Instr>,
    exec_ctx: crate::backend::VReg,
    dst: crate::backend::VReg,
    offset: i64,
) {
    instructions.push(Arm64Instr::Load {
        kind: LoadKind::ULoad,
        rd: dst,
        mem: AddressMode::reg_unsigned_imm12(exec_ctx, offset),
        bits: 64,
    });
}

fn add_immediate(instructions: &mut Vec<Arm64Instr>, reg: crate::backend::VReg, amount: i64) {
    let imm = as_imm12(amount as u64).expect("entry preamble pointer increments must fit imm12");
    instructions.push(Arm64Instr::AluRRImm12 {
        op: AluOp::Add,
        rd: reg,
        rn: reg,
        imm,
        bits: 64,
        set_flags: false,
    });
}

fn temp_reg_for(ty: crate::ssa::Type) -> crate::backend::VReg {
    if ty.is_int() {
        vreg_for_real_reg(X15)
    } else {
        vreg_for_real_reg(V15)
    }
}

fn emit_param_load(
    instructions: &mut Vec<Arm64Instr>,
    ptr: crate::backend::VReg,
    dst: crate::backend::VReg,
    ty: crate::ssa::Type,
) {
    instructions.push(Arm64Instr::Load {
        kind: if ty.is_int() {
            LoadKind::ULoad
        } else {
            LoadKind::FpuLoad
        },
        rd: dst,
        mem: AddressMode::reg_unsigned_imm12(ptr, 0),
        bits: ty.bits(),
    });
    add_immediate(instructions, ptr, if ty.bits() == 128 { 16 } else { 8 });
}

pub(crate) fn build_entry_preamble(signature: &Signature, use_host_stack: bool) -> Vec<Arm64Instr> {
    let abi = Arm64Abi::from_signature(signature);
    let mut instructions = Vec::new();
    let exec_ctx = vreg_for_real_reg(X0);
    let saved_exec_ctx = vreg_for_real_reg(X20);
    let param_result_ptr = vreg_for_real_reg(X19);
    let function_executable = vreg_for_real_reg(X24);

    instructions.push(Arm64Instr::Move {
        rd: saved_exec_ctx,
        rn: exec_ctx,
        bits: 64,
    });
    store_exec_ctx(
        &mut instructions,
        saved_exec_ctx,
        vreg_for_real_reg(FP),
        EXECUTION_CONTEXT_OFFSET_ORIGINAL_FRAME_POINTER.i64(),
    );
    instructions.push(Arm64Instr::Move {
        rd: vreg_for_real_reg(TMP),
        rn: vreg_for_real_reg(SP),
        bits: 64,
    });
    store_exec_ctx(
        &mut instructions,
        saved_exec_ctx,
        vreg_for_real_reg(TMP),
        EXECUTION_CONTEXT_OFFSET_ORIGINAL_STACK_POINTER.i64(),
    );
    store_exec_ctx(
        &mut instructions,
        saved_exec_ctx,
        vreg_for_real_reg(LR),
        EXECUTION_CONTEXT_OFFSET_GO_RETURN_ADDRESS.i64(),
    );

    if !use_host_stack {
        instructions.push(Arm64Instr::Move {
            rd: vreg_for_real_reg(SP),
            rn: vreg_for_real_reg(X26),
            bits: 64,
        });
    }

    let mut param_ptr = param_result_ptr;
    if abi.function.args.len() > 2 && !abi.function.rets.is_empty() {
        param_ptr = vreg_for_real_reg(X17);
        instructions.push(Arm64Instr::Move {
            rd: param_ptr,
            rn: param_result_ptr,
            bits: 64,
        });
    }

    let stack_slot_size = abi.function.aligned_arg_result_stack_slot_size() as i64;
    for arg in abi.function.args.iter().skip(2) {
        if arg.kind == crate::backend::AbiArgKind::Reg {
            emit_param_load(&mut instructions, param_ptr, arg.reg, arg.ty);
        } else {
            let tmp = temp_reg_for(arg.ty);
            emit_param_load(&mut instructions, param_ptr, tmp, arg.ty);
            let mem = resolve_address_mode_for_offset(
                -stack_slot_size + arg.offset,
                arg.ty.bits(),
                vreg_for_real_reg(SP),
                vreg_for_real_reg(TMP),
            );
            instructions.push(Arm64Instr::Store {
                kind: if arg.ty.is_int() {
                    StoreKind::Store
                } else {
                    StoreKind::FpuStore
                },
                src: tmp,
                mem,
                bits: arg.ty.bits(),
            });
        }
    }

    instructions.push(Arm64Instr::BrReg {
        rn: function_executable,
        link: true,
    });

    for ret in &abi.function.rets {
        let src = if ret.kind == crate::backend::AbiArgKind::Reg {
            ret.reg
        } else {
            let tmp = temp_reg_for(ret.ty);
            let mem = resolve_address_mode_for_offset(
                abi.function.arg_stack_size - stack_slot_size + ret.offset,
                ret.ty.bits(),
                vreg_for_real_reg(SP),
                vreg_for_real_reg(TMP),
            );
            instructions.push(Arm64Instr::Load {
                kind: if ret.ty.is_int() {
                    LoadKind::ULoad
                } else {
                    LoadKind::FpuLoad
                },
                rd: tmp,
                mem,
                bits: ret.ty.bits(),
            });
            tmp
        };
        instructions.push(Arm64Instr::Store {
            kind: if ret.ty.is_int() {
                StoreKind::Store
            } else {
                StoreKind::FpuStore
            },
            src,
            mem: AddressMode::reg_unsigned_imm12(param_result_ptr, 0),
            bits: ret.ty.bits(),
        });
        add_immediate(
            &mut instructions,
            param_result_ptr,
            if ret.ty.bits() == 128 { 16 } else { 8 },
        );
    }

    load_exec_ctx(
        &mut instructions,
        saved_exec_ctx,
        vreg_for_real_reg(FP),
        EXECUTION_CONTEXT_OFFSET_ORIGINAL_FRAME_POINTER.i64(),
    );
    load_exec_ctx(
        &mut instructions,
        saved_exec_ctx,
        vreg_for_real_reg(TMP),
        EXECUTION_CONTEXT_OFFSET_ORIGINAL_STACK_POINTER.i64(),
    );
    instructions.push(Arm64Instr::Move {
        rd: vreg_for_real_reg(SP),
        rn: vreg_for_real_reg(TMP),
        bits: 64,
    });
    load_exec_ctx(
        &mut instructions,
        saved_exec_ctx,
        vreg_for_real_reg(LR),
        EXECUTION_CONTEXT_OFFSET_GO_RETURN_ADDRESS.i64(),
    );
    instructions.push(Arm64Instr::Ret);
    instructions
}

pub fn compile_entry_preamble(signature: &Signature, use_host_stack: bool) -> Vec<u8> {
    let instructions = build_entry_preamble(signature, use_host_stack);
    let mut buf = Vec::new();
    for instr in instructions {
        if let Ok(words) = encode_instruction(&instr) {
            for word in words {
                buf.extend_from_slice(&word.to_le_bytes());
            }
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::build_entry_preamble;
    use crate::ssa::{Signature, SignatureId, Type};

    #[test]
    fn entry_preamble_saves_state_and_switches_stack() {
        let sig = Signature::new(SignatureId(0), vec![Type::I64, Type::I64], vec![Type::I64]);
        let text = build_entry_preamble(&sig, false)
            .into_iter()
            .map(|instr| instr.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("mov x20, x0"));
        assert!(text.contains("str x29, [x20, #0x10]"));
        assert!(text.contains("mov sp, x26"));
        assert!(text.contains("blr x24"));
        assert!(text.contains("ldr x30, [x20, #0x20]"));
    }

    #[test]
    fn entry_preamble_marshals_stack_args_and_results() {
        let sig = Signature::new(
            SignatureId(1),
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
            ],
        );
        let text = build_entry_preamble(&sig, true)
            .into_iter()
            .map(|instr| instr.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("ldr x15, [x17]"));
        assert!(text.contains("str x15, [sp, #0x-10]"));
        assert!(text.contains("ldr x15, [sp"));
        assert!(text.contains("str x15, [x19]"));
    }
}
