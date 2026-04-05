# se-wazero Roadmap Review — April 5, 2026

## Overall: ~45-50% complete across all workstreams

| Workstream | Progress | Summary |
|---|---|---|
| **1. Foundation & threat model** | ~90% | THREAT_MODEL.md, error/exit codes, benchmark scaffolding all done. Only regression baselines for trap overhead are missing. |
| **2. Memory sandboxing** | ~80% | mmap abstraction (unix/windows/fallback), `GuardPageAllocator`, `WithSecureMode` API, and unit tests are all in place. Missing end-to-end Wasm module OOB trap integration tests. |
| **3. Fuel metering** | ~70% | Core infrastructure is solid — `FuelController` interface, SSA injection at function entry + loop back-edges, config API, and call engine wiring all exist. **Zero tests or benchmarks** (explicitly noted in `phase-2-tasks.md`). Interpreter unsupported. |
| **4. Async yield/resume** | ~15% | Only a basic `Snapshot`/`Restore` prototype with tests. No yield protocol, no stack unwinding, no state capture for locals/value stack/IP, no async host call integration. |
| **5. Zero-trust host interface** | ~5% | Placeholder error/exit codes defined but nothing wired up. No default-deny FS, no egress policy, no timer jitter. All upstream-only behavior. |
| **6. Validation & hardening** | ~15% | Secure mode is opt-in, threat model documented, some unit tests exist. Missing fuel tests, e2e memory fault tests, fuzzing, and observability hooks. |

### Key strengths

- The foundational layer (workstream 1) is essentially done
- Memory sandboxing is the most mature implementation workstream — the mmap + guard page approach is functional on Linux/Windows with a clean fallback story
- Fuel metering compiler infrastructure is in place, just needs validation

### Biggest gaps

- **No tests for fuel** — this is the most urgent gap since the code exists but is unvalidated
- **Async yield/resume is barely started** — snapshot prototype ≠ the cooperative suspension + stack unwinding the roadmap calls for
- **Zero-trust host interface is entirely unimplemented** — filesystem, network, and clock hardening are all still on the drawing board
- **No end-to-end integration tests** for any of the security features (only unit-level mmap tests)

The project is roughly halfway through the first three workstreams, and hasn't meaningfully started workstreams 5 and 6.
