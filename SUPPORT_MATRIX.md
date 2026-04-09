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
| `WithFuel(fuel > 0)` deterministic metering | ✅ | ✅ | ✅ | ❌ | Interpreter ignores `WithFuel`; if `NewRuntimeConfig()` auto-selects the interpreter, fuel behaves as a no-op. `fuel <= 0` also disables metering. | Implemented in the compiler path and covered by repository fuel/controller tests and benches. |

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
- `YieldPolicy` is checked only when the host function actually calls
  `Yielder.Yield()`. A denial terminates execution with
  `ErrRuntimePolicyDenied`.
- `TrapObserver` fires only for recognized runtime traps (for example
  `policy_denied`, `fuel_exhausted`, memory faults). It does not report plain
  host panics, and `Resumer.Cancel()` is not a trap.

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
- On the compiler path, a call-scoped `FuelController` overrides the runtime
  fuel budget for that call. The interpreter ignores `WithFuel`.
- When a `FuelObserver` is present on the compiler path, lifecycle events are
  emitted in call order: `budgeted`, then any in-host `recharged` events from
  `AddFuel`, then terminal `consumed` or `exhausted`.
- For yielded executions, the fuel controller/budget is chosen when the Wasm
  call starts and is carried through later `Resume` calls for that suspended
  execution.
- If a resume context provides a new `FuelObserver`, it takes precedence for
  subsequent events. If it omits one, the suspended execution keeps using the
  original observer.
- Fuel consumption is reported through `Consumed` on normal completion and on
  trap/error paths.

## Practical guidance

- If you want the broadest compatibility, use `NewRuntimeConfig()` and assume interpreter fallback is possible.
- If you require deterministic fuel metering, require the compiler explicitly and treat interpreter fallback as unsupported for that deployment.
- If you require the Workstream 1 hardware-fault secure-mode path, target Linux first:
  - **Linux/amd64** is the current validated path.
  - **Linux/arm64** is implemented but should still be treated as pending native validation sign-off.
- On other targets, `WithSecureMode(true)` still opts into the secure-mode configuration surface, but the runtime falls back to checked execution instead of the Linux hardware-fault path.
