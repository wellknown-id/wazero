use crate::backend::compiler::RelocationInfo;

use super::instr::{AluOp, Arm64Instr, LoadKind};
use super::instr_encoding::{
    encode_adr, encode_instruction, encode_unconditional_branch, encode_unconditional_branch_reg,
    encode_alu_rrr, MAX_SIGNED_INT26, MIN_SIGNED_INT26,
};
use super::lower_mem::AddressMode;
use super::reg::{vreg_for_real_reg, TMP, X11};

pub const TRAMPOLINE_CALL_SIZE: usize = 20;
pub const MAX_UNCONDITIONAL_BRANCH_OFFSET: i64 = MAX_SIGNED_INT26 * 4;
pub const MIN_UNCONDITIONAL_BRANCH_OFFSET: i64 = MIN_SIGNED_INT26 * 4;
pub const TRAMPOLINE_ISLAND_INTERVAL: usize =
    ((MAX_UNCONDITIONAL_BRANCH_OFFSET - 1) as usize) / 2;
pub const MAX_NUM_FUNCTIONS: usize = TRAMPOLINE_ISLAND_INTERVAL >> 6;
pub const MAX_FUNCTION_EXECUTABLE_SIZE: usize = TRAMPOLINE_ISLAND_INTERVAL >> 2;

pub fn call_trampoline_island_info(num_functions: usize) -> Result<(usize, usize), String> {
    if num_functions > MAX_NUM_FUNCTIONS {
        Err(format!(
            "too many functions: {num_functions} > {MAX_NUM_FUNCTIONS}"
        ))
    } else {
        Ok((TRAMPOLINE_ISLAND_INTERVAL, TRAMPOLINE_CALL_SIZE * num_functions))
    }
}

pub fn encode_call_trampoline_island(
    ref_to_binary_offset: &[i32],
    imported_fns: usize,
    island_offset: usize,
    executable: &mut [u8],
) {
    let tmp = vreg_for_real_reg(TMP);
    let tmp2 = vreg_for_real_reg(X11);
    for (index, fn_offset) in ref_to_binary_offset.iter().copied().skip(imported_fns).enumerate() {
        let trampoline_offset = island_offset + TRAMPOLINE_CALL_SIZE * index;
        let diff = fn_offset - (trampoline_offset as i32 + 16);
        let words = [
            encode_adr(27, 16),
            encode_instruction(&Arm64Instr::Load {
                kind: LoadKind::SLoad,
                rd: tmp2,
                mem: AddressMode::reg_unsigned_imm12(tmp, 0),
                bits: 32,
            })
            .expect("trampoline load must encode")[0],
            encode_alu_rrr(AluOp::Add, 27, 27, 12, 64, false),
            encode_unconditional_branch_reg(27, false),
            diff as u32,
        ];
        let mut cursor = trampoline_offset;
        for word in words {
            executable[cursor..cursor + 4].copy_from_slice(&word.to_le_bytes());
            cursor += 4;
        }
    }
}

pub fn search_trampoline_island(offsets: &[i32], offset: i32) -> i32 {
    match offsets.binary_search(&offset) {
        Ok(index) => offsets[index],
        Err(index) if index >= offsets.len() => *offsets.last().expect("trampoline offsets empty"),
        Err(index) => offsets[index],
    }
}

pub fn resolve_relocations(
    ref_to_binary_offset: &[i32],
    imported_fns: usize,
    executable: &mut [u8],
    relocations: &[RelocationInfo],
    call_trampoline_island_offsets: &[i32],
) {
    for island_offset in call_trampoline_island_offsets {
        encode_call_trampoline_island(
            ref_to_binary_offset,
            imported_fns,
            *island_offset as usize,
            executable,
        );
    }

    for relocation in relocations {
        let instr_offset = relocation.offset;
        let callee_offset = ref_to_binary_offset[relocation.func_ref.0 as usize] as i64;
        let mut diff = callee_offset - instr_offset;
        if !(MIN_UNCONDITIONAL_BRANCH_OFFSET..=MAX_UNCONDITIONAL_BRANCH_OFFSET).contains(&diff) {
            let island_offset =
                search_trampoline_island(call_trampoline_island_offsets, instr_offset as i32);
            let func_offset = relocation.func_ref.0 as i32 - imported_fns as i32;
            let island_target_offset = island_offset + (TRAMPOLINE_CALL_SIZE as i32 * func_offset);
            diff = island_target_offset as i64 - instr_offset;
        }
        let word = encode_unconditional_branch(!relocation.is_tail_call, diff);
        executable[instr_offset as usize..instr_offset as usize + 4]
            .copy_from_slice(&word.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::{
        call_trampoline_island_info, encode_call_trampoline_island, resolve_relocations,
        search_trampoline_island, MAX_FUNCTION_EXECUTABLE_SIZE, MAX_NUM_FUNCTIONS,
        TRAMPOLINE_CALL_SIZE, TRAMPOLINE_ISLAND_INTERVAL,
    };
    use crate::backend::compiler::RelocationInfo;
    use crate::ssa::FuncRef;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn relocation_limits_match_go_invariants() {
        assert!(MAX_NUM_FUNCTIONS * TRAMPOLINE_CALL_SIZE < TRAMPOLINE_ISLAND_INTERVAL / 2);
        assert!(MAX_FUNCTION_EXECUTABLE_SIZE < TRAMPOLINE_ISLAND_INTERVAL / 2);
        assert!(call_trampoline_island_info(MAX_NUM_FUNCTIONS).is_ok());
    }

    #[test]
    fn trampoline_island_encoding_matches_go_fixture() {
        let mut executable = vec![0; 16 * 1000];
        let island_offset = 160usize;
        let refs = [0, 16, 1600, 16000];
        encode_call_trampoline_island(&refs, 0, island_offset, &mut executable);
        for (index, fn_offset) in refs.iter().enumerate() {
            let offset = island_offset + TRAMPOLINE_CALL_SIZE * index;
            assert_eq!(
                hex(&executable[offset..offset + TRAMPOLINE_CALL_SIZE - 4]),
                "9b0000106b0380b97b030c8b60031fd6"
            );
            let imm = u32::from_le_bytes(
                executable[offset + TRAMPOLINE_CALL_SIZE - 4..offset + TRAMPOLINE_CALL_SIZE]
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(imm, (*fn_offset - (offset as i32 + 16)) as u32);
        }
    }

    #[test]
    fn relocation_resolution_uses_nearest_trampoline_for_far_calls() {
        let mut executable = vec![0; 1024];
        let relocations = [RelocationInfo {
            offset: 100,
            func_ref: FuncRef(4),
            is_tail_call: false,
        }];
        resolve_relocations(
            &[10, 20, 30, 40, 200 * 1024 * 1024],
            3,
            &mut executable,
            &relocations,
            &[500],
        );
        let word = u32::from_le_bytes(executable[100..104].try_into().unwrap());
        assert_ne!(word, 0);
    }

    #[test]
    fn trampoline_search_matches_go_behavior() {
        let offsets = [16, 32, 48];
        assert_eq!(search_trampoline_island(&offsets, 30), 32);
        assert_eq!(search_trampoline_island(&offsets, 17), 32);
        assert_eq!(search_trampoline_island(&offsets, 1), 16);
        assert_eq!(search_trampoline_island(&offsets, 56), 48);
    }
}
