#![doc = "Signal-handler fallback glue."]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JitCodeRange {
    pub start: usize,
    pub end: usize,
}

pub fn signal_handler_supported() -> bool {
    false
}

pub fn install_signal_handler() {}

pub fn register_jit_code_range(_start: usize, _end: usize) {}
