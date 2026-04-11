# Workstream 1 support matrix

This document centralizes the current support status for Workstream 1 runtime
features:

- runtime mode selection
- `RuntimeConfig::with_secure_mode`
- `RuntimeConfig::with_fuel`
- current fallback behavior
- validation status where it materially affects the support story

It complements [THREAT_MODEL.md](THREAT_MODEL.md) and
[ARM64-SECURE-MODE-VALIDATION.md](ARM64-SECURE-MODE-VALIDATION.md).

This matrix covers core runtime behavior only. Filesystem, network, clock, and
other system-facing host interfaces are expected to be supplied externally by
the embedder rather than by a built-in runtime-owned WASI layer.

## Runtime mode selection

| Runtime selection | Current behavior | Fallback / unsupported behavior |
| --- | --- | --- |
| `Runtime::new()` / `RuntimeConfig::new()` | Uses the interpreter by default. | n/a |
| `RuntimeConfig::new_auto()` | Uses the compiler when `razero_platform::compiler_supported()` is true, otherwise uses the interpreter. | Safe automatic fallback to interpreter. |
| `RuntimeConfig::new_compiler()` | Forces the compiler engine. | Panics if the current host cannot run the compiler. |
| `RuntimeConfig::new_interpreter()` | Forces the interpreter engine. | Always available, but compiler-only features stay unavailable. |

## Platform buckets used below

- **Linux/amd64 compiler**: current best-supported secure-mode compiler path.
- **Linux/arm64 compiler**: secure-mode compiler path is implemented and
  capability-gated, but native fault-path sign-off is still pending.
- **Other compiler-supported targets**: platforms where the compiler can run,
  but the Workstream 1 Linux signal-handler fault path is not enabled.
- **Interpreter / non-compiler targets**: any explicit interpreter
  configuration, plus any target where `RuntimeConfig::new_auto()` falls back to
  the interpreter.

## Workstream 1 feature matrix

| Feature | Linux/amd64 compiler | Linux/arm64 compiler | Other compiler-supported targets | Interpreter / non-compiler targets | Fallback when unavailable | Validation status |
| --- | --- | --- | --- | --- | --- | --- |
| Baseline Wasm execution | ✅ | ✅ | ✅ | ✅ | n/a | Covered by normal repository test suites on supported targets. |
| `RuntimeConfig::with_secure_mode(true)`: guard-page-backed linear memory allocation | ✅ on unix/windows | ✅ on unix/windows | ✅ on unix/windows | ✅ on unix/windows | On non-`unix`/`windows` targets, secure mode stays on regular checked memory paths. If the embedder already provides a custom `experimental::MemoryAllocator`, razero does not replace it. | Backed by `internal/platform` and `internal/secmem` tests; this is broader than the Linux fault-trap path below. |
| `RuntimeConfig::with_secure_mode(true)`: hardware fault to Wasm OOB trap path and compiler bounds-check elision | ✅ | ✅ in code | ❌ | ❌ | Outside Linux `amd64` / `arm64`, compiled code keeps normal software bounds checks even when secure mode is enabled. Interpreter always keeps software bounds checks. | Linux/amd64 has end-to-end repository coverage for the hardware-fault-to-trap path. Linux/arm64 implementation is present, but [native validation is still pending](ARM64-SECURE-MODE-VALIDATION.md). |
| `RuntimeConfig::with_fuel(fuel > 0)`: deterministic metering | ✅ | ✅ | ✅ | ✅ | `fuel <= 0` disables metering. | Covered by repository fuel/controller tests on both engines and by fuel benches on the compiler path. |

## Platform limitations and performance tradeoffs

- **Linux/amd64 compiler** is the current best-supported secure-mode path when
  you want hardware-fault-backed OOB trapping and the strongest existing native
  validation story.
- **Linux/arm64 compiler** has the secure-mode implementation in place, but it
  should still be treated as pending native sign-off until
  [ARM64-SECURE-MODE-VALIDATION.md](ARM64-SECURE-MODE-VALIDATION.md) is closed
  out.
- On **other compiler-supported targets**, `with_secure_mode(true)` still
  enables the secure-mode configuration surface and guarded allocation behavior
  where supported, but execution falls back to the normal checked path instead
  of the Linux signal-handler fault path.
- On the **interpreter**, secure mode does not remove software bounds checks. It
  should be treated as the compatibility-first path, not the highest-throughput
  path.
- **`RuntimeConfig::new_auto()`** is the safest default when your embedder needs
  portability. It may transparently land on the interpreter, so do not assume
  compiler-specific performance characteristics unless you forced the compiler.
- **Fuel metering** is available on both engines, but the current
  compiler/secure-mode path still has one known trap-mapping limitation: fuel
  exhaustion stops execution, yet the final surfaced trap can still read as
  `memory fault` instead of `fuel exhausted` until the remaining signal-handler
  integration work lands.
- **Observers and policy hooks** (`TrapObserver`, `YieldObserver`,
  `HostCallPolicy`, `YieldPolicy`, `FuelObserver`) are intended for safety and
  diagnostics, not free instrumentation. They add extra work to each relevant
  call or trap path and should be enabled deliberately.

## Hook ordering and precedence

These rules cover the currently shipped observer/policy/config surfaces:
`TrapObserver`, `YieldObserver`, `HostCallPolicy`, `YieldPolicy`,
`ImportResolverConfig`, and fuel control via `RuntimeConfig::with_fuel` /
`experimental::with_fuel_controller`.

### Imported host calls and traps

- Host call context is prepared in this order:
  1. use the call context as-is;
  2. evaluate `HostCallPolicy`;
  3. if `experimental::with_yielder` was enabled, expose a `Yielder` to the
     host function.
- `HostCallPolicy` therefore runs before the host function executes and before
  any yield attempt. If it denies the call, `YieldPolicy` is never consulted.
- `HostCallPolicyObserver`, when present, reports the allow/deny decision with
  the resolved host-function name and caller-module metadata before any
  resulting `TrapObserver` notification for denied host calls.
- `FunctionListener.Before` for an imported host function runs only after
  `HostCallPolicy` allows that call. A denied host import therefore reports the
  policy decision and aborts only the caller listener.
- `YieldPolicy` is checked only when the host function actually calls
  `Yielder::yield()`. A denial terminates execution with a policy-denied
  runtime error.
- `TrapObserver` fires only for recognized runtime traps (for example
  `policy_denied`, `fuel_exhausted`, memory faults). It does not report plain
  host panics, and `Resumer::cancel()` is not a trap.
- `FunctionListener.Abort` runs during stack unwinding before trap and terminal
  fuel notifications are emitted.

```rust
let call_ctx = experimental::with_trap_observer(
    &experimental::with_yield_policy(
        &experimental::with_host_call_policy(
            &experimental::with_yielder(&ctx),
            host_policy,
        ),
        yield_policy,
    ),
    trap_observer,
);
```

With this setup, `host_policy` runs first. `yield_policy` only matters if the
allowed host function later calls `Yield()`.

### Yield / resume lifecycle

- `YieldObserver` event order is:
  - `yielded` when execution suspends;
  - `resumed` on a successful `Resume` attempt before re-entry **when the resume
    context itself installs a `YieldObserver`**.
- `YieldEvent::Cancelled` exists in the public enum, but the current runtime
  path does **not** emit a `YieldObserver` callback on `Cancel`.
- Validation errors before resume starts (for example a missing resume context or
  wrong host result count) do not emit `resumed` and do not spend the resumer.
- Resume uses the resume context for subsequent host-call state. That means a
  resumed execution can swap in a new `TrapObserver`, `HostCallPolicy`, or
  `YieldPolicy`. A `YieldObserver` attached to the resume context receives the
  `resumed` notification for that resume attempt, and a `TrapObserver` attached
  to the resume context receives follow-on traps raised during the resumed
  segment. A `HostCallPolicyObserver` attached to the resume context receives
  allow/deny decisions for follow-on resumed-segment host calls.
- If the resume context omits `YieldObserver`, the suspended execution does not
  currently emit additional yield-observer callbacks for the resumed segment.
- If the resume context omits `TrapObserver`, the resumed segment does not
  currently report its follow-on traps to the observer from the initial call.
- If the resume context omits `HostCallPolicyObserver`, the resumed segment does
  not currently report its later host-call policy decisions to the observer
  from the initial call.
- A `TimeProvider` attached to the resume context is what later resumed-segment
  host calls observe. If the resume context omits `TimeProvider`, resumed host
  calls do not inherit the initial call's provider.
- `Snapshotter` is currently narrower: it is injected for the initial exported
  call path, but resumed host calls do not currently receive a snapshotter even
  if the resume context includes `experimental::with_snapshotter`.

### Import resolution

- `ImportResolverConfig::acl` is evaluated before any resolver or store lookup.
- Inside the ACL, deny rules beat allow rules. If any allow rule exists,
  unmatched imports are denied.
- If the ACL permits the import, `Resolver` runs next.
- Store fallback happens only when `resolver` is absent or returns `None` and
  `fail_closed` is false.
- `with_import_resolver_observer` reports ACL allow/deny decisions, resolver
  hits, store fallback, and fail-closed denial during instantiation.
- `with_import_resolver_acl` preserves an existing resolver while adding ACL
  state. `with_import_resolver_config` replaces the whole import-resolution
  config for that derived context.

```rust
let ctx = experimental::with_import_resolver_config(
    &ctx,
    experimental::ImportResolverConfig {
        acl: Some(experimental::ImportACL::new().allow_modules(["env"])),
        resolver: Some(resolve_import),
        fail_closed: true,
    },
);
```

This means: check the ACL first, ask `resolve_import` second, and never fall
back to the instantiated-module store.

### Fuel compatibility

- The shipped fuel hooks are: `RuntimeConfig::with_fuel`,
  `experimental::with_fuel_controller`, `experimental::with_fuel_observer`,
  `experimental::remaining_fuel`, `experimental::add_fuel`, and
  `FuelController::consumed`.
- On both engines, a call-scoped `FuelController` overrides the runtime fuel
  budget for that call. A non-positive controller budget disables metering for
  that call.
- When a `FuelObserver` is present, lifecycle events are emitted in call order:
  `budgeted`, then any in-host `recharged` events from `add_fuel`, then terminal
  `consumed` or `exhausted`.
- `FunctionListener.Before` for the guest call runs after the initial
  `budgeted` notification, and `FunctionListener.Abort` / `After` happens
  before the terminal `consumed` or `exhausted` fuel event.
- Fuel controller behavior on `Resume` is currently narrower than the original
  budget-controller docs implied: a resume context can replace the
  `FuelController` for later resumed-segment host calls, and omitting one on
  resume leaves those later resumed-segment host calls without a controller.
- Current yielded-call observer semantics are narrower than the yield observer
  surface: the initial yielded call emits its `budgeted` / terminal
  `consumed` lifecycle before returning the `YieldError`, and later `Resume`
  calls do not currently emit additional `FuelObserver` callbacks even if the
  resume context installs a `FuelObserver`.
- Fuel consumption is reported through `Consumed` on normal completion and on
  trap/error paths.

## Current experimental fuel cost model

The current fuel surface is intentionally **coarse-grained**. A fuel unit is not
yet documented as “one Wasm instruction” or any other per-opcode promise.
Instead, the current experimental contract is:

- razero charges fuel at a small set of **control-flow checkpoints** chosen to
  keep accounting deterministic without paying instruction-by-instruction
  overhead everywhere.
- On the **compiler path**, the current production metering points are function
  entries plus loop / control-flow boundaries used by the compiler’s injected
  fuel checks.
- On the **interpreter path**, fuel is currently debited at guest/native
  function entry and on backward-branch / loop progression paths.
- **Basic-block-level charging is not complete yet**, so embedders should treat
  today’s fuel values as runtime-version-specific accounting units rather than a
  stable cross-version “instruction count”.

### What embedders can rely on today

- Fuel is best treated as a **deterministic execution budget**, not as an exact
  cost profiler.
- The same runtime configuration and engine choice will charge fuel
  deterministically for the same execution shape.
- Long-running loops and repeated control-flow re-entry consume fuel and can be
  stopped through exhaustion.
- `experimental::with_fuel_controller` can override the per-call budget chosen
  from `RuntimeConfig::with_fuel`.
- Imported host work is **not automatically priced by wall clock or syscall
  cost**. If your embedder wants host-side resource accounting, debit or
  recharge explicitly from the host with `experimental::add_fuel`.
- Yield / resume keeps using the budget/controller selected for the suspended
  execution unless the runtime’s existing documented override points say
  otherwise.

### What is still intentionally unspecified

- No stable per-instruction pricing table is promised yet.
- No compatibility guarantee is made that a given module will consume the exact
  same numeric fuel amount across future accounting-model revisions.
- Compiler and interpreter fuel units are intended to be operationally similar,
  but they should still be treated as **engine-specific experimental accounting
  surfaces**, especially while basic-block injection and compiler trap mapping
  remain incomplete.

## Practical guidance

- Install hardening hooks at the narrowest scope that matches their job:

  | Scope | Put these hooks here | Why |
  | --- | --- | --- |
  | runtime defaults | `RuntimeConfig::with_host_call_policy`, `RuntimeConfig::with_yield_policy`, `RuntimeConfig::with_fuel` | Sets a baseline for every module instantiated by that runtime. Call-scoped overrides can still narrow or replace the default later. |
  | instantiation context | `experimental::with_import_resolver_acl`, `experimental::with_import_resolver_config` | Import resolution happens only while instantiating a module, so attach ACL/resolver policy to `compile`, `instantiate`, or `instantiate_with_context` flows. |
  | call / resume context | `experimental::with_yielder`, `experimental::with_trap_observer`, `experimental::with_yield_observer`, `experimental::with_fuel_controller`, `experimental::with_fuel_observer`, `experimental::with_host_call_policy`, `experimental::with_yield_policy`, `experimental::with_time_provider` | These govern a specific execution attempt and can change on `Resume`. Rebuild the resume context with the observers/policies you still want active. |
  | initial call context only | `experimental::with_snapshotter` | Snapshot capture currently attaches to the initial exported call path; resumed host calls do not currently get a snapshotter re-injected from the resume context. |

- A practical pattern is:
  1. set runtime-wide host/yield defaults once;
  2. instantiate each guest with an import ACL / resolver config;
  3. wrap each exported-function call with only the per-request observers,
     fuel controls, and call-scoped overrides you need.
- If you want the broadest compatibility, use `RuntimeConfig::new_auto()` and
  assume interpreter fallback is possible.
- Deterministic fuel metering works on both engines. Require the compiler
  explicitly only if you also depend on compiler-only behavior such as JIT
  performance or compiler-path benchmarking.
- If you require the Workstream 1 hardware-fault secure-mode path, target Linux
  first:
  - **Linux/amd64** is the current validated path.
  - **Linux/arm64** is implemented but should still be treated as pending native
    validation sign-off.
- On other targets, `RuntimeConfig::with_secure_mode(true)` still opts into the
  secure-mode configuration surface, but the runtime falls back to checked
  execution instead of the Linux hardware-fault path.
- If your policy depends on exact trap classification, treat compiler/secure
  fuel exhaustion as a known caveat until the remaining signal-handler mapping
  work is complete.
