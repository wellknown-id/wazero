#![doc = "amd64 compiler entrypoint glue."]

pub const ENTRY_ASM_SOURCE: &str = include_str!("backend/isa/amd64/abi_entry.S");

pub fn entrypoint(
    _preamble_executable: *const u8,
    _function_executable: *const u8,
    _execution_context_ptr: usize,
    _module_context_ptr: *const u8,
    _param_result_ptr: *mut u64,
    _go_allocated_stack_slice_ptr: usize,
) {
    panic!("amd64 compiler entrypoint assembly is not wired in the Rust port yet");
}

pub fn after_go_function_call_entrypoint(
    _executable: *const u8,
    _execution_context_ptr: usize,
    _stack_pointer: usize,
    _frame_pointer: usize,
) {
    panic!("amd64 compiler after-go-function entrypoint is not wired in the Rust port yet");
}

#[cfg(test)]
mod tests {
    use super::ENTRY_ASM_SOURCE;

    #[test]
    fn assembly_source_is_present() {
        assert!(ENTRY_ASM_SOURCE.contains("entrypoint"));
    }
}
