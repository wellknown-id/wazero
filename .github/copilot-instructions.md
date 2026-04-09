# Copilot instructions

This repository is a security-focused `wazero` fork. The Go module path is still `github.com/tetratelabs/wazero`, so code, package names, and many docs still refer to `wazero` even when repository docs discuss `se-wazero` or `se-razero`.

## Build, test, and lint commands

- Full Go test suite: `make test`
- CI-style Go test run: `make test go_test_options='-timeout 20m -race -short'`
- Run one Go test in the root package: `go test . -run TestSecureMode_HardwareFaultToTrap`
- Run one Go test in a package subtree: `go test ./internal/engine/wazevo/... -run TestName`
- Cross-platform/preflight build checks: `make check`
- Go lint: `make lint`
- Go/asm formatting: `make format`
- Coverage: `make coverage`
- Rebuild spec fixtures when integration assets change: `make build.spectest`
- Example suite: `make test.examples`
- Rust workspace build: `cargo build --workspace`
- Rust workspace tests: `cargo test --workspace`
- Run one Rust test: `cargo test -p razero-secmem allocation_is_zeroed -- --exact`
- Rust formatting: `cargo fmt --all`

## High-level architecture

- The main runtime is still the Go codebase. The public API is the root `wazero` package plus `api/`; implementation details live under `internal/`.
- The Go execution pipeline is: `Runtime.CompileModule` in `runtime.go` decodes and validates Wasm (`internal/wasm/binary`, `internal/wasm`), resolves function type IDs/listeners, and hands execution to either the compiler engine in `internal/engine/wazevo` or the interpreter in `internal/engine/interpreter`.
- Secure-mode changes are threaded from `RuntimeConfig.WithSecureMode` in `config.go` through compile and instantiation paths. In compiler mode, `internal/engine/wazevo` enables hardware-backed memory isolation only when signal-handler support exists; instantiation then injects `internal/secmem.GuardPageAllocator` so linear memory uses guard-page-backed allocations. Unsupported platforms intentionally fall back to software bounds checks via `internal/platform`.
- Fuel metering is a Go-side extension: `RuntimeConfig.WithFuel` sets a default budget, and `experimental.FuelController` can override it per call via context. Only the compiler path (`internal/engine/wazevo`) consumes fuel; it also injects a context accessor so host functions can call `experimental.AddFuel` and `experimental.RemainingFuel`.
- The Rust workspace mirrors the same subsystem split instead of replacing the Go runtime yet: `razero` is the public Rust API, `razero-wasm` holds runtime-side Wasm data structures, `razero-decoder` parses binaries, `razero-compiler` contains the optimizing compiler/AOT/signal-handler work, `razero-interp` is the interpreter, `razero-platform` wraps OS and CPU features, `razero-secmem` provides guard-page-backed memory helpers, and `razero-ffi` exposes a C ABI.

## Key conventions

- Preserve the public/internal split. Shared stable types live in `api/`; implementation details stay under `internal/`. Avoid adding mutable exported data to public packages.
- Public Go types are usually interfaces. `RuntimeConfig` is immutable and every `WithXxx` returns a cloned config, while builders such as `HostModuleBuilder` are mutable and preserve declaration order.
- Follow existing Go test style: table-driven tests plus the internal assertion library in `internal/testing/require` instead of `testify`. Public API behavior is often tested from external-package tests (`package wazero_test`) rather than from inside the implementation package.
- When changing secure mode or fuel behavior, update both the public config surface and the `wazevo` execution path. The interpreter intentionally does not implement fuel metering, and capability gates such as `platform.CompilerSupports`, `signalHandlerSupported`, and guard-page support checks are deliberate fail-safe boundaries.
- Keep the low-dependency posture intact. Reuse existing internal helpers and packages before introducing new third-party dependencies.
- Rust files are expected to stay `rustfmt`-clean; `lefthook` runs `cargo fmt --all -- {staged_files}` for staged Rust files on commit.
- If asked to create a commit, use DCO signoff (`git commit -s`) because `CONTRIBUTING.md` requires it.
