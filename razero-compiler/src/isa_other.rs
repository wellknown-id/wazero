#![doc = "Fallback ISA selection glue."]

pub const ARCH: &str = "unsupported";

pub fn is_supported() -> bool {
    false
}

pub fn unwind_stack(
    _stack_pointer: usize,
    _frame_pointer: usize,
    _stack_top: usize,
    _return_addresses: &mut Vec<usize>,
) {
    panic!("unsupported architecture")
}

pub fn go_call_stack_view(_words: &[u64]) -> &[u64] {
    panic!("unsupported architecture")
}

pub fn adjust_cloned_stack(
    _old_sp: usize,
    _old_top: usize,
    _new_sp: usize,
    _new_fp: usize,
    _new_top: usize,
) {
    panic!("unsupported architecture")
}
