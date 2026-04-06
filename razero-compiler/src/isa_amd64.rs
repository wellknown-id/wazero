#![doc = "amd64 ISA selection glue."]

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
    words
}

pub fn adjust_cloned_stack(
    _old_sp: usize,
    _old_top: usize,
    _new_sp: usize,
    _new_fp: usize,
    _new_top: usize,
) {
}
