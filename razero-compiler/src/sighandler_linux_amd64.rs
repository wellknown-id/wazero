#![doc = "Linux amd64 signal-handler glue."]

use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JitCodeRange {
    pub start: usize,
    pub end: usize,
}

pub const MAX_JIT_CODE_RANGES: usize = 4096;
pub const ASM_SOURCE: &str = include_str!("sighandler_linux_amd64.S");

fn ranges() -> &'static Mutex<Vec<JitCodeRange>> {
    static RANGES: OnceLock<Mutex<Vec<JitCodeRange>>> = OnceLock::new();
    RANGES.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn signal_handler_supported() -> bool {
    false
}

pub fn install_signal_handler() {}

pub fn register_jit_code_range(start: usize, end: usize) {
    assert!(start != 0 && end > start, "invalid JIT code range");
    let mut guard = ranges().lock().unwrap();
    if guard
        .iter()
        .any(|range| range.start == start && range.end == end)
    {
        return;
    }
    assert!(
        guard.len() < MAX_JIT_CODE_RANGES,
        "too many JIT code ranges"
    );
    guard.push(JitCodeRange { start, end });
    guard.sort_by_key(|range| range.start);
}

pub fn registered_jit_code_ranges() -> Vec<JitCodeRange> {
    ranges().lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::{register_jit_code_range, registered_jit_code_ranges, ASM_SOURCE};

    #[test]
    fn registers_unique_ranges() {
        register_jit_code_range(100, 200);
        register_jit_code_range(100, 200);
        assert!(registered_jit_code_ranges()
            .iter()
            .any(|range| range.start == 100 && range.end == 200));
    }

    #[test]
    fn assembly_source_contains_expected_labels() {
        assert!(ASM_SOURCE.contains("jitSigHandler"));
    }
}
