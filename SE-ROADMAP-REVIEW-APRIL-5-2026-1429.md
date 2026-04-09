# se-wazero Roadmap Review — April 5, 2026 (Updated)

## Overall: ~52-57% complete across all workstreams

| Workstream                       | Progress | Summary                                                                                                                                                                                                                                                                                                                                                         |
| -------------------------------- | -------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **1. Foundation & threat model** | ~92%     | THREAT_MODEL.md, support matrix framing, error/exit code taxonomy, and baseline scaffolding are in place. The trap model now includes real hardware-fault conversion in the compiler path on Linux/amd64. Remaining work is mostly benchmark/regression depth and docs tightening.                                                                              |
| **2. Memory sandboxing**         | ~90%     | mmap abstraction (unix/windows/fallback), `GuardPageAllocator`, `WithSecureMode` API, and unit tests are in place. Linux/amd64 now has a custom SIGSEGV path that converts JIT guard-page faults into recoverable Wasm out-of-bounds traps, with executable-range registration and secure-mode capability gating. Arm64 parity for fault handling remains open. |
| **3. Fuel metering**             | ~85%     | Core infrastructure is solid — `FuelController` interface, SSA injection at function entry + loop back-edges, config API, and call engine wiring all exist. Tests and benchmarks are complete: integration tests in `fuel_test.go`, `FuelController` unit tests, and overhead benchmarks in `secbench/`. Interpreter remains unsupported.                       |
| **4. Async yield/resume**        | ~15%     | Only a basic `Snapshot`/`Restore` prototype with tests. No yield protocol, no stack unwinding, no state capture for locals/value stack/IP, no async host call integration.                                                                                                                                                                                      |
| **5. Zero-trust host interface** | ~5%      | Placeholder error/exit codes exist but no policy wiring yet. No default-deny FS, no egress policy layer, and no WASI clock precision hardening.                                                                                                                                                                                                                 |
| **6. Validation & hardening**    | ~30%     | Secure mode remains opt-in, and memory fault handling now has an end-to-end integration test (`TestSecureMode_HardwareFaultToTrap`) validating Wasm-visible OOB trap behavior in both secure and non-secure configurations. Still missing fuzzing/negative testing expansion, observability hooks, and broader platform hardening.                              |

### Key strengths

- Workstreams 1-3 are substantially complete enough for serious experimental use in the compiler path
- Memory sandboxing now includes Linux/amd64 end-to-end hardware fault to Wasm trap conversion instead of panic-on-fault style behavior
- Fuel metering is implemented, tested, and benchmarked with good coverage for deterministic budgeting workflows
- Secure-mode behavior is now more fail-closed: capability-gated enablement and explicit fallback on unsupported targets

### Biggest gaps

- **Arm64 secure-mode fault handling parity is missing** — Linux/amd64 trap conversion is implemented, but arm64 still needs equivalent signal/fault path work
- **Async yield/resume is barely started** — snapshot prototype is not yet cooperative suspension with full state capture and resume semantics
- **Zero-trust host interface is largely unimplemented** — filesystem, network, and clock hardening remain roadmap items
- **Interpreter lacks fuel support** — fuel metering is still compiler-only
- **Validation depth is still limited** — more fuzzing, adversarial test coverage, and observability are needed before broader confidence

### Update since last review (April 5, third review)

- Memory sandboxing moved from ~80% to ~90% due to Linux/amd64 SIGSEGV-to-trap integration for JIT guard-page faults, including executable-range registration and trap return flow.
- Validation moved from ~20% to ~30% due to end-to-end secure-mode memory fault integration testing (`TestSecureMode_HardwareFaultToTrap`) that verifies Wasm-visible OOB trap behavior.
- Backend plumbing was cleaned up for workstreams 1-3: compiler-level `DisableStackCheck` propagation and machine entry preamble API parity (`CompileEntryPreamble(..., useGoStack bool)`) across architectures.
- Overall progress moved from ~48-53% to ~52-57%.

### Update since last review (April 5)

- Fuel metering had already moved from ~70% to ~85% earlier on April 5 after landing tests and benchmarks from `phase-2-tasks.md`.
- This latest refresh adds the memory fault-to-trap milestone and corresponding validation updates.

The first three workstreams remain the strongest area. Workstreams 4 and 5 are still the major implementation gap.
