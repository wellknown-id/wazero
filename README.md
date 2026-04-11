# razero

`razero` is a Rust WebAssembly runtime evolved from an earlier Go codebase into
a Rust-first workspace. The focus here is embedding untrusted
Wasm workloads with stronger isolation experiments, deterministic resource
controls, and explicit host-owned system integration.

## Status

This repository is **experimental**. The workspace version is currently
`0.0.0`, the public API is still moving, and the repo documents below are the
best source of truth for what is implemented today.

## Workspace layout

The main crates are:

- `razero` - public embedding/runtime API
- `razero-compiler` - compiler and AOT/native packaging support
- `razero-interp` - interpreter engine
- `razero-wasm`, `razero-decoder`, `razero-platform`, `razero-secmem`,
  `razero-features`, `razero-ffi` - supporting runtime/compiler crates

## Quick start

Run the bundled host-import example:

```bash
cargo run -p razero --example hello_world
```

Expected output:

```text
hello world from guest
```

## Runtime configuration

`razero` currently exposes three runtime-engine entry points:

- `RuntimeConfig::new()` - interpreter-first default configuration
- `RuntimeConfig::new_auto()` - compiler when supported, otherwise interpreter
- `RuntimeConfig::new_compiler()` / `RuntimeConfig::new_interpreter()` - force a
  specific engine

Selected hardening controls are configured directly on `RuntimeConfig`, for
example:

- `with_secure_mode(true)` for guard-page-backed secure-mode execution where
  supported
- `with_fuel(...)` for deterministic execution budgeting
- `with_close_on_context_done(true)` for context-driven termination

For exact platform/runtime support details, see
[SUPPORT_MATRIX.md](SUPPORT_MATRIX.md).

## Repository guide

- [examples/README.md](examples/README.md) - example and fixture overview
- [THREAT_MODEL.md](THREAT_MODEL.md) - current security assumptions and
  boundaries
- [SUPPORT_MATRIX.md](SUPPORT_MATRIX.md) - runtime/feature/platform support
- [SE-ROADMAP.md](SE-ROADMAP.md) - staged implementation roadmap
- [razero-compiler/AOT_PACKAGING_ABI.md](razero-compiler/AOT_PACKAGING_ABI.md) -
  current AOT packaging ABI
- [CONTRIBUTING.md](CONTRIBUTING.md) - contributor workflow
