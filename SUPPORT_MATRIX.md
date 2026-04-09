# Workstream 1 support matrix

This document centralizes the current support status for Workstream 1 runtime features:

- runtime mode selection
- `WithSecureMode`
- `WithFuel`
- current fallback behavior
- validation status where it materially affects the support story

It complements [THREAT_MODEL.md](THREAT_MODEL.md) and
[ARM64-SECURE-MODE-VALIDATION.md](ARM64-SECURE-MODE-VALIDATION.md).

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

## Practical guidance

- If you want the broadest compatibility, use `NewRuntimeConfig()` and assume interpreter fallback is possible.
- If you require deterministic fuel metering, require the compiler explicitly and treat interpreter fallback as unsupported for that deployment.
- If you require the Workstream 1 hardware-fault secure-mode path, target Linux first:
  - **Linux/amd64** is the current validated path.
  - **Linux/arm64** is implemented but should still be treated as pending native validation sign-off.
- On other targets, `WithSecureMode(true)` still opts into the secure-mode configuration surface, but the runtime falls back to checked execution instead of the Linux hardware-fault path.
