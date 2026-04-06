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
