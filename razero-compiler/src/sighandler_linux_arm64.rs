#![doc = "Linux arm64 signal-handler glue."]

pub use crate::sighandler_linux_amd64::{registered_jit_code_ranges, JitCodeRange, MAX_JIT_CODE_RANGES};

pub const ASM_SOURCE: &str = include_str!("sighandler_linux_arm64.S");

pub fn signal_handler_supported() -> bool {
    false
}

pub fn install_signal_handler() {}

pub fn register_jit_code_range(start: usize, end: usize) {
    crate::sighandler_linux_amd64::register_jit_code_range(start, end);
}

#[cfg(test)]
mod tests {
    use super::ASM_SOURCE;

    #[test]
    fn assembly_source_contains_expected_labels() {
        assert!(ASM_SOURCE.contains("faultReturnTrampoline"));
    }
}
