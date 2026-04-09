#![doc = "Linux arm64 signal-handler glue."]

pub use crate::sighandler_linux::{registered_jit_code_ranges, JitCodeRange, MAX_JIT_CODE_RANGES};

pub const ASM_SOURCE: &str = include_str!("sighandler_linux_arm64.S");

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
core::arch::global_asm!(include_str!("sighandler_linux_arm64.S"));

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
unsafe extern "C" {
    fn razero_jit_sig_handler_addr() -> usize;
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn handler_addr() -> usize {
    unsafe { razero_jit_sig_handler_addr() }
}

#[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
fn handler_addr() -> usize {
    0
}

pub fn signal_handler_supported() -> bool {
    cfg!(all(target_os = "linux", target_arch = "aarch64"))
}

pub fn install_signal_handler() {
    crate::sighandler_linux::install_signal_handler(handler_addr());
}

pub fn register_jit_code_range(start: usize, end: usize) {
    crate::sighandler_linux::register_jit_code_range(start, end, handler_addr());
}

#[cfg(test)]
mod tests {
    use super::ASM_SOURCE;

    #[test]
    fn assembly_source_contains_expected_labels() {
        assert!(ASM_SOURCE.contains("razero_fault_return_trampoline"));
    }
}
