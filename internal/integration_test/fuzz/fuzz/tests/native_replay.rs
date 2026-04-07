use std::{env, fs};

use wazero_fuzz_fuzz::{
    replay_native_parity, replay_validation, run_native_parity, run_validation, ParityOptions,
};

const FAC_WASM: &[u8] = include_bytes!("../../../../../testdata/fac.wasm");
const MEM_GROW_WASM: &[u8] = include_bytes!("../../../../../testdata/mem_grow.wasm");
const FUZZ_TEST_WASM: &[u8] = include_bytes!("../../wazerolib/testdata/test.wasm");

#[test]
fn known_modules_replay_under_native_parity() {
    replay_native_parity(
        FAC_WASM,
        ParityOptions {
            check_memory: false,
            check_logging: true,
        },
    );
    replay_native_parity(
        MEM_GROW_WASM,
        ParityOptions {
            check_memory: true,
            check_logging: false,
        },
    );
    replay_native_parity(
        FUZZ_TEST_WASM,
        ParityOptions {
            check_memory: true,
            check_logging: true,
        },
    );
}

#[test]
fn seeded_generated_modules_cover_native_targets() {
    let mut parity_hits = 0;
    let mut memory_hits = 0;
    let mut logging_hits = 0;
    let mut validation_hits = 0;

    for seed in seeded_inputs() {
        parity_hits += usize::from(run_native_parity(&seed, false, false).is_ok());
        memory_hits += usize::from(run_native_parity(&seed, true, false).is_ok());
        logging_hits += usize::from(run_native_parity(&seed, false, true).is_ok());
        validation_hits += usize::from(run_validation(&seed).is_ok());
    }

    assert!(
        parity_hits > 0,
        "at least one deterministic seed should exercise native parity"
    );
    assert!(
        memory_hits > 0,
        "at least one deterministic seed should exercise native memory parity"
    );
    assert!(
        logging_hits > 0,
        "at least one deterministic seed should exercise native logging parity"
    );
    assert!(
        validation_hits > 0,
        "at least one deterministic seed should exercise native validation"
    );
}

#[test]
fn rerun_failed_native_parity_case() {
    let Ok(path) = env::var("WASM_BINARY_PATH") else {
        return;
    };
    let wasm = fs::read(path).expect("failed replay wasm should be readable");
    replay_native_parity(
        &wasm,
        ParityOptions {
            check_memory: true,
            check_logging: true,
        },
    );
}

#[test]
fn rerun_failed_native_validation_case() {
    let Ok(path) = env::var("WASM_BINARY_PATH") else {
        return;
    };
    let wasm = fs::read(path).expect("failed replay wasm should be readable");
    replay_validation(&wasm);
}

fn seeded_inputs() -> Vec<Vec<u8>> {
    vec![
        (0..4096).map(|i| (i % 251) as u8).collect(),
        (0..4096)
            .map(|i| 255_u8.wrapping_sub((i % 251) as u8))
            .collect(),
        (0..4096).map(|i| ((i * 17) % 253) as u8).collect(),
        b"razero-native-fuzz-seed"
            .iter()
            .copied()
            .cycle()
            .take(4096)
            .collect(),
    ]
}
