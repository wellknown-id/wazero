pub const ENTRYPOINT_SYMBOL: &str = "razero_arm64_entrypoint";
pub const AFTER_HOST_CALL_ENTRYPOINT_SYMBOL: &str =
    "razero_arm64_after_go_function_call_entrypoint";

pub fn entry_asm_source() -> &'static str {
    include_str!("abi_entry.S")
}

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(include_str!("abi_entry.S"));

#[cfg(target_arch = "aarch64")]
unsafe extern "C" {
    pub fn razero_arm64_entrypoint(
        preamble_executable: *const u8,
        function_executable: *const u8,
        execution_context_ptr: usize,
        module_context_ptr: *const u8,
        param_result_ptr: *mut u64,
        go_allocated_stack_slice_ptr: usize,
    );

    pub fn razero_arm64_after_go_function_call_entrypoint(
        executable: *const u8,
        execution_context_ptr: usize,
        stack_pointer: usize,
        frame_pointer: usize,
    );
}

#[cfg(test)]
mod tests {
    use super::{entry_asm_source, AFTER_HOST_CALL_ENTRYPOINT_SYMBOL, ENTRYPOINT_SYMBOL};

    #[test]
    fn assembly_scaffold_contains_expected_symbols() {
        let asm = entry_asm_source();
        assert!(asm.contains(ENTRYPOINT_SYMBOL));
        assert!(asm.contains(AFTER_HOST_CALL_ENTRYPOINT_SYMBOL));
    }
}
