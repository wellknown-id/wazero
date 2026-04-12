#![doc = "Shared Linux signal-handler runtime state."]

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use razero_platform::signal::{install_sigsegv_handler, read_sigsegv_handler};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct JitCodeRange {
    pub start: usize,
    pub end: usize,
}

pub const MAX_JIT_CODE_RANGES: usize = 4096;

#[no_mangle]
pub static razero_jit_range_count: AtomicU32 = AtomicU32::new(0);

#[no_mangle]
pub static razero_saved_go_handler: AtomicUsize = AtomicUsize::new(0);

#[no_mangle]
pub static mut razero_jit_ranges: [JitCodeRange; MAX_JIT_CODE_RANGES] =
    [JitCodeRange { start: 0, end: 0 }; MAX_JIT_CODE_RANGES];

static SIGNAL_HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);

fn state_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub fn signal_handler_supported() -> bool {
    cfg!(target_os = "linux") && cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
}

#[cfg(target_os = "linux")]
fn install_signal_handler_impl(handler_addr: usize) {
    if SIGNAL_HANDLER_INSTALLED.load(Ordering::Acquire) {
        return;
    }

    let _guard = state_lock().lock().unwrap();
    if SIGNAL_HANDLER_INSTALLED.load(Ordering::Relaxed) {
        return;
    }

    match install_sigsegv_handler(handler_addr) {
        Ok(old) => {
            razero_saved_go_handler.store(old.handler, Ordering::Release);
        }
        Err(e) => panic!("wazevo: failed to install SIGSEGV handler: {}", e),
    }

    SIGNAL_HANDLER_INSTALLED.store(true, Ordering::Release);
}

#[cfg(not(target_os = "linux"))]
fn install_signal_handler_impl(_handler_addr: usize) {}

pub fn install_signal_handler(handler_addr: usize) {
    if !signal_handler_supported() {
        return;
    }
    install_signal_handler_impl(handler_addr);
}

pub fn register_jit_code_range(start: usize, end: usize, handler_addr: usize) {
    assert!(start != 0 && end > start, "invalid JIT code range");

    install_signal_handler(handler_addr);

    let _guard = state_lock().lock().unwrap();
    let count = razero_jit_range_count.load(Ordering::Relaxed) as usize;

    for i in 0..count {
        let range = unsafe { razero_jit_ranges[i] };
        if range.start == start && range.end == end {
            return;
        }
    }

    assert!(count < MAX_JIT_CODE_RANGES, "too many JIT code ranges");

    unsafe {
        razero_jit_ranges[count] = JitCodeRange { start, end };
    }
    razero_jit_range_count.store((count + 1) as u32, Ordering::Release);
}

pub fn registered_jit_code_ranges() -> Vec<JitCodeRange> {
    let _guard = state_lock().lock().unwrap();
    let count = razero_jit_range_count.load(Ordering::Acquire) as usize;
    (0..count)
        .map(|i| unsafe { razero_jit_ranges[i] })
        .collect()
}

#[cfg(test)]
pub(crate) fn signal_handler_installed() -> bool {
    SIGNAL_HANDLER_INSTALLED.load(Ordering::Acquire)
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn current_sigsegv_handler() -> usize {
    let _guard = state_lock().lock().unwrap();
    match read_sigsegv_handler() {
        Ok(current) => current.handler,
        Err(e) => panic!("wazevo: failed to read current SIGSEGV handler: {}", e),
    }
}

#[cfg(test)]
pub(crate) fn reset_jit_code_ranges_for_tests() {
    let _guard = state_lock().lock().unwrap();
    let count = razero_jit_range_count.swap(0, Ordering::AcqRel) as usize;
    for i in 0..count {
        unsafe {
            razero_jit_ranges[i] = JitCodeRange::default();
        }
    }
}
