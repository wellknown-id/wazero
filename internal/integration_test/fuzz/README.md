Fuzzing infrastructure for native Rust `razero` verification via [wasm-tools](https://github.com/bytecodealliance/wasm-tools).

### Dependency

- [cargo](https://doc.rust-lang.org/cargo/getting-started/installation.html)
  - Needs to enable nightly (for libFuzzer).
- [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz)
  - `cargo install cargo-fuzz`

### Run Fuzzing

Currently, we have the following fuzzing targets:

- `no_diff`: generates terminating Wasm modules and compares native Rust execution outcomes between standard and secure mode.
- `memory_no_diff`: same as `no_diff`, and also compares the final guest memory snapshot between modes.
- `logging_no_diff`: same as `no_diff`, and also compares listener/log formatting output between modes.
- `policy_no_diff`: replays fixed cooperative-yield and imported-host-call guest scenarios and compares policy-denied behavior, trap-observer output, and relevant yield / host-call policy observer event streams between standard and secure mode.
- `trap_no_diff`: selects from a deterministic set of fixed trap fixtures (for example out-of-bounds memory access, divide-by-zero, invalid conversion, indirect-call mismatch, and unreachable) and compares trap behavior between standard and secure mode.
- `validation`: compiles maybe-invalid Wasm module binaries with the native Rust runtime to ensure validation and compilation do not panic.

`cargo test` in this workspace also runs deterministic replay coverage for
`fac.wasm`, `mem_grow.wasm`, `oob_load.wasm`, the cooperative-yield fixture,
an additional bundled `test.wasm` fixture, fixed trap fixtures shared with
runtime tests, and fixed seed inputs shared across all native targets.


To run the fuzzer on a target, execute the following command:

```
# Running on the host archictecture.
cargo fuzz run <target>
```

where you replace `<target>` with one of the targets described above.

See `cargo fuzz run --help` for the options. Especially, the following flags are useful:

- `-jobs=N`: `cargo fuzz run` by default only spawns one worker, so this flag helps do the parallel fuzzing.
  - usage: `cargo fuzz run no_diff -- -jobs=5` will run 5 parallel workers to run fuzzing jobs.
- `-max_total_time`: the maximum total time in seconds to run the fuzzer.
  - usage: `cargo fuzz run no_diff -- -max_total_time=100` will run fuzzing for 100 seconds.
- `-timeout` sets the timeout seconds _per fuzzing run_, not the entire job.
- `-rss_limit_mb` sets the memory usage limit which is 2GB by default. Usually 2GB is not enough for some large Wasm binary.

#### Example commands

```
# Running the `no_diff` target with 15 concurrent jobs with total runnig time with 2hrs and 8GB memory limit.
$ cargo fuzz run no_diff --sanitizer=none --no-trace-compares -- -rss_limit_mb=8192 -max_len=5000000 -max_total_time=7200 -jobs=15

# Running the `memory_no_diff` target with 15 concurrent jobs with timeout 2hrs and setting timeout per fuzz case to 30s.
$ cargo fuzz run memory_no_diff --sanitizer=none --no-trace-compares -- -timeout=30 -max_total_time=7200 -jobs=15

# Running the `validation` target with 4 concurrent jobs with timeout 2hrs and setting timeout per fuzz case to 30s.
# cargo fuzz run validation --sanitizer=none --no-trace-compares -- -timeout=30 -max_total_time=7200 -jobs=4

# Running the `policy_no_diff` target to compare policy denial and trap-observer behavior.
$ cargo fuzz run policy_no_diff --sanitizer=none --no-trace-compares -- -max_total_time=3600 -jobs=4

# Running the `trap_no_diff` target to compare fixed trap fixtures.
$ cargo fuzz run trap_no_diff --sanitizer=none --no-trace-compares -- -max_total_time=3600 -jobs=4
```

Note that `--sanitizer=none` and `--no-trace-compares` are always recommended to use because the sanitizer is not useful for our use case plus this will speed up the fuzzing by like multiple times.

### Reproduce errors

If the fuzzer encounters an error, libFuzzer writes the crashing input to `fuzz/artifacts/<target>/...`.
You can replay that input against the native Rust helpers with:

```
WASM_BINARY_PATH=fuzz/artifacts/no_diff/crash-... \
  cargo test --manifest-path internal/integration_test/fuzz/fuzz/Cargo.toml --test native_replay rerun_failed_native_parity_case -- --exact --nocapture

WASM_BINARY_PATH=fuzz/artifacts/validation/crash-... \
  cargo test --manifest-path internal/integration_test/fuzz/fuzz/Cargo.toml --test native_replay rerun_failed_native_validation_case -- --exact --nocapture

FUZZ_INPUT_PATH=fuzz/artifacts/policy_no_diff/crash-... \
  cargo test --manifest-path internal/integration_test/fuzz/fuzz/Cargo.toml --test native_replay rerun_failed_native_policy_case -- --exact --nocapture

FUZZ_INPUT_PATH=fuzz/artifacts/trap_no_diff/crash-... \
  cargo test --manifest-path internal/integration_test/fuzz/fuzz/Cargo.toml --test native_replay rerun_failed_native_fixed_trap_case -- --exact --nocapture
```

`cargo fuzz tmin` still works to minimize the crashing input while preserving the native Rust failure.
