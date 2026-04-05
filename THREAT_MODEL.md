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
│  │     (WASI, custom imports)   │            │
│  └─────────────┬────────────────┘            │
│                │                             │
│  ┌─────────────▼────────────────┐            │
│  │     Engine / Store / OS      │  TRUSTED   │
│  └──────────────────────────────┘            │
└──────────────────────────────────────────────┘
```

**Boundary 1 — Module ↔ Linear Memory**: Each module instance has its own linear memory. In standard mode, bounds are checked in software. In secure mode on supported platforms, OS virtual memory protections (guard pages) enforce bounds at the hardware level.

**Boundary 2 — Module ↔ Host Functions**: Host function calls cross from compiled/interpreted Wasm into Go code. Arguments are passed via a value stack. The module cannot influence which host functions are called except via its declared imports.

**Boundary 3 — Module ↔ Tables**: Indirect calls through `call_indirect` are type-checked at runtime using `FunctionTypeID` comparisons. A type mismatch traps the module.

**Boundary 4 — Module ↔ WASI surface**: WASI host functions mediate access to the filesystem, network, clocks, and random number generation. In standard mode, these follow upstream wazero behaviour. Secure mode will progressively tighten these in later phases.

## Threat categories

### T1 — Memory corruption (out-of-bounds read/write)

**Attack**: A Wasm module attempts to read or write memory outside its allocated linear memory, potentially accessing host process memory or another tenant's data.

**Mitigation (standard mode)**: Software bounds checks on every memory access (both interpreter and compiler paths). An out-of-bounds access returns `false` from `MemoryInstance.hasSize()` or triggers `ErrRuntimeOutOfBoundsMemoryAccess`.

**Mitigation (secure mode, supported platforms)**: Linear memory is backed by a large mmap reservation with a 4 GiB guard region of `PROT_NONE` pages. Any out-of-bounds access triggers a hardware fault (SIGSEGV on Linux, EXCEPTION_IN_PAGE_ERROR on Windows) which Go's `runtime.SetPanicOnFault` converts into a recoverable panic, translated to a Wasm trap. The 4 GiB guard region ensures that any 32-bit offset from within the linear memory base address that exceeds the committed region hits a guard page — no software bounds check is needed for basic load/store instructions.

### T2 — Resource exhaustion (CPU)

**Attack**: A Wasm module enters an infinite loop or performs excessive computation, consuming host CPU indefinitely and starving other tenants.

**Current mitigation**: `WithCloseOnContextDone(true)` inserts periodic exit-code checks at loop headers and function entries. Combined with `context.WithTimeout`, this terminates runaway modules. However, this relies on wall-clock time, not deterministic instruction counting.

**Mitigation (fuel metering, compiler path)**: Deterministic fuel metering injects fuel counters at function entries and loop back-edges. Fuel exhaustion triggers `ErrRuntimeFuelExhausted` without relying on wall-clock timing. Host functions can inspect remaining fuel via `experimental.RemainingFuel()` and recharge via `experimental.AddFuel()`. Multi-tenant budgets are supported via `FuelController` and `AggregatingFuelController`.

### T3 — Resource exhaustion (memory growth)

**Attack**: A Wasm module calls `memory.grow` repeatedly to exhaust host process virtual memory or physical RAM.

**Mitigation**: `WithMemoryLimitPages` caps the maximum pages per memory instance. In secure mode, the entire max reservation is virtual (mmap with `PROT_NONE`), so uncommitted pages consume no physical RAM. Growing memory only commits additional pages via `mprotect`.

### T4 — WASI filesystem escape

**Attack**: A Wasm module uses WASI `path_open`, `fd_read`, `fd_write` etc. to read or write files outside its designated sandbox, or to traverse upward via `../` sequences.

**Current mitigation**: Upstream wazero's `FSConfig` and `sysfs.DirFS` restrict access to configured mount points. Path normalization is performed before OS operations.

**Planned mitigation (Phase 4)**: Default-deny synthetic filesystem, explicit path allowlists, traversal protection hardening.

### T5 — WASI network escape

**Attack**: A Wasm module uses socket APIs to connect to arbitrary network endpoints, exfiltrate data, or perform SSRF attacks.

**Current mitigation**: Socket support is opt-in via `experimental/sock`. No sockets are available unless the host explicitly configures listeners.

**Planned mitigation (Phase 4)**: Egress policy layer filtering by tenant, destination, and port.

### T6 — Timing side channels

**Attack**: A Wasm module uses high-resolution clocks (`clock_time_get` with `monotonic` clock) to perform timing-based side-channel attacks (e.g., cache timing, speculative execution probing).

**Current mitigation**: By default, wazero provides fake clock implementations that return deterministic values. Real clocks require explicit opt-in via `WithSysWalltime()` / `WithSysNanotime()`.

**Planned mitigation (Phase 4)**: Configurable timer coarsening or jitter injection for WASI clock APIs in secure mode.

### T7 — Cross-module data leakage

**Attack**: One module instance reads data belonging to another module instance through shared memory, shared tables, or global state.

**Mitigation**: Each `ModuleInstance` has its own linear memory, tables, and globals. Memory sharing only occurs when explicitly configured via the WebAssembly threads proposal (`shared` memory). In secure mode, each module's mmap reservation is at a distinct virtual address — hardware page protections prevent cross-tenant access even in the presence of compiler bugs.

### T8 — Indirect call type confusion

**Attack**: A Wasm module crafts table entries to invoke functions with mismatched signatures, potentially corrupting the call stack or accessing wrong data.

**Mitigation**: `call_indirect` performs runtime type checking via `FunctionTypeID` comparison. A mismatch triggers `ErrRuntimeIndirectCallTypeMismatch` (a trap). This is enforced in both interpreter and compiler paths.

## Security property matrix

| Property | Linux amd64 (compiler) | Linux arm64 (compiler) | Windows amd64 (compiler) | Other / Interpreter |
|---|---|---|---|---|
| Software bounds checks | ✅ | ✅ | ✅ | ✅ |
| Hardware memory isolation (guard pages) | ✅ secure mode | ✅ secure mode | ✅ secure mode | ❌ software fallback |
| Context-based termination | ✅ | ✅ | ✅ | ✅ |
| Deterministic fuel metering | ✅ secure mode | ✅ secure mode | ✅ secure mode | ❌ interpreter unsupported |
| WASI default-deny filesystem | ❌ Phase 4 | ❌ Phase 4 | ❌ Phase 4 | ❌ Phase 4 |
| WASI network egress policy | ❌ Phase 4 | ❌ Phase 4 | ❌ Phase 4 | ❌ Phase 4 |
| Clock coarsening | ❌ Phase 4 | ❌ Phase 4 | ❌ Phase 4 | ❌ Phase 4 |
| Async yield/resume | ❌ Phase 3 | ❌ Phase 3 | ❌ Phase 3 | ❌ Phase 3 |
| Indirect call type checks | ✅ | ✅ | ✅ | ✅ |

## Assumptions

1. The Go runtime is trusted and correctly implements `runtime.SetPanicOnFault`.
2. The host operating system kernel correctly enforces virtual memory protections.
3. Host functions provided by the embedder are correctly implemented and do not violate the memory safety guarantees of Go.
4. The WebAssembly module is structurally valid (passes wazero's validation phase) before execution.
5. The attack surface of the compilation cache (file-backed or in-memory) is limited to availability (filling disk), not integrity (compiled code is checksummed).

## Out of scope for Phase 1

- Speculative execution side channels (Spectre, Meltdown). Mitigation requires CPU microarchitectural controls beyond what a userspace runtime can enforce.
- Multi-process isolation. se-wazero runs modules in-process. For stronger isolation, use separate OS processes or containers.
- Supply chain attacks on the Wasm binary. se-wazero validates structural correctness but does not verify provenance or signing.
- Denial-of-service via compilation. Modules with pathological structure may consume excessive compile time. This will be addressed in Phase 6 (validation and hardening).
