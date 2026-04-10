#![doc = "amd64 compiler entrypoint glue."]

pub const ENTRY_ASM_SOURCE: &str = include_str!("backend/isa/amd64/abi_entry.S");

#[cfg(target_arch = "x86_64")]
pub fn entrypoint(
    preamble_executable: *const u8,
    function_executable: *const u8,
    execution_context_ptr: usize,
    module_context_ptr: *const u8,
    param_result_ptr: *mut u64,
    go_allocated_stack_slice_ptr: usize,
) {
    unsafe {
        crate::backend::isa::amd64::abi_entry::razero_amd64_entrypoint(
            preamble_executable,
            function_executable,
            execution_context_ptr,
            module_context_ptr,
            param_result_ptr,
            go_allocated_stack_slice_ptr,
        );
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn entrypoint(
    _preamble_executable: *const u8,
    _function_executable: *const u8,
    _execution_context_ptr: usize,
    _module_context_ptr: *const u8,
    _param_result_ptr: *mut u64,
    _go_allocated_stack_slice_ptr: usize,
) {
    panic!("amd64 entrypoint is only available on x86_64 targets");
}

#[cfg(target_arch = "x86_64")]
pub fn after_go_function_call_entrypoint(
    executable: *const u8,
    execution_context_ptr: usize,
    stack_pointer: usize,
    frame_pointer: usize,
) {
    unsafe {
        crate::backend::isa::amd64::abi_entry::razero_amd64_after_go_function_call_entrypoint(
            executable,
            execution_context_ptr,
            stack_pointer,
            frame_pointer,
        );
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn after_go_function_call_entrypoint(
    _executable: *const u8,
    _execution_context_ptr: usize,
    _stack_pointer: usize,
    _frame_pointer: usize,
) {
    panic!("amd64 after-go-function entrypoint is only available on x86_64 targets");
}

#[cfg(test)]
mod tests {
    use super::ENTRY_ASM_SOURCE;
    #[cfg(not(target_arch = "x86_64"))]
    use super::{after_go_function_call_entrypoint, entrypoint};
    #[cfg(not(target_arch = "x86_64"))]
    use std::panic::{catch_unwind, AssertUnwindSafe};

    #[test]
    fn assembly_source_is_present() {
        assert!(ENTRY_ASM_SOURCE.contains("razero_amd64_entrypoint"));
        assert!(ENTRY_ASM_SOURCE.contains("razero_amd64_after_go_function_call_entrypoint"));
    }

    #[cfg(not(target_arch = "x86_64"))]
    #[test]
    fn entrypoint_rejects_non_x86_64_targets() {
        let err = catch_unwind(AssertUnwindSafe(|| {
            entrypoint(
                core::ptr::null(),
                core::ptr::null(),
                0,
                core::ptr::null(),
                core::ptr::null_mut(),
                0,
            )
        }))
        .expect_err("non-x86_64 entrypoint should panic");

        let message = err
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| err.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic>");
        assert_eq!(
            "amd64 entrypoint is only available on x86_64 targets",
            message
        );
    }

    #[cfg(not(target_arch = "x86_64"))]
    #[test]
    fn after_go_function_call_entrypoint_rejects_non_x86_64_targets() {
        let err = catch_unwind(AssertUnwindSafe(|| {
            after_go_function_call_entrypoint(core::ptr::null(), 0, 0, 0)
        }))
        .expect_err("non-x86_64 after-go entrypoint should panic");

        let message = err
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| err.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic>");
        assert_eq!(
            "amd64 after-go-function entrypoint is only available on x86_64 targets",
            message
        );
    }
}
