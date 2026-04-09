# Workstream 1 support matrix

This document centralizes the current support status for Workstream 1 runtime features:

- runtime mode selection
- `WithSecureMode`
- `WithFuel`
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
| `NewRuntimeConfig()` | Uses the compiler when `platform.CompilerSupported()` is true, otherwise uses the interpreter. | Safe automatic fallback to interpreter. |
| `NewRuntimeConfigCompiler()` | Forces the compiler engine. | Panics if the current `GOOS` / `GOARCH` cannot run the compiler. |
| `NewRuntimeConfigInterpreter()` | Forces the interpreter engine. | Always available, but compiler-only features stay unavailable. |

## Platform buckets used below

- **Linux/amd64 compiler**: current best-supported secure-mode compiler path.
- **Linux/arm64 compiler**: secure-mode compiler path is implemented and capability-gated, but native fault-path sign-off is still pending.
- **Other compiler-supported targets**: platforms where the compiler can run, but the Workstream 1 Linux signal-handler fault path is not enabled.
- **Interpreter / non-compiler targets**: any explicit interpreter configuration, plus any target where `NewRuntimeConfig()` auto-falls back to the interpreter.

## Workstream 1 feature matrix

| Feature | Linux/amd64 compiler | Linux/arm64 compiler | Other compiler-supported targets | Interpreter / non-compiler targets | Fallback when unavailable | Validation status |
| --- | --- | --- | --- | --- | --- | --- |
| Baseline Wasm execution | ✅ | ✅ | ✅ | ✅ | n/a | Covered by normal repository test suites on supported targets. |
| `WithSecureMode(true)`: guard-page-backed linear memory allocation | ✅ on unix/windows | ✅ on unix/windows | ✅ on unix/windows | ✅ on unix/windows | On non-`unix`/`windows` targets, secure mode stays on regular checked memory paths. If the embedder already provides a custom `experimental.MemoryAllocator`, wazero does not replace it. | Backed by `internal/platform` and `internal/secmem` tests; this is broader than the Linux fault-trap path below. |
| `WithSecureMode(true)`: hardware fault to Wasm OOB trap path and compiler bounds-check elision | ✅ | ✅ in code | ❌ | ❌ | Outside Linux `amd64` / `arm64`, compiled code keeps normal software bounds checks even when secure mode is enabled. Interpreter always keeps software bounds checks. | Linux/amd64 has end-to-end coverage via `TestSecureMode_HardwareFaultToTrap`. Linux/arm64 implementation is present, but [native validation is still pending](ARM64-SECURE-MODE-VALIDATION.md). |
| `WithFuel(fuel > 0)` deterministic metering | ✅ | ✅ | ✅ | ✅ | `fuel <= 0` disables metering. | Covered by repository fuel/controller tests on both engines and by fuel benches on the compiler path. |

## Hook ordering and precedence

These rules cover the currently shipped observer/policy/config surfaces:
`TrapObserver`, `YieldObserver`, `HostCallPolicy`, `YieldPolicy`,
`TimeProvider`, `ImportResolverConfig`, and fuel control via `WithFuel` /
`WithFuelController`.

### Imported host calls and traps

- Host call context is prepared in this order:
  1. use the call context as-is;
  2. if it has no `TimeProvider`, inherit the runtime/module default;
  3. evaluate `HostCallPolicy`;
  4. if `WithYielder` was enabled, expose a `Yielder` to the host function.
- `HostCallPolicy` therefore runs before the host function executes and before
  any yield attempt. If it denies the call, `YieldPolicy` is never consulted.
- `FunctionListener.Before` for an imported host function runs only after
  `HostCallPolicy` allows that call. A denied host import therefore reports the
  policy decision and aborts the caller listener. On the compiler path today,
  stack unwinding can also emit an orphan `FunctionListener.Abort` for the
  denied callee even though no matching `Before` ran.
- `YieldPolicy` is checked only when the host function actually calls
  `Yielder.Yield()`. A denial terminates execution with
  `ErrRuntimePolicyDenied`.
- `TrapObserver` fires only for recognized runtime traps (for example
  `policy_denied`, `fuel_exhausted`, memory faults). It does not report plain
  host panics, and `Resumer.Cancel()` is not a trap.
- `FunctionListener.Abort` runs during stack unwinding before trap and terminal
  fuel notifications are emitted.

```go
callCtx := experimental.WithTrapObserver(
    experimental.WithYieldPolicy(
        experimental.WithHostCallPolicy(experimental.WithYielder(ctx), hostPolicy),
        yieldPolicy,
    ),
    trapObserver,
)
```

With this setup, `hostPolicy` runs first. `yieldPolicy` only matters if the
allowed host function later calls `Yield()`.

### Yield / resume lifecycle

- `YieldObserver` event order is:
  - `yielded` when execution suspends;
  - `resumed` on a successful `Resume` attempt before re-entry;
  - `cancelled` on `Cancel`.
- Validation errors before resume starts (for example nil resume context or
  wrong host result count) do not emit `resumed` / `cancelled` and do not spend
  the resumer.
- Resume uses the resume context for subsequent host-call state. That means a
  resumed execution can swap in a new `TrapObserver`, `YieldObserver`,
  `HostCallPolicy`, `YieldPolicy`, or call-scoped `TimeProvider`.
- If the resume context omits `YieldObserver`, the observer from the suspended
  execution remains in effect. If the resume context omits a call-scoped
  `TimeProvider`, the runtime/module default still applies; the original
  per-call override does not leak across resumes unless reapplied.

### Import resolution

- `ImportResolverConfig.ACL` is evaluated before any resolver or store lookup.
- Inside the ACL, deny rules beat allow rules. If any allow rule exists,
  unmatched imports are denied.
- If the ACL permits the import, `Resolver` runs next.
- Store fallback happens only when `Resolver` is nil or returns nil and
  `FailClosed` is false.
- `WithImportResolverObserver` reports ACL allow/deny decisions, resolver hits,
  store fallback, and fail-closed denial during instantiation.
- `WithImportResolverACL` preserves an existing resolver while adding ACL state.
  `WithImportResolverConfig` replaces the whole import-resolution config for
  that derived context.

```go
ctx = experimental.WithImportResolverConfig(ctx, experimental.ImportResolverConfig{
    ACL:        experimental.NewImportACL().AllowModules("env"),
    Resolver:   resolveImport,
    FailClosed: true,
})
```

This means: check the ACL first, ask `resolveImport` second, and never fall
back to the instantiated-module store.

### Fuel compatibility

- The shipped fuel hooks are: `RuntimeConfig.WithFuel`,
  `experimental.WithFuelController`, `experimental.WithFuelObserver`,
  `experimental.RemainingFuel`, `experimental.AddFuel`, and
  `FuelController.Consumed`.
- On both engines, a call-scoped `FuelController` overrides the runtime fuel
  budget for that call. A non-positive controller budget disables metering for
  that call.
- When a `FuelObserver` is present, lifecycle events are
  emitted in call order: `budgeted`, then any in-host `recharged` events from
  `AddFuel`, then terminal `consumed` or `exhausted`.
- `FunctionListener.Before` for the guest call runs after the initial
  `budgeted` notification, and `FunctionListener.Abort` / `After` happens
  before the terminal `consumed` or `exhausted` fuel event.
- For yielded executions, the fuel controller/budget is chosen when the Wasm
  call starts and is carried through later `Resume` calls for that suspended
  execution.
- If a resume context provides a new `FuelObserver`, it takes precedence for
  subsequent events. If it omits one, the suspended execution keeps using the
  original observer.
- Fuel consumption is reported through `Consumed` on normal completion and on
  trap/error paths.

## Practical guidance

- Install hardening hooks at the narrowest scope that matches their job:

  | Scope | Put these hooks here | Why |
  | --- | --- | --- |
  | runtime defaults | `RuntimeConfig.WithHostCallPolicy`, `RuntimeConfig.WithYieldPolicy`, `RuntimeConfig.WithTimeProvider`, `RuntimeConfig.WithFuel` | Sets a baseline for every module instantiated by that runtime. Call-scoped overrides can still narrow or replace the default later. |
  | instantiation context | `experimental.WithImportResolverACL`, `experimental.WithImportResolverConfig` | Import resolution happens only while instantiating a module, so attach ACL/resolver policy to `Instantiate`, `InstantiateWithConfig`, or `InstantiateModule`. |
  | call / resume context | `experimental.WithYielder`, `experimental.WithTrapObserver`, `experimental.WithYieldObserver`, `experimental.WithFuelController`, `experimental.WithFuelObserver`, `experimental.WithHostCallPolicy`, `experimental.WithYieldPolicy`, `experimental.WithTimeProvider` | These govern a specific execution attempt and can change on `Resume`. Rebuild the resume context with the observers/policies you still want active. |

- A practical pattern is:
  1. set runtime-wide host/yield defaults once;
  2. instantiate each guest with an import ACL / resolver config;
  3. wrap each exported-function call with only the per-request observers,
     fuel controls, and call-scoped overrides you need.
- The `experimental` package example `Example_runtimeHardeningHooks` shows this
  layering end-to-end: import ACL at instantiation, runtime-default
  `HostCallPolicy`, per-call `YieldObserver`, and a denial path surfaced through
  `TrapObserver`.
- If you want the broadest compatibility, use `NewRuntimeConfig()` and assume interpreter fallback is possible.
- Deterministic fuel metering works on both engines. Require the compiler
  explicitly only if you also depend on compiler-only behavior such as JIT
  performance or compiler-path benchmarking.
- If you require the Workstream 1 hardware-fault secure-mode path, target Linux first:
  - **Linux/amd64** is the current validated path.
  - **Linux/arm64** is implemented but should still be treated as pending native validation sign-off.
- On other targets, `WithSecureMode(true)` still opts into the secure-mode configuration surface, but the runtime falls back to checked execution instead of the Linux hardware-fault path.
