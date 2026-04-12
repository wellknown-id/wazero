# razero roadmap

`razero` is a Rust-based WebAssembly runtime focused on running untrusted workloads in multi-tenant environments with stronger isolation and more deterministic resource controls. The project started as a port of an earlier Go runtime and now documents a Rust-first workspace and API surface.

This document turns the rough specification into an initial roadmap. It is intentionally ambitious and should be treated as a staged research and implementation plan, not a promise of short-term delivery.

Note: this roadmap encompasses work commencing from commit 7f2e5f44e791c45714ab233d3cff48bad9818168.

## Goals

- Easy embedding, no mandatory C dependencies, and cross-compilation friendliness.
- Improve safety for untrusted and multi-tenant workloads.
- Prefer fail-closed behavior when the runtime cannot safely support a feature on a target platform.
- Introduce security features incrementally so each stage can be validated independently.

## Non-goals for the first phase

- Full feature parity across every operating system and architecture.
- Replacing all existing software safety checks immediately.
- Delivering production-ready multi-tenant isolation in a single release.

## Design constraints

- Keep Linux `amd64` and `arm64` as the primary initial target for security-sensitive features.
- Preserve a clear fallback story when hardware trapping or other platform behavior is unavailable or inconsistent.
- Keep security policy decisions explicit and host-controlled.

## Workstreams

### Status overview

| Workstream | Progress | Summary |
| --- | --- | --- |
| 1. Foundation and threat model | [42%:763f642b] | Runtime/error scaffolding plus secure-mode, fuel, and yield surfaces exist; the `secbench` manual benchmark workflow now identifies the core baseline groups for roadmap tracking; and `SUPPORT_MATRIX.md` now spells out the current experimental fuel cost model as a control-flow-budget contract rather than a per-instruction promise. Rust-specific threat/support docs and broader baseline practice still remain. |
| 2. Hardware-assisted memory sandboxing | [99%:472db265] | Guarded allocation, Linux-first reserved-memory, compiler codegen memory isolation, SIGSEGV handling, typed bounds validation, compiler lowering validation for all scalar and subword load/store paths, entrypoint glue and signal-handler assembly validation on both Linux backends. Wider platform hardening and deeper native arm64 validation still lag. |
| 3. Deterministic CPU metering ("fuel") | [84%:d89465d1] | Fuel controllers, host fuel APIs, exhaustion handling, and compiler function-entry plus loop-header metering are wired through the production compiler path; interpreter debits native function entry and backward branches with nested-call, yield/resume, and host-interleaved validation. Config-level fuel clamping/round-trip, module-engine fuel initialization (local vs imported), fuel observer lifecycle (Consumed/Exhausted/absent), and full E2E runtime fuel exhaustion (interpreter + compiler/secure-mode) are now verified. Basic-block injection and broader cost/overhead validation still remain incomplete. |
| 4. Async yield and resume | [78%:4888107a] | Yield/resume protocol, resumers, cancellation, cross-thread resume, `YieldPolicy` wiring, context overrides, direct host-export denial, suspended-module reentry rejection, stale/spent resumers, and host-result arity validation across re-yield. Deeper compiler-native suspend/restore validation still remains. |
| 5. Zero-trust host interface | [98%:881c6ab6] | Core crates keep WASI/system behavior outside the runtime. Import ACLs, fail-closed resolution, observer/audit events, `HostCallPolicy`/`YieldPolicy` surfaces with full caller-module metadata, signature-based enforcement, import-resolver observer lifecycle events, `FuelObserver` lifecycle notifications, `TimeProvider` surface, and `YieldObserver` wiring are in place, and the README/support/contributing docs now make the embedder-owned host-interface model explicit. Broader observer composition and remaining lifecycle coverage still remain, but resumed-segment observer/provider handoff semantics are now much better pinned by public tests and summarized in one explicit support-matrix contract, including direct-call ordering plus resume-time `YieldPolicyObserver` handoff, host-call denial short-circuiting later yield observers, and the current non-reinjected `Snapshotter` behavior on resumed host calls. |
| 6. Validation, hardening, and operational readiness | [99%:5db9ff2c] | Parity/spec/smoke tests, listener surfaces, benches, packaging ABI docs, policy/fault-path coverage, compiler-backed secure-mode OOB validation, trap observer API, interpreter and compiler trap-observer coverage, full public surface parity, import-resolver config/ACL round-trips, public runtime tests for import-resolver observer ordering plus yield-observer resume semantics, and broad AMD64 codegen coverage. Broader hardening depth and fallback breadth still remain; the fuzz workspace now includes policy-denial negative-path coverage for cooperative yield and host-call denial, relevant observer event-stream parity, deterministic replay coverage, a fixed-fixture trap target spanning multiple trap families, interpreter-mode fuel observer parity coverage, and fixed import-resolver observer ordering coverage, and targeted runtime tests now pin the current fuel-observer yielded/resume behavior, current fuel-controller resume behavior, fuel-observer ordering around listener `before` / `after` plus the current host-overdraw `Consumed`-then-`Exhausted` sequence, trap-observer resume behavior, `Resumer::cancel()` remaining outside trap-observer callbacks, plain host panics staying outside trap-observer callbacks, resume-path `TimeProvider` handoff, current non-reinjected `Snapshotter` behavior on resumed host calls, host-call listener ordering on denied imports, trap-observer ordering ahead of listener abort on denied imports, the current absence of separate imported-host listener callbacks on allowed guest-import calls, and both direct-call and resume-path host-call policy observer behavior plus direct-call ordering and resume-path handoff for `YieldPolicyObserver`, including allowed-yield ordering against `YieldObserver`, so support docs do not overclaim resume-time callbacks or policy-hook ordering. |
| 7. AOT packaging and native distribution | [99%:931c751e] | Strong Linux ELF/AOT packaging path with extensive negative coverage for malformed metadata, truncation, and corruption across all sidecar fields. The linked runtime support surface now also has explicit public fail-closed coverage for constructor, `call_export`, and `start` errors around missing exports, wrong arity, non-function exports, malformed local-function/type metadata, and invalid first-function pointers. Broader runtime-state packaging hardening still remains. |

### 1. Foundation and threat model

Establish the baseline needed to evaluate all later work.

- [95%:dedf9a6c] Document the threat model for untrusted tenant code, host functions, and shared infrastructure.
- [83%:2a1cfbcf] Define supported and unsupported security properties for each runtime mode and platform, with cached and uncached secure-mode compile paths now sharing the same memory-isolation capability gate on unsupported targets, the runtime now routing secure-mode guard-page allocation through one explicit helper, and secure-mode runtime configuration explicitly tested to propagate into internal secure-memory store state.
- [85%:dedf9a6c] Identify the razero subsystems that will change first: memory management, compiler backends, and trap handling.
- [97%:156dfeb5] Define new error and trap categories for memory faults, fuel exhaustion, policy denials, and async yield/resume transitions.
- [88%:dedf9a6c] Add benchmark and regression baselines for compile time, execution time, memory growth, and trap overhead, using the documented `cargo bench -p razero --bench secbench` manual workflow and its core baseline groups.

### 2. Hardware-assisted memory sandboxing

Shift linear memory protection toward OS-backed virtual memory primitives.

- [100%:dedf9a6c] Introduce an abstraction for reserving large virtual address ranges for Wasm linear memory.
- [100%:dedf9a6c] Prototype `mmap`-backed memory reservation with committed active pages and inaccessible guard pages.
- [90%:156dfeb5] Translate page faults caused by out-of-bounds access into recoverable Wasm traps using signal handling where supported.
- [95%:dedf9a6c] Keep software bounds checks or other safe fallbacks where hardware trapping is not yet reliable.
- [75%:dedf9a6c] Start with Linux `amd64` and `arm64`, then evaluate portability gaps before expanding platform support.

**Exit criteria**

- [80%:156dfeb5] A tenant memory fault terminates the offending module instance without crashing the host process on supported targets.
- [85%:dedf9a6c] Unsupported targets clearly fall back to an explicitly documented mode.

### 3. Deterministic CPU metering ("fuel")

Add deterministic execution budgeting to compiled and interpreted execution paths.

- [60%:dedf9a6c] Define a cost model for Wasm instructions that is simple enough to reason about and stable enough to expose to embedders, with `SUPPORT_MATRIX.md` now documenting the current experimental contract as control-flow checkpoint accounting (function entry / loop or backward-edge progress) rather than a per-instruction pricing promise.
- [90%:dedf9a6c] Inject fuel accounting at function entries, loop headers, and other control-flow boundaries chosen for acceptable overhead.
- [100%:d89465d1] Add a trap path for resource exhaustion that does not depend on wall-clock deadlines. Interpreter and compiler/secure-mode paths are now both validated E2E and surface `fuel exhausted` / `TrapCause::FuelExhausted` on the strict runtime path.
- [85%:d89465d1] Expose safe host APIs to inspect, add, and subtract fuel for a running instance. Config validation, controller round-trips, observer lifecycle notifications (Budgeted/Consumed/Recharged/Exhausted), and absent-observer safety now covered.
- [55%:d89465d1] Validate that compiler overhead, execution overhead, and accounting accuracy are acceptable for the experiment. Module-engine fuel initialization correctness validated (local gets fuel, imported does not, disabled skips stale values).

**Exit criteria**

- [100%:d89465d1] Long-running or infinite Wasm execution can be stopped deterministically through fuel exhaustion. Full E2E is now verified for both interpreter and compiler/secure-mode paths.
- [80%:d89465d1] Hosts can safely recharge or debit fuel during execution without breaking isolation guarantees. Add/remaining/overdraw/recharge observer notification coverage complete.

### 4. Async yield and resume

Introduce cooperative suspension for Wasm execution that waits on host-side asynchronous work.

- [90%:dedf9a6c] Define a yield protocol between compiled Wasm code and host functions.
- [80%:dedf9a6c] Extend the compiler pipeline with a stack-unwinding and state-capture strategy for resumable execution.
- [90%:dedf9a6c] Reserve storage for saved locals, value stack state, and resume instruction pointers.
- [85%:dedf9a6c] Implement resume semantics that restore execution exactly at the suspended point.
- [75%:4888107a] Establish invariants for what host functions may do while a module is suspended, including rejected reentry, stale/spent resumers, and host-result arity validation across re-yield.

**Exit criteria**

- [85%:dedf9a6c] A module can yield on supported async host calls and later resume without restarting execution.
- [80%:dedf9a6c] Suspended modules do not require a permanently blocked thread per waiting tenant.

### 5. Zero-trust host interface

Harden the runtime by remaining completely unopinionated about system functionality.

- [72%:dedf9a6c] Remove all WASI implementation code, OS dependencies, and system-level structures from the core engine, with the repo front door now explicitly documenting that there is no built-in runtime-owned WASI layer in core crates.
- [77%:dedf9a6c] Establish an architecture where embedders are strictly responsible for providing any required host interfaces, including filesystems, network egress policies, and clock resolution controls; README, support, and threat docs now all say this directly, and public runtime tests now pin resume-path `TimeProvider` handoff to the embedder-supplied call context.
- [48%:dedf9a6c] Explicitly fail-closed by removing platform-specific adapter layers and system wrappers that bypass host application policies, with import ACL / resolver policy now documented in the README as the expected instantiation-time default-deny surface.
- [58%:dedf9a6c] Keep contributor guidance aligned with the zero-trust design so new core changes do not silently reintroduce runtime-owned system surfaces or convenience wrappers that bypass embedder policy.

**Exit criteria**

- [15%:dedf9a6c] The core engine contains absolutely zero system or OS coupling.
- [88%:dedf9a6c] WASI layers (P1/P2) must be supplied strictly externally by the embedder, and the main README now calls that requirement out directly instead of leaving it buried in supporting docs.

### 6. Validation, hardening, and operational readiness

Turn prototypes into an experimental runtime that can be evaluated seriously.

- [99%:16b42271] Add targeted tests for memory fault recovery, fuel exhaustion, async resumption, and policy enforcement, with public runtime coverage now also checking import-resolver observer ordering for ACL allow/deny, resolver success, store fallback, fail-closed denial, the current yield-observer resume / cancellation semantics, the current yielded-call fuel-observer behavior around resume, current fuel-controller resume behavior, fuel-observer ordering around listener `before` / `after` plus the current host-overdraw `Consumed`-then-`Exhausted` sequence, trap-observer resume behavior, `Resumer::cancel()` staying outside trap-observer callbacks, plain host panics staying outside trap-observer callbacks, resume-path `TimeProvider` handoff, current resumed-call `Snapshotter` behavior, denied-host-import listener ordering, trap-observer ordering ahead of listener abort on denied imports, the current absence of separate imported-host listener callbacks on allowed guest-import calls, host-call denial short-circuiting `YieldPolicyObserver` / `YieldObserver`, and host-call plus yield-policy observer metadata / ordering / handoff for both direct-call and resumed-segment policy decisions, including `YieldPolicyObserver` ordering ahead of `YieldObserver::Yielded` on allowed cooperative yields.
- [87%:dedf9a6c] Expand fuzzing and negative testing around host interfaces and trap paths, with the `internal/integration_test/fuzz` workspace now covering cooperative-yield and host-call policy denial plus trap-observer, yield-observer, host-policy observer parity, deterministic replay for `policy_no_diff`, fixed-fixture trap parity via `trap_no_diff`, interpreter-mode fuel observer parity via `fuel_no_diff`, and fixed import-resolver observer ordering via `import_resolver_no_diff`.
- [99%:1baa1d65] Document platform limitations, performance tradeoffs, and security assumptions in `SUPPORT_MATRIX.md` and `THREAT_MODEL.md`, including secure-mode fallback behavior, observer overhead, the current yielded-call fuel-observer semantics around resume, the current fuel-controller resume semantics, the current trap-observer resume semantics, resume-path `TimeProvider` handoff semantics, current resumed-call `Snapshotter` limitations, the current absence of separate imported-host listener callbacks on allowed guest-import calls plus host-call policy observer handoff on resumed-segment denied calls, host-call denial short-circuiting `YieldPolicyObserver` / `YieldObserver`, `YieldPolicyObserver` ordering ahead of `YieldObserver::Yielded` on allowed cooperative yields, resume-path `YieldPolicyObserver` handoff semantics, and one consolidated resumed-segment hook handoff matrix.
- [74%:16b42271] Add observability hooks for trap causes, fuel usage, yield counts, and policy denials, with `SUPPORT_MATRIX.md` now tightened to match the current shipped yield-observer semantics on resume and cancel paths.
- [95%:dedf9a6c] Keep the secure mode opt-in until behavior and compatibility are well understood.

### 7. AOT packaging and native distribution

Keep the core runtime small, keep WASI out of core crates, preserve interpreter
availability for no-JIT environments, and support native packaging for embedders
that want a fully linked executable.

#### Current implemented state

- [100%:04af62a8] The runtime supports explicit **interpreter** and **compiler** modes.
- [100%:04af62a8] `razero` supports **precompiled artifacts** through
  `build_precompiled_artifact`, `compile_precompiled_artifact`, and
  `instantiate_precompiled_artifact`, so `.wasm` can be AOT-prepared and later
  loaded without recompiling at runtime.
- [100%:04af62a8] `razero-compiler` preserves a richer **AOT metadata** sidecar describing:
  target, function metadata, relocations, function signatures, module shape,
  import descriptors, memory/table/global metadata, module-context layout, and
  execution-context/helper ABI details.
- [100%:04af62a8] `CompiledModule::emit_relocatable_object()` emits
  **Linux ELF relocatable objects** plus Razero metadata sidecar.
- [100%:04af62a8] `razero_compiler::runtime_support::LinkedModule` provides a
  metadata-driven linked startup/call surface for Linux ELF AOT modules with the
  supported runtime-state slice, and its public fail-closed constructor /
  `call_export` / `start` error paths, eager start-signature/type validation,
  plus repeated-call runtime-state behavior are now pinned by targeted tests,
  including `_start` fallback rejection for non-function exports, missing local
  function metadata, and missing cached entry preambles.
- [100%:04af62a8] Linked runtime-plan bounds and metadata-shape rejection for
  packaged local memory / table initialization are now pinned by targeted tests
  too, so obvious segment-overflow, segment-kind/type, element-initializer,
  offset/type-expression, const-expression parsing, shared-memory,
  local-function, count-shape, host-module, import-shape, and
  termination-helper regressions fail closed under direct planner coverage.
- [95%:04af62a8] `razero_compiler::linker::link_native_executable(...)` packages one or
  more relocatable Wasm objects into a native executable for the current C ABI-first
  surface.
- [100%:ea77dc43] The current public C ABI export filter is now pinned too:
  native packaging rejects exported functions with non-scalar params,
  non-scalar results, or more than one result rather than silently widening
  the surface.
- [100%:ea77dc43] The current export selector is pinned too: mixed modules keep
  supported scalar C ABI exports while dropping unsupported signatures instead
  of rejecting the whole module once at least one supported export remains, and
  stale local function type metadata currently shrinks that export surface the
  same way; wrapper generation and preamble emission follow the same filtered
  export set.
- [100%:ea77dc43] The current named-preamble shape is pinned too: the shipped
  `trim_void_preamble_prologue` knob is presently a no-op on current targets
  rather than producing a distinct void-preamble body.
- [100%:ea77dc43] The current native-link entrypoint now has targeted pre-link
  rejection coverage for corrupt sidecar metadata (including stale
  export/start/import-type indexes), unsupported target architectures, and
  linked-runtime metadata that falls outside the packaged slice, plus
  wrapper-generation rejection for missing local start-function metadata.
- [100%:ea77dc43] `razero_compiler::linker::link_hello_host_executable(...)` provides a
  **specialized** native-link flow for the existing `hello-host` example,
  including its single explicit `(i32, i32) -> ()` host import and local memory
  setup, and now has direct public-entrypoint rejection coverage for invalid
  sidecar metadata (including stale host-import type indexes), malformed
  host-import signatures, missing `run` exports, `run` exports that resolve to
  imports or missing type metadata, plus wrong import-count / missing-memory
  shapes that currently fail earlier through the shared packaged-host
  validator.
- [100%:ea77dc43] The current `hello-host` / packaged-host-import metadata
  validators now have targeted fail-closed coverage for missing host-import type
  metadata, generic packaged-host descriptor/target mismatches, plus `run`
  exports that either resolve to imports or point at missing type metadata.
- [100%:04af62a8] The current packaging flow emits a package metadata bundle alongside the
  executable (`.razero-package`) and is covered by end-to-end tests.

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
5. AOT metadata, precompiled artifacts, and Linux/ELF packaging hardening
6. Async yield and resume
7. Broader validation and platform expansion

This order prioritizes containment and deterministic limits before more invasive coroutine-style execution work.

### Future optimizations

- [45%:04af62a8] **SSA `ExitIfTrue` instruction for fuel checks**: Consider adding a first-class `ExitIfTrue(cond, exitCode)` SSA opcode to replace the current branch-to-exit-block pattern used by fuel metering at function entries and loop back-edges. A dedicated opcode would allow the SSA optimizer to reason about fuel check elimination at compile time (e.g., coalescing consecutive checks, hoisting checks out of inner loops with bounded iteration counts). This is not urgent since the current `insertFuelCheck` pattern (load/sub/store/cmp/branch-to-exit) is ~5 native instructions and already efficient, but the optimization would reduce code size and improve instruction cache utilization in fuel-heavy workloads.

### Near-term planning follow-on

After the negative-path fuzzing slice above, the next planning target should be
Workstream 1 benchmark and regression baselines. The repository already has a
substantial `razero/benches/secbench.rs` surface covering compile time,
execution, trap overhead, memory allocation/growth, and fuel-related overhead,
so the remaining work is best framed as turning that existing bench surface into
explicit baseline and regression-tracking coverage rather than inventing a new
benchmark harness.

## Main risks and tradeoffs

- [70%:04af62a8] Large virtual memory reservations and guard-page strategies may behave differently across kernels and operating systems.
- [30%:04af62a8] Fuel injection and async stack handling will increase compiler complexity, compile time, and runtime overhead.
- [65%:04af62a8] Strict uncoupling from system APIs means embedders must meticulously provide their own WASI or host functionality.
- [80%:04af62a8] Some features may still land compiler-first, leaving the interpreter with a narrower hardening and security profile until parity catches up.

## Near-term deliverables

- [90%:04af62a8] A written threat model and support matrix for secure mode.
- [90%:04af62a8] A Linux-first memory sandboxing prototype.
- [80%:04af62a8] A compiler prototype with fuel metering and resource exhaustion traps.
- [45%:04af62a8] A minimalist, pure WebAssembly core engine completely decoupled from system, OS, and WASI dependencies.
- [100%:04af62a8] A Linux-first AOT/native-packaging path with documented ABI limits, plus
  a concrete plan to generalize host-import and runtime-state packaging.
- [90%:04af62a8] An experimental status report describing what is safe, what is incomplete, and what remains research.
