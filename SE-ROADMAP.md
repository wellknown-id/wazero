# se-wazero roadmap

`se-wazero` is an experimental fork of wazero focused on running untrusted WebAssembly workloads in multi-tenant environments with stronger isolation and more deterministic resource controls.

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

### 1. Foundation and threat model

Establish the baseline needed to evaluate all later work.

- [95%:dedf9a6c] Document the threat model for untrusted tenant code, host functions, and shared infrastructure.
- [80%:dedf9a6c] Define supported and unsupported security properties for each runtime mode and platform.
- [85%:dedf9a6c] Identify the wazero subsystems that will change first: memory management, compiler backends, and trap handling.
- [90%:dedf9a6c] Define new error and trap categories for memory faults, fuel exhaustion, policy denials, and async yield/resume transitions.
- [80%:dedf9a6c] Add benchmark and regression baselines for compile time, execution time, memory growth, and trap overhead.

### 2. Hardware-assisted memory sandboxing

Shift linear memory protection toward OS-backed virtual memory primitives.

- [100%:dedf9a6c] Introduce an abstraction for reserving large virtual address ranges for Wasm linear memory.
- [100%:dedf9a6c] Prototype `mmap`-backed memory reservation with committed active pages and inaccessible guard pages.
- [85%:dedf9a6c] Translate page faults caused by out-of-bounds access into recoverable Wasm traps using Go runtime fault recovery facilities where supported.
- [95%:dedf9a6c] Keep software bounds checks or other safe fallbacks where hardware trapping is not yet reliable.
- [75%:dedf9a6c] Start with Linux `amd64` and `arm64`, then evaluate portability gaps before expanding platform support.

**Exit criteria**

- [75%:dedf9a6c] A tenant memory fault terminates the offending module instance without crashing the host process on supported targets.
- [85%:dedf9a6c] Unsupported targets clearly fall back to an explicitly documented mode.

### 3. Deterministic CPU metering ("fuel")

Add deterministic execution budgeting to the compiler path.

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
- [60%:dedf9a6c] Establish invariants for what host functions may do while a module is suspended.

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

- [45%:dedf9a6c] Add targeted tests for memory fault recovery, fuel exhaustion, async resumption, and policy enforcement.
- [10%:dedf9a6c] Expand fuzzing and negative testing around host interfaces and trap paths.
- [70%:dedf9a6c] Document platform limitations, performance tradeoffs, and security assumptions.
- [25%:dedf9a6c] Add observability hooks for trap causes, fuel usage, yield counts, and policy denials.
- [95%:dedf9a6c] Keep the secure mode opt-in until behavior and compatibility are well understood.

### 7. Rust-port AOT packaging and native distribution

Track the Rust port as a first-class deliverable, not as an afterthought to the
Go runtime. The goal is to keep the core runtime small, keep WASI out of core
crates, preserve interpreter availability for no-JIT environments, and support
native packaging for embedders that want a fully linked executable.

#### Current implemented state

- [100%:dedf9a6c] The Rust runtime already supports explicit **interpreter** and **compiler**
  modes.
- [100%:dedf9a6c] `razero` now supports **precompiled artifacts** through
  `build_precompiled_artifact`, `compile_precompiled_artifact`, and
  `instantiate_precompiled_artifact`, so `.wasm` can be AOT-prepared and later
  loaded without recompiling at runtime.
- [95%:dedf9a6c] `razero-compiler` now preserves a richer **AOT metadata** sidecar describing:
  target, function metadata, relocations, function signatures, module shape,
  import descriptors, memory/table/global metadata, module-context layout, and
  execution-context/helper ABI details.
- [100%:dedf9a6c] `CompiledModule::emit_relocatable_object()` now emits a first
  **Linux/x86_64 ELF relocatable object** plus Razero metadata sidecar.
- [90%:dedf9a6c] `razero_compiler::runtime_support::LinkedModule` provides a minimal
  metadata-driven linked startup/call surface for simple Linux/x86_64 AOT
  modules.
- [75%:dedf9a6c] `razero_compiler::linker::link_native_executable(...)` now packages one or
  more relocatable Wasm objects into a native executable for the current narrow
  surface: exported functions with scalar C ABI-compatible signatures.
- [100%:dedf9a6c] `razero_compiler::linker::link_hello_host_executable(...)` now provides a
  **specialized** native-link flow for the existing `hello-host` example,
  including its `env.print(ptr, len)` host import and local memory setup.
- [100%:dedf9a6c] The current packaging flow emits a package metadata bundle alongside the
  executable (`.razero-package`) and is covered by end-to-end Rust tests.

#### Product shape we should preserve

- [100%:dedf9a6c] **Interpreter runtime mode remains required.** Packaged native executables do
  not replace the interpreter; they are a separate AOT deployment target.
- [95%:dedf9a6c] **WASI stays out of the core crates.** Host APIs remain embedder-defined and
  must be linked or supplied explicitly.
- [90%:dedf9a6c] **Initial packaged host ABI remains C ABI first.**
- [100%:dedf9a6c] **Linux ELF remains the first shipping object/executable target**, with
  x86_64 the current implemented architecture.

#### What is still incomplete

- [30%:dedf9a6c] The **generic** native-link path is still intentionally narrow:
  - [15%:dedf9a6c] Linux/x86_64 only
  - [20%:dedf9a6c] exported functions only
  - [35%:dedf9a6c] scalar C ABI-compatible signatures only
  - [10%:dedf9a6c] no generic packaging yet for imports, local/global runtime state, start
    sections, tables, element segments, or richer module shapes
- [40%:dedf9a6c] `hello-host` is now packaged, but through a **special-case linker path**, not
  through a generalized host-ABI/runtime-support layer.
- [15%:dedf9a6c] There is not yet a stable, general-purpose packaging story for:
  - [10%:dedf9a6c] arbitrary host imports
  - [20%:dedf9a6c] multiple linked Wasm modules with cross-module runtime state
  - [10%:dedf9a6c] modules with memory/table/global requirements beyond the specialized paths
  - [20%:dedf9a6c] AArch64 object emission and native packaging
- [35%:dedf9a6c] The package metadata format exists in code, but still needs to be treated as a
  versioned product surface with explicit compatibility guarantees.

#### Work still required to finish this properly

1. **Generalize host-import packaging**
   - [20%:dedf9a6c] Replace the `hello-host` special case with a reusable host-ABI/runtime
     support layer.
   - [30%:dedf9a6c] Make imported functions resolve through explicit packaged host descriptors
     instead of generated ad hoc example logic.
   - [45%:dedf9a6c] Keep host ownership explicit: the packager should wire host APIs supplied by
     the embedder, not smuggle system functionality back into core crates.

2. **Generalize runtime-state packaging**
   - [15%:dedf9a6c] Support packaged modules that need memory, globals, tables, start functions,
     and data/element initialization without relying on hand-written per-example
     startup code.
   - [25%:dedf9a6c] Promote the current metadata-driven startup assumptions into a real runtime
     support contract.

3. **Stabilize the packaging ABI**
   - [40%:dedf9a6c] Treat execution-context layout, module-context layout, helper IDs, symbol
     names, sidecar schema, and `.razero-package` contents as versioned ABI.
   - [10%:dedf9a6c] Document what is private vs link-visible so future compiler/runtime changes
     do not silently break packaged artifacts.

4. **Widen target coverage**
   - [5%:dedf9a6c] Add AArch64 relocatable object emission and packaging.
   - [85%:dedf9a6c] Decide later whether Mach-O/COFF are in scope, but do not block Linux/ELF
     hardening on that decision.

5. **Decide crate/product boundaries**
   - [85%:dedf9a6c] Keep `razero` focused on embedding/runtime APIs.
   - [90%:dedf9a6c] Keep `razero-compiler` focused on codegen, metadata, object emission, and
     linker support unless and until a separate `razero-aot` / `razero-pack`
     crate becomes warranted.
   - [80%:dedf9a6c] Preserve `razero-ffi` as a possible stable static-lib integration surface,
     but do not force packaged execution to depend on a bloated monolithic FFI
     layer.

6. **Expand validation coverage**
   - [60%:dedf9a6c] Keep interpreter, compiler, precompiled-artifact, and native-packaged flows
     all green at once.
   - [35%:dedf9a6c] Add more end-to-end fixtures covering:
     - [90%:dedf9a6c] one Wasm module + one host static library
     - [15%:dedf9a6c] multiple Wasm modules
     - [20%:dedf9a6c] packaged explicit host imports beyond `hello-host`
     - [35%:dedf9a6c] negative tests for ABI mismatches, malformed metadata, and unsupported
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

- [45%:dedf9a6c] **SSA `ExitIfTrue` instruction for fuel checks**: Consider adding a first-class `ExitIfTrue(cond, exitCode)` SSA opcode to replace the current branch-to-exit-block pattern used by fuel metering at function entries and loop back-edges. A dedicated opcode would allow the SSA optimizer to reason about fuel check elimination at compile time (e.g., coalescing consecutive checks, hoisting checks out of inner loops with bounded iteration counts). This is not urgent since the current `insertFuelCheck` pattern (load/sub/store/cmp/branch-to-exit) is ~5 native instructions and already efficient, but the optimization would reduce code size and improve instruction cache utilization in fuel-heavy workloads.

## Main risks and tradeoffs

- [75%:dedf9a6c] Go runtime fault handling is platform-sensitive and may limit portability.
- [70%:dedf9a6c] Large virtual memory reservations and guard-page strategies may behave differently across kernels and operating systems.
- [30%:dedf9a6c] Fuel injection and async stack handling will increase compiler complexity, compile time, and runtime overhead.
- [65%:dedf9a6c] Strict uncoupling from system APIs means embedders must meticulously provide their own WASI or host functionality.
- [80%:dedf9a6c] Some features may remain compiler-only, leaving the interpreter with a different security profile.

## Near-term deliverables

- [90%:dedf9a6c] A written threat model and support matrix for secure mode.
- [90%:dedf9a6c] A Linux-first memory sandboxing prototype.
- [80%:dedf9a6c] A compiler prototype with fuel metering and resource exhaustion traps.
- [45%:dedf9a6c] A minimalist, pure WebAssembly core engine completely decoupled from system, OS, and WASI dependencies.
- [85%:dedf9a6c] A Linux-first Rust AOT/native-packaging path with documented ABI limits, plus
  a concrete plan to generalize host-import and runtime-state packaging.
- [80%:dedf9a6c] An experimental status report describing what is safe, what is incomplete, and what remains research.
