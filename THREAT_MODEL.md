# se-wazero Threat Model

This document describes the security assumptions, threat boundaries, and mitigations that se-wazero provides for running untrusted WebAssembly workloads in multi-tenant Go processes.

## Actors

### Untrusted tenant code

WebAssembly modules provided by tenants. These modules are assumed to be adversarial: they may attempt to read or write memory outside their linear memory, exhaust CPU or memory resources, escape filesystem sandboxing, perform timing side-channel attacks, or interfere with other tenants.

### Trusted host code

Go code that embeds se-wazero. Host functions registered via `api.GoModuleFunction` or `api.GoFunction` are trusted. Security bugs in host function implementations are outside the scope of this model but the runtime should limit the damage a buggy host function can cause.

### Shared infrastructure

The wazero `Store`, `Engine`, compilation caches, and type registries are shared across module instances within a single `Runtime`. These are assumed to be correctly implemented and are part of the trusted computing base.

## Trust boundaries

```
┌──────────────────────────────────────────────┐
│                  Host Process                │
│                                              │
│  ┌────────────┐    ┌────────────┐            │
│  │ Tenant A   │    │ Tenant B   │            │
│  │ Module     │    │ Module     │            │
│  │            │    │            │            │
│  │ Linear Mem ├────┤ Linear Mem │  ISOLATED  │
│  │ Tables     │    │ Tables     │            │
│  │ Globals    │    │ Globals    │            │
│  └─────┬──────┘    └──────┬─────┘            │
│        │ host calls       │                  │
│  ┌─────▼──────────────────▼─────┐            │
│  │        Host Functions        │  TRUSTED   │
│  │ (embedder-supplied imports)  │            │
│  └─────────────┬────────────────┘            │
│                │                             │
│  ┌─────────────▼────────────────┐            │
│  │     Engine / Store / OS      │  TRUSTED   │
│  └──────────────────────────────┘            │
└──────────────────────────────────────────────┘
```

**Boundary 1 — Module ↔ Linear Memory**: Each module instance has its own linear memory. In standard mode, bounds are checked in software. In secure mode, wazero prefers guard-page-backed linear memory on `unix` / `windows` targets, and the compiler's Linux `amd64` / `arm64` secure-mode path can enforce bounds via hardware faults instead of the normal checked path.

**Boundary 2 — Module ↔ Host Functions**: Host function calls cross from compiled/interpreted Wasm into Go code. Arguments are passed via a value stack. The module cannot influence which host functions are called except via its declared imports.

**Boundary 3 — Module ↔ Tables**: Indirect calls through `call_indirect` are type-checked at runtime using `FunctionTypeID` comparisons. A type mismatch traps the module.

**Boundary 4 — Module ↔ Embedder-provided host interfaces**: Filesystem, network, clock, random, and similar system-facing capabilities are not part of the core engine. They are only reachable when the embedder supplies host imports (for example, a WASI layer or custom host modules). The security boundary is therefore between untrusted Wasm and the embedder's trusted policy layer, not between the module and a built-in runtime-owned WASI subsystem.

## Threat categories

### T1 — Memory corruption (out-of-bounds read/write)

**Attack**: A Wasm module attempts to read or write memory outside its allocated linear memory, potentially accessing host process memory or another tenant's data.

**Mitigation (standard mode)**: Software bounds checks on every memory access (both interpreter and compiler paths). An out-of-bounds access returns `false` from `MemoryInstance.hasSize()` or triggers `ErrRuntimeOutOfBoundsMemoryAccess`.

**Mitigation (secure mode)**: On `unix` / `windows` targets, linear memory can be backed by a large reservation with a 4 GiB guard region. On the compiler's Linux `amd64` / `arm64` secure-mode path, out-of-bounds accesses are converted into Wasm traps by the custom signal-handler fault path, so basic load/store instructions do not need the normal software bounds checks. When execution reaches that hardware-backed trap path, the runtime surfaces `ErrRuntimeMemoryFault`; software-checked paths continue to report `ErrRuntimeOutOfBoundsMemoryAccess`. On other targets, secure mode falls back to the checked execution path. See [SUPPORT_MATRIX.md](SUPPORT_MATRIX.md) for the exact runtime-mode and platform matrix.

### T2 — Resource exhaustion (CPU)

**Attack**: A Wasm module enters an infinite loop or performs excessive computation, consuming host CPU indefinitely and starving other tenants.

**Current mitigation**: `WithCloseOnContextDone(true)` inserts periodic exit-code checks at loop headers and function entries. Combined with `context.WithTimeout`, this terminates runaway modules. However, this relies on wall-clock time, not deterministic instruction counting.

**Mitigation (fuel metering, compiler path)**: Deterministic fuel metering injects fuel counters at function entries and loop back-edges. Fuel exhaustion triggers `ErrRuntimeFuelExhausted` without relying on wall-clock timing. Host functions can inspect remaining fuel via `experimental.RemainingFuel()` and recharge via `experimental.AddFuel()`. Multi-tenant budgets are supported via `FuelController` and `AggregatingFuelController`.

### T3 — Resource exhaustion (memory growth)

**Attack**: A Wasm module calls `memory.grow` repeatedly to exhaust host process virtual memory or physical RAM.

**Mitigation**: `WithMemoryLimitPages` caps the maximum pages per memory instance. When secure mode is using guard-page-backed linear memory, the entire max reservation is virtual, so uncommitted pages consume no physical RAM. Growing memory only commits additional pages via `mprotect` / `VirtualAlloc`.

### T4 — Host filesystem policy escape

**Attack**: A Wasm module uses embedder-provided filesystem imports (including WASI-style `path_open`, `fd_read`, `fd_write`, etc.) to read or write files outside its designated sandbox, or to traverse upward via `../` sequences.

**Current mitigation**: The core runtime does not provide a built-in filesystem policy surface. Filesystem exposure exists only if the embedder installs a host module for it, so confinement depends on that host module's mount rules, path normalization, and fail-closed behavior.

**Architectural direction**: Keep filesystem policy outside the core runtime. Any default-deny filesystem, explicit allowlists, or traversal hardening should live in the embedder-supplied host layer.

### T5 — Host network policy escape

**Attack**: A Wasm module uses embedder-provided network imports to connect to arbitrary endpoints, exfiltrate data, or perform SSRF-style attacks.

**Current mitigation**: The core runtime does not provide a built-in network surface. Network access exists only if the embedder explicitly wires in host imports for it, so egress controls are the embedder's responsibility.

**Architectural direction**: Keep egress policy outside the core runtime. Destination filtering, per-tenant network policy, and listener exposure belong in the embedder-supplied host layer.

### T6 — Timing side channels

**Attack**: A Wasm module uses embedder-provided clock or timing imports (including WASI-style `clock_time_get`) to perform timing-based side-channel attacks (e.g., cache timing, speculative execution probing).

**Current mitigation**: The core runtime does not provide a built-in clock policy surface. Timing resolution is therefore determined by whichever host imports the embedder exposes.

**Architectural direction**: Keep timer policy outside the core runtime. Any clock coarsening, jitter injection, or deterministic/fake time source should live in the embedder-supplied host layer.

### T7 — Cross-module data leakage

**Attack**: One module instance reads data belonging to another module instance through shared memory, shared tables, or global state.

**Mitigation**: Each `ModuleInstance` has its own linear memory, tables, and globals. Memory sharing only occurs when explicitly configured via the WebAssembly threads proposal (`shared` memory). In secure mode, each module's mmap reservation is at a distinct virtual address — hardware page protections prevent cross-tenant access even in the presence of compiler bugs.

### T8 — Indirect call type confusion

**Attack**: A Wasm module crafts table entries to invoke functions with mismatched signatures, potentially corrupting the call stack or accessing wrong data.

**Mitigation**: `call_indirect` performs runtime type checking via `FunctionTypeID` comparison. A mismatch triggers `ErrRuntimeIndirectCallTypeMismatch` (a trap). This is enforced in both interpreter and compiler paths.

## Security property matrix

For the operational support, fallback, and validation status by runtime mode and
platform, see [SUPPORT_MATRIX.md](SUPPORT_MATRIX.md).

| Property | Linux/amd64 compiler | Linux/arm64 compiler | Other compiler-supported targets | Interpreter / non-compiler |
| --------------------------------------- | -------------------- | -------------------- | -------------------------------- | -------------------------- |
| Software bounds checks (standard mode)  | ✅                   | ✅                   | ✅                               | ✅                         |
| Guard-page-backed linear memory         | ✅ secure mode       | ✅ secure mode       | ✅ secure mode on `unix` / `windows` | ✅ secure mode on `unix` / `windows` |
| Hardware fault to Wasm OOB trap path    | ✅                   | ✅ code path; native validation pending | ❌ software-checked path   | ❌ software-checked path   |
| Context-based termination               | ✅                   | ✅                   | ✅                               | ✅                         |
| Deterministic fuel metering             | ✅                   | ✅                   | ✅                               | ❌ interpreter unsupported |
| Built-in filesystem policy layer        | n/a external host layer | n/a external host layer | n/a external host layer         | n/a external host layer   |
| Built-in network egress policy          | n/a external host layer | n/a external host layer | n/a external host layer         | n/a external host layer   |
| Built-in clock/timer policy             | n/a external host layer | n/a external host layer | n/a external host layer         | n/a external host layer   |
| Async yield/resume                      | ❌ Phase 3           | ❌ Phase 3           | ❌ Phase 3                       | ❌ Phase 3                 |
| Indirect call type checks               | ✅                   | ✅                   | ✅                               | ✅                         |

## Assumptions

1. The Go runtime is trusted and correctly implements `runtime.SetPanicOnFault`.
2. The host operating system kernel correctly enforces virtual memory protections.
3. Host functions provided by the embedder are correctly implemented and do not violate the memory safety guarantees of Go.
4. The WebAssembly module is structurally valid (passes wazero's validation phase) before execution.
5. The attack surface of the compilation cache (file-backed or in-memory) is limited to availability (filling disk), not integrity (compiled code is checksummed).
6. Any filesystem, network, clock, random, or other system-facing imports are supplied externally by the embedder and are governed by embedder policy rather than by a built-in runtime subsystem.

## Out of scope for Phase 1

- Speculative execution side channels (Spectre, Meltdown). Mitigation requires CPU microarchitectural controls beyond what a userspace runtime can enforce.
- Multi-process isolation. se-wazero runs modules in-process. For stronger isolation, use separate OS processes or containers.
- Supply chain attacks on the Wasm binary. se-wazero validates structural correctness but does not verify provenance or signing.
- Denial-of-service via compilation. Modules with pathological structure may consume excessive compile time. This will be addressed in Phase 6 (validation and hardening).
