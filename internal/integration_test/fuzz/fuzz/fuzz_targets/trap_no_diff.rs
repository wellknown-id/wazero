#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = wazero_fuzz_fuzz::run_fixed_trap_parity(data);
});
