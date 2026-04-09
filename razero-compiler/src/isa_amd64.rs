#![doc = "amd64 ISA selection glue."]

use std::slice;

pub const ARCH: &str = "amd64";

pub fn is_supported() -> bool {
    cfg!(target_arch = "x86_64")
}

pub fn unwind_stack(
    _stack_pointer: usize,
    _frame_pointer: usize,
    _stack_top: usize,
    return_addresses: &mut Vec<usize>,
) {
    return_addresses.clear();
}

pub fn go_call_stack_view(words: &[u64]) -> &[u64] {
    let size_bytes = words[0] as usize;
    assert_eq!(
        size_bytes & 7,
        0,
        "amd64 go-call stack size must be u64 aligned"
    );
    let len = size_bytes / 8;
    unsafe { slice::from_raw_parts(words.as_ptr().add(1), len) }
}

pub fn adjust_cloned_stack(
    _old_sp: usize,
    _old_top: usize,
    _new_sp: usize,
    _new_fp: usize,
    _new_top: usize,
) {
}
