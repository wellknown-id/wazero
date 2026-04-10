# razero roadmap

`razero` is an rust based experimental fork of go based wazero focused on running untrusted WebAssembly workloads in multi-tenant environments with stronger isolation and more deterministic resource controls.

This document turns the rough specification into an initial roadmap. It is intentionally ambitious and should be treated as a staged research and implementation plan, not a promise of short-term delivery.

Note: this roadmap encompasses work commencing from commit 7f2e5f44e791c45714ab233d3cff48bad9818168.

## Goals

- Preserve wazero's core strengths: pure Go, easy embedding, no mandatory CGO, and cross-compilation friendliness.
- Improve safety for untrusted and multi-tenant workloads.
- Prefer fail-closed behavior when the runtime cannot safely support a feature on a target platform.
- Introduce security features incrementally so each stage can be validated independently.

## Non-goals for the first phase

- Full feature parity across every operating system and architecture supported by upstream wazero.
- Replacing all existing software safety checks immediately.
- Delivering production-ready multi-tenant isolation in a single release.

## Design constraints

- Prefer standard library `syscall` and existing `golang.org/x/sys` usage over CGO.
- Keep Linux `amd64` and `arm64` as the primary initial target for security-sensitive features.
- Preserve a clear fallback story when hardware trapping or other platform behavior is unavailable or inconsistent.
- Keep security policy decisions explicit and host-controlled.

## Workstreams

### Track view

Use this table as the quick side-by-side status board. The detailed workstream
sections below remain the canonical item lists and should continue to carry the
commit-backed progress markers for each track.

| Workstream                                          | Go track                                                                                                                                             | Rust track                                                                                                                    |
| --------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| 1. Foundation and threat model                      | [95%:bbe2d375] Threat model and trap taxonomy are largely defined; support matrix and benchmark baselines still need more work.                      | [30%:763f642b] Rust has runtime/error scaffolding plus secure-mode, fuel, and yield surfaces, but still lacks Rust-specific threat/support docs and benchmark baselines. |
| 2. Hardware-assisted memory sandboxing              | [100%:dedf9a6c] Virtual memory reservation and `mmap` guard-page prototype are in place; portability/fallback work remains.                          | [99%:cb254623] Guarded allocation and Linux-first reserved-memory paths exist in Rust, secure-mode compiler codegen now honors memory isolation, guarded-memory SIGSEGV handling reports `MEMORY_FAULT`, compiler-backed trap observation now distinguishes secure-mode memory faults from software OOB traps on supported targets, the live store path now falls back to plain memory on unsupported guard-page platforms, guarded allocations now reserve-to-max while committing/growing only live pages, both public and compiler-backed post-grow OOB accesses still trap on the expected path, secure-mode imported host callbacks now reject OOB writes while compiler-backed OOB reads return `None` and writes fail through the public memory API, including typed `read_u32_le`/`write_u32_le`, `read_u64_le`/`write_u64_le`, `read_f32_le`/`write_f32_le`, and `read_f64_le`/`write_f64_le` bounds validation, public `pages()` accessor ratio validation, and `grow()` return-value plus maximum-pages boundary validation, and compiler lowering validation now covers full-word `i32.load`, `i64.load`, `f32.load`, `f64.load`, `i32.store`, `i64.store`, `f32.store`, and `f64.store` plus subword `i32.load8_s`, `i32.load8_u`, `i32.load16_s`, `i32.load16_u`, `i32.store8`, `i32.store16`, `i64.load8_s`, `i64.load8_u`, `i64.load16_s`, `i64.load16_u`, `i64.load32_s`, `i64.load32_u`, `i64.store8`, `i64.store16`, and `i64.store32`, with both local bounds-check and memory-isolation validation now covering the remaining i32 and i64 subword paths; wider platform hardening and native arm64 validation still lag. |
| 3. Deterministic CPU metering ("fuel")              | [90%:dedf9a6c] Compiler-side fuel metering and exhaustion traps are in place; host fuel APIs and validation depth still need work.                   | [60%:3a506bff] Fuel controllers, host fuel APIs, exhaustion handling, and compiler function-entry plus loop-header metering now exist in Rust and are wired through the production compiler path; interpreter execution now also debits native function entry and backward branches while preserving shared fuel state for guest-call/resume host access, but basic-block injection and deeper nested-call/resume validation remain incomplete. |
| 4. Async yield and resume                           | [90%:dedf9a6c] Yield protocol, suspension, resume semantics, and broad Go validation are in place; suspension invariants still need tightening.      | [78%:4888107a] Yield/resume protocol, resumers, cancellation, and cross-thread resume work in Rust runtime tests, the runtime now exposes `YieldPolicy`, wires it through runtime config/context, enforces it at cooperative-yield boundaries, and covers context overrides plus direct host-export denial, and public resumer/module state now rejects suspended-module reentry, stale/spent resumers, and wrong host-result arity while validating fresh resumer handoff across re-yield; deeper compiler-native suspend/restore validation still remains. |
| 5. Zero-trust host interface                        | [65%:dedf9a6c] Direction is set, but the Go core engine is not yet fully stripped of system/OS coupling.                                             | [77%:699d0564] Rust core crates keep WASI/system behavior outside the runtime and imports embedder-owned, and now have import ACLs, fail-closed resolution, observer/audit events that distinguish resolver attempts from plain fallback, plus `HostCallPolicy` and `YieldPolicy` surfaces with host-function and caller-module metadata, signature-based and caller-aware enforcement coverage at guest and direct host-call boundaries including cooperative-yield context, and end-to-end ACL + host-call + yield-policy composition, with caller-module metadata now flowing through direct host-export HostCallPolicy enforcement alongside imported-function calls, both policy request types exposing function identity, caller-module identity, signature/import metadata accessors, and memory-definition metadata accessors for direct policy checks, with runtime wiring now carrying guest memory metadata into both imported-function HostCallPolicy checks and cooperative-yield policy decisions, function and memory definitions exposing module-name and import metadata accessors, and a global-definition metadata surface for value-type, mutability, module/import, and export-name inspection; broader host policy surfaces still remain. |
| 6. Validation, hardening, and operational readiness | [85%:16b42271] Extensive Go `experimental/` coverage now exists for memory faults, fuel exhaustion, async resumption, trap causes, and policy flows. | [99%:d95ef3d9] Rust has parity/spec/smoke tests, listener surfaces, benches, packaging ABI docs, stronger policy/fault-path coverage including `HostCallPolicy`/`YieldPolicy` runtime-config and context override cases plus direct host-export and cooperative-yield denial paths, compiler-backed secure-mode OOB validation across scalar and subword load/store paths, an experimental trap observer API with compiler-backed trap notifications plus public policy-denied notifications across direct host-call and yield/resume paths, distinct supported-target secure-mode `MEMORY_FAULT` classification, and broader real AMD64 codegen coverage including scalar float comparison, arithmetic, `sqrt`, scalar rounding (`ceil`/`floor`/`trunc`/`nearest`), scalar `min/max` with NaN/signed-zero handling, scalar `abs`/`neg`/`copysign`, scalar reinterpret/bitcast (`i32<->f32`, `i64<->f64`), signed and unsigned `i32/i64 -> f32/f64` conversion, signed and unsigned `i32 <- f32/f64` truncation plus signed and unsigned `i64 <- f32/f64` truncation with invalid-conversion and overflow trap exits, `i32.wrap_i64`, signed and unsigned `i64.extend_i32`, signed `i32.extend8/16_s`, signed `i64.extend8/16/32_s`, and float promotion/demotion, general integer comparisons, signed and unsigned `div/rem` with zero-divisor and overflow handling, direct and conditional trap exits, address-path `UExtend`, integer select, partial-load extension, unary bit ops, bitwise ops, and shift/rotate lowering, but broader hardening depth and fallback breadth still remain behind Go. |
| 7. Rust-port AOT packaging and native distribution  | —                                                                                                                                                    | [99%:219180bf] Strong Linux ELF/AOT packaging path exists, and package-bundle deserialization now has core negative coverage for invalid magic, truncated headers, numeric fields, module names, module-name-length and module-name-bytes, invalid UTF-8 in module names, guest module names, import modules, import names, and host symbol names, empty module names, empty guest module names, empty import modules, empty import names, empty host symbol names, positive max function-index and type-index round-trip coverage, and zero-length module sidecars plus truncated module sidecars and sidecar-length fields, truncated host-import count, guest-module-name-length and guest-module-name-bytes, function-index (full and partial), type-index (full and partial), symbol-name-length and symbol-name-bytes, import-module-name-length and import-module-name-bytes, and import-name-length and import-name-bytes fields, and unexpected trailing bytes; broader runtime-state packaging hardening still remains. |

**Tracking rule:** use the Go column for Go-runtime progress, the Rust column
for Rust-port progress, and leave a column as `—` until that track has been
explicitly assessed for the corresponding workstream.

### 1. Foundation and threat model

Establish the baseline needed to evaluate all later work.

- [95%:dedf9a6c] Document the threat model for untrusted tenant code, host functions, and shared infrastructure.
- [80%:dedf9a6c] Define supported and unsupported security properties for each runtime mode and platform.
- [85%:dedf9a6c] Identify the wazero subsystems that will change first: memory management, compiler backends, and trap handling.
- [97%:156dfeb5] Define new error and trap categories for memory faults, fuel exhaustion, policy denials, and async yield/resume transitions.
- [80%:dedf9a6c] Add benchmark and regression baselines for compile time, execution time, memory growth, and trap overhead.

### 2. Hardware-assisted memory sandboxing

Shift linear memory protection toward OS-backed virtual memory primitives.

- [100%:dedf9a6c] Introduce an abstraction for reserving large virtual address ranges for Wasm linear memory.
- [100%:dedf9a6c] Prototype `mmap`-backed memory reservation with committed active pages and inaccessible guard pages.
- [90%:156dfeb5] Translate page faults caused by out-of-bounds access into recoverable Wasm traps using Go runtime fault recovery facilities where supported.
- [95%:dedf9a6c] Keep software bounds checks or other safe fallbacks where hardware trapping is not yet reliable.
- [75%:dedf9a6c] Start with Linux `amd64` and `arm64`, then evaluate portability gaps before expanding platform support.

**Exit criteria**

- [80%:156dfeb5] A tenant memory fault terminates the offending module instance without crashing the host process on supported targets.
- [85%:dedf9a6c] Unsupported targets clearly fall back to an explicitly documented mode.

### 3. Deterministic CPU metering ("fuel")

Add deterministic execution budgeting to compiled and interpreted execution paths.

- [35%:dedf9a6c] Define a cost model for Wasm instructions that is simple enough to reason about and stable enough to expose to embedders.
- [90%:dedf9a6c] Inject fuel accounting at function entries, loop headers, and other control-flow boundaries chosen for acceptable overhead.
- [90%:dedf9a6c] Add a trap path for resource exhaustion that does not depend on wall-clock deadlines.
- [70%:dedf9a6c] Expose safe host APIs to inspect, add, and subtract fuel for a running instance.
- [45%:dedf9a6c] Validate that compiler overhead, execution overhead, and accounting accuracy are acceptable for the experiment.

**Exit criteria**

- [80%:dedf9a6c] Long-running or infinite Wasm execution can be stopped deterministically through fuel exhaustion.
- [65%:dedf9a6c] Hosts can safely recharge or debit fuel during execution without breaking isolation guarantees.

### 4. Async yield and resume

Introduce cooperative suspension for Wasm execution that waits on host-side asynchronous work.

- [90%:dedf9a6c] Define a yield protocol between compiled Wasm code and host functions.
- [80%:dedf9a6c] Extend the compiler pipeline with a stack-unwinding and state-capture strategy for resumable execution.
- [90%:dedf9a6c] Reserve storage for saved locals, value stack state, and resume instruction pointers.
- [85%:dedf9a6c] Implement resume semantics that restore execution exactly at the suspended point.
- [75%:4888107a] Establish invariants for what host functions may do while a module is suspended, including rejected reentry, stale/spent resumers, and host-result arity validation across re-yield.

**Exit criteria**

- [85%:dedf9a6c] A module can yield on supported async host calls and later resume without restarting execution.
- [80%:dedf9a6c] Suspended modules do not require a permanently blocked Go thread per waiting tenant.

### 5. Zero-trust host interface

Harden the runtime by remaining completely unopinionated about system functionality.

- [65%:dedf9a6c] Remove all WASI implementation code, OS dependencies, and system-level `Context` structures from the core engine.
- [65%:dedf9a6c] Establish an architecture where embedders are strictly responsible for providing any required host interfaces, including filesystems, network egress policies, and clock resolution controls.
- [40%:dedf9a6c] Explicitly fail-closed by removing platform-specific adapter layers and system wrappers that bypass host application policies.

**Exit criteria**

- [15%:dedf9a6c] The core engine contains absolutely zero system or OS coupling.
- [80%:dedf9a6c] WASI layers (P1/P2) must be supplied strictly externally by the embedder.

### 6. Validation, hardening, and operational readiness

Turn prototypes into an experimental runtime that can be evaluated seriously.

- [85%:16b42271] Add targeted tests for memory fault recovery, fuel exhaustion, async resumption, and policy enforcement.
- [10%:dedf9a6c] Expand fuzzing and negative testing around host interfaces and trap paths.
- [75%:1baa1d65] Document platform limitations, performance tradeoffs, and security assumptions.
- [70%:16b42271] Add observability hooks for trap causes, fuel usage, yield counts, and policy denials.
- [95%:dedf9a6c] Keep the secure mode opt-in until behavior and compatibility are well understood.

### 7. Rust-port AOT packaging and native distribution

Track the Rust port as a first-class deliverable, not as an afterthought to the
Go runtime. The goal is to keep the core runtime small, keep WASI out of core
crates, preserve interpreter availability for no-JIT environments, and support
native packaging for embedders that want a fully linked executable.

#### Current implemented state

- [100%:04af62a8] The Rust runtime already supports explicit **interpreter** and **compiler**
  modes.
- [100%:04af62a8] `razero` now supports **precompiled artifacts** through
  `build_precompiled_artifact`, `compile_precompiled_artifact`, and
  `instantiate_precompiled_artifact`, so `.wasm` can be AOT-prepared and later
  loaded without recompiling at runtime.
- [100%:04af62a8] `razero-compiler` now preserves a richer **AOT metadata** sidecar describing:
  target, function metadata, relocations, function signatures, module shape,
  import descriptors, memory/table/global metadata, module-context layout, and
  execution-context/helper ABI details.
- [100%:04af62a8] `CompiledModule::emit_relocatable_object()` now emits
  **Linux ELF relocatable objects** plus Razero metadata sidecar.
- [100%:04af62a8] `razero_compiler::runtime_support::LinkedModule` provides a
  metadata-driven linked startup/call surface for Linux ELF AOT modules with the
  supported runtime-state slice.
- [95%:04af62a8] `razero_compiler::linker::link_native_executable(...)` now packages one or
  more relocatable Wasm objects into a native executable for the current C ABI-first
  surface.
- [100%:ea77dc43] `razero_compiler::linker::link_hello_host_executable(...)` now provides a
  **specialized** native-link flow for the existing `hello-host` example,
  including its single explicit `(i32, i32) -> ()` host import and local memory
  setup.
- [100%:04af62a8] The current packaging flow emits a package metadata bundle alongside the
  executable (`.razero-package`) and is covered by end-to-end Rust tests.

#### Product shape we should preserve

- [100%:04af62a8] **Interpreter runtime mode remains required.** Packaged native executables do
  not replace the interpreter; they are a separate AOT deployment target.
- [100%:04af62a8] **WASI stays out of the core crates.** Host APIs remain embedder-defined and
  must be linked or supplied explicitly.
- [100%:04af62a8] **Initial packaged host ABI remains C ABI first.**
- [100%:04af62a8] **Linux ELF remains the first shipping object/executable target**, with
  x86_64 and AArch64 now implemented.

#### What is still incomplete

- [60%:04af62a8] The **generic** native-link path is still intentionally narrower than a fully
  general packaging/runtime product:
  - [80%:04af62a8] Linux ELF only
  - [70%:04af62a8] exported-function-oriented C ABI wrappers
  - [70%:04af62a8] scalar C ABI-compatible signatures only
  - [60%:04af62a8] not every host/import/runtime shape is generalized yet
- [85%:04af62a8] `hello-host` remains available as a **specialized convenience path**, but no
  longer defines the only host-ABI/runtime-support route.
- [55%:04af62a8] There is not yet a fully stable, general-purpose packaging story for:
  - [80%:04af62a8] arbitrary host imports
  - [75%:04af62a8] multiple linked Wasm modules with cross-module runtime state
  - [80%:04af62a8] modules with memory/table/global requirements beyond the specialized paths
  - [100%:04af62a8] AArch64 object emission and native packaging
- [95%:04af62a8] The package metadata format is now treated as a versioned product surface with
  explicit compatibility guarantees.

#### Work still required to finish this properly

1. **Generalize host-import packaging**
   - [90%:04af62a8] Replace the `hello-host` special case with a reusable host-ABI/runtime
     support layer.
   - [90%:04af62a8] Make imported functions resolve through explicit packaged host descriptors
     instead of generated ad hoc example logic.
   - [90%:04af62a8] Keep host ownership explicit: the packager should wire host APIs supplied by
     the embedder, not smuggle system functionality back into core crates.

2. **Generalize runtime-state packaging**
   - [85%:04af62a8] Support packaged modules that need memory, globals, tables, start functions,
     and data/element initialization without relying on hand-written per-example
     startup code.
   - [85%:04af62a8] Promote the current metadata-driven startup assumptions into a real runtime
     support contract.

3. **Stabilize the packaging ABI**
   - [95%:04af62a8] Treat execution-context layout, module-context layout, helper IDs, symbol
     names, sidecar schema, and `.razero-package` contents as versioned ABI.
   - [95%:04af62a8] Document what is private vs link-visible so future compiler/runtime changes
     do not silently break packaged artifacts.

4. **Widen target coverage**
   - [100%:04af62a8] Add AArch64 relocatable object emission and packaging.
   - [90%:04af62a8] Decide later whether Mach-O/COFF are in scope, but do not block Linux/ELF
     hardening on that decision.

5. **Decide crate/product boundaries**
   - [90%:04af62a8] Keep `razero` focused on embedding/runtime APIs.
   - [95%:04af62a8] Keep `razero-compiler` focused on codegen, metadata, object emission, and
     linker support unless and until a separate `razero-aot` / `razero-pack`
     crate becomes warranted.
   - [85%:04af62a8] Preserve `razero-ffi` as a possible stable static-lib integration surface,
     but do not force packaged execution to depend on a bloated monolithic FFI
     layer.

6. **Expand validation coverage**
   - [85%:04af62a8] Keep interpreter, compiler, precompiled-artifact, and native-packaged flows
     all green at once.
   - [80%:04af62a8] Add more end-to-end fixtures covering:
     - [100%:04af62a8] one Wasm module + one host static library
     - [80%:04af62a8] multiple Wasm modules
     - [85%:04af62a8] packaged explicit host imports beyond `hello-host`
   - [100%:6a7b8288] negative tests for ABI mismatches, malformed metadata, and unsupported
     target/runtime shapes

## Suggested implementation order

1. Foundation and threat model
2. Hardware-assisted memory sandboxing prototype
3. Deterministic CPU metering
4. Pure core engine (WASI decoupling)
5. Rust AOT metadata, precompiled artifacts, and Linux/ELF packaging hardening
6. Async yield and resume
7. Broader validation and platform expansion

This order prioritizes containment and deterministic limits before more invasive coroutine-style execution work.

### Future optimizations

- [45%:04af62a8] **SSA `ExitIfTrue` instruction for fuel checks**: Consider adding a first-class `ExitIfTrue(cond, exitCode)` SSA opcode to replace the current branch-to-exit-block pattern used by fuel metering at function entries and loop back-edges. A dedicated opcode would allow the SSA optimizer to reason about fuel check elimination at compile time (e.g., coalescing consecutive checks, hoisting checks out of inner loops with bounded iteration counts). This is not urgent since the current `insertFuelCheck` pattern (load/sub/store/cmp/branch-to-exit) is ~5 native instructions and already efficient, but the optimization would reduce code size and improve instruction cache utilization in fuel-heavy workloads.

## Main risks and tradeoffs

- [75%:04af62a8] Go runtime fault handling is platform-sensitive and may limit portability.
- [70%:04af62a8] Large virtual memory reservations and guard-page strategies may behave differently across kernels and operating systems.
- [30%:04af62a8] Fuel injection and async stack handling will increase compiler complexity, compile time, and runtime overhead.
- [65%:04af62a8] Strict uncoupling from system APIs means embedders must meticulously provide their own WASI or host functionality.
- [80%:04af62a8] Some features may still land compiler-first, leaving the interpreter with a narrower hardening and security profile until parity catches up.

## Near-term deliverables

- [90%:04af62a8] A written threat model and support matrix for secure mode.
- [90%:04af62a8] A Linux-first memory sandboxing prototype.
- [80%:04af62a8] A compiler prototype with fuel metering and resource exhaustion traps.
- [45%:04af62a8] A minimalist, pure WebAssembly core engine completely decoupled from system, OS, and WASI dependencies.
- [100%:04af62a8] A Linux-first Rust AOT/native-packaging path with documented ABI limits, plus
  a concrete plan to generalize host-import and runtime-state packaging.
- [90%:04af62a8] An experimental status report describing what is safe, what is incomplete, and what remains research.
