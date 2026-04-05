# se-wazero Roadmap Review — April 5, 2026 (Updated)

## Overall: ~48-53% complete across all workstreams

| Workstream | Progress | Summary |
|---|---|---|
| **1. Foundation & threat model** | ~90% | THREAT_MODEL.md, error/exit codes, benchmark scaffolding all done. Only regression baselines for trap overhead are missing. |
| **2. Memory sandboxing** | ~80% | mmap abstraction (unix/windows/fallback), `GuardPageAllocator`, `WithSecureMode` API, and unit tests are all in place. Missing end-to-end Wasm module OOB trap integration tests. |
| **3. Fuel metering** | ~85% | Core infrastructure is solid — `FuelController` interface, SSA injection at function entry + loop back-edges, config API, and call engine wiring all exist. **Tests and benchmarks are now complete**: 7 integration tests in `fuel_test.go` (all passing), `FuelController` unit tests, and overhead benchmarks in `secbench/`. All `phase-2-tasks.md` items checked off. Interpreter still unsupported. |
| **4. Async yield/resume** | ~15% | Only a basic `Snapshot`/`Restore` prototype with tests. No yield protocol, no stack unwinding, no state capture for locals/value stack/IP, no async host call integration. |
| **5. Zero-trust host interface** | ~5% | Placeholder error/exit codes defined but nothing wired up. No default-deny FS, no egress policy, no timer jitter. All upstream-only behavior. |
| **6. Validation & hardening** | ~20% | Secure mode is opt-in, threat model documented, fuel tests now passing. Still missing e2e memory fault tests, fuzzing, and observability hooks. |

### Key strengths

- The foundational layer (workstream 1) is essentially done
- Memory sandboxing is the most mature implementation workstream — the mmap + guard page approach is functional on Linux/Windows with a clean fallback story
- Fuel metering is now fully implemented, tested, and benchmarked — the compiler path is production-ready for experimentation

### Biggest gaps

- **Async yield/resume is barely started** — snapshot prototype ≠ the cooperative suspension + stack unwinding the roadmap calls for
- **Zero-trust host interface is entirely unimplemented** — filesystem, network, and clock hardening are all still on the drawing board
- **No end-to-end integration tests** for memory fault-to-Wasm-trap translation (platform tests use `debug.SetPanicOnFault` but don't verify the full Wasm trap path)
- **Interpreter lacks fuel support** — fuel metering is compiler-only

### Update since last review (April 5, second review)

A second review was conducted after the initial assessment. **No new progress was found** — all percentages remain unchanged. The prior review's conclusions were confirmed through deeper investigation of each workstream's implementation files, tests, and benchmarks.

### Update since last review (April 5)

- Fuel metering moved from ~70% to ~85% — all tests and benchmarks from `phase-2-tasks.md` are now implemented and passing. This was the most urgent gap at the time of the prior review.
- Validation workstream (6) nudged from ~15% to ~20% due to fuel tests landing.
- Overall progress moved from ~45-50% to ~48-53%.

The first three workstreams are now substantially complete. Workstreams 5 and 6 remain largely untouched.
