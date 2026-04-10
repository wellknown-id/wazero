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
    use super::{signal_handler_supported, ASM_SOURCE};

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    use super::{install_signal_handler, register_jit_code_range, registered_jit_code_ranges};

    #[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
    #[test]
    fn signal_handler_is_reported_unsupported_off_arm64_linux() {
        assert!(!signal_handler_supported());
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
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

    #[test]
    fn assembly_source_defines_execution_context_offsets() {
        assert!(ASM_SOURCE.contains(".equ EXECCTX_EXITCODE_OFF, 0"));
        assert!(ASM_SOURCE.contains(".equ EXECCTX_ORIGFP_OFF, 16"));
        assert!(ASM_SOURCE.contains(".equ EXECCTX_ORIGSP_OFF, 24"));
        assert!(ASM_SOURCE.contains(".equ EXECCTX_GORET_OFF, 32"));
    }

    #[test]
    fn assembly_source_defines_sigcontext_offsets() {
        assert!(ASM_SOURCE.contains(".equ UCONTEXT_MCONTEXT_OFF, 176"));
        assert!(ASM_SOURCE.contains(".equ SIGCONTEXT_REGS_OFF, 8"));
        assert!(ASM_SOURCE.contains(".equ SIGCONTEXT_SP_OFF, 256"));
        assert!(ASM_SOURCE.contains(".equ SIGCONTEXT_PC_OFF, 264"));
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
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
