#![doc = "Fallback compiler entrypoint glue."]

pub fn entrypoint(
    _preamble_executable: *const u8,
    _function_executable: *const u8,
    _execution_context_ptr: usize,
    _module_context_ptr: *const u8,
    _param_result_ptr: *mut u64,
    _go_allocated_stack_slice_ptr: usize,
) {
    panic!("unsupported architecture for compiler entrypoint");
}

pub fn after_go_function_call_entrypoint(
    _executable: *const u8,
    _execution_context_ptr: usize,
    _stack_pointer: usize,
    _frame_pointer: usize,
) {
    panic!("unsupported architecture for compiler entrypoint");
}
