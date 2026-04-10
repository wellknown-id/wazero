#![doc = "Linux amd64 signal-handler glue."]

pub use crate::sighandler_linux::{registered_jit_code_ranges, JitCodeRange, MAX_JIT_CODE_RANGES};

pub const ASM_SOURCE: &str = include_str!("sighandler_linux_amd64.S");

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
core::arch::global_asm!(
    include_str!("sighandler_linux_amd64.S"),
    options(att_syntax)
);

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe extern "C" {
    fn razero_jit_sig_handler_addr() -> usize;
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn handler_addr() -> usize {
    unsafe { razero_jit_sig_handler_addr() }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn handler_addr() -> usize {
    0
}

pub fn signal_handler_supported() -> bool {
    cfg!(all(target_os = "linux", target_arch = "x86_64"))
}

pub fn install_signal_handler() {
    crate::sighandler_linux::install_signal_handler(handler_addr());
}

pub fn register_jit_code_range(start: usize, end: usize) {
    crate::sighandler_linux::register_jit_code_range(start, end, handler_addr());
}

#[cfg(test)]
mod tests {
    use super::{
        install_signal_handler, register_jit_code_range, registered_jit_code_ranges,
        signal_handler_supported, ASM_SOURCE,
    };

    #[test]
    fn registers_unique_ranges() {
        crate::sighandler_linux::reset_jit_code_ranges_for_tests();
        register_jit_code_range(100, 200);
        register_jit_code_range(100, 200);
        assert!(registered_jit_code_ranges()
            .iter()
            .any(|range| range.start == 100 && range.end == 200));
    }

    #[test]
    fn assembly_source_contains_expected_labels() {
        assert!(ASM_SOURCE.contains("razero_jit_sig_handler"));
        assert!(ASM_SOURCE.contains("razero_fault_return_trampoline"));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn install_signal_handler_replaces_sigsegv_handler() {
        install_signal_handler();
        assert!(signal_handler_supported());
        assert!(crate::sighandler_linux::signal_handler_installed());
        assert_eq!(
            crate::sighandler_linux::current_sigsegv_handler(),
            super::handler_addr()
        );
    }
}
