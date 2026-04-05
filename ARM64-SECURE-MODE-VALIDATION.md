# arm64 secure mode validation plan

This document defines how to validate Linux/arm64 secure-mode memory fault handling when most development happens on non-arm64 hosts.

## Why QEMU is not enough alone

QEMU (especially TCG) is useful for rapid iteration, but this feature depends on Linux signal ABI behavior that can differ from emulation:

- `ucontext_t` register layout and signal frame details.
- `siginfo_t` fault metadata (`si_addr`, codes).
- Return-to-user control flow after signal handler rewrites RIP/PC/SP/FP.
- Kernel and libc edge behavior for synchronous `SIGSEGV` faults in JIT memory.

Therefore, QEMU can be a strong pre-check, but final confidence requires real Linux/arm64 execution.

## Validation tiers

### Tier 1: fast local loop (cross-compile + QEMU)

Use this for frequent dev feedback and basic regression checks.

- Build/test with `GOARCH=arm64`.
- Run core compiler and secure-mode tests under QEMU user/system mode.
- Verify that expected trap translation tests pass and no host process crash occurs in normal cases.

Suggested scope:

- `go test ./internal/engine/wazevo/...`
- `go test -run TestSecureMode_HardwareFaultToTrap .`

Treat Tier 1 as necessary but not sufficient.

### Tier 2: merge gate on real Linux/arm64

Use this for branch protection and release confidence.

- Run on native arm64 hardware (example: Graviton, Ampere, or native arm64 CI runners).
- Run the same tests as Tier 1 plus full repo tests.
- Confirm non-JIT faults still follow expected crash/forward behavior.

Required gate:

- `go test ./...` on real Linux/arm64 must pass.
- Secure-mode fault path must show Wasm-visible OOB trap behavior without process-level crash for supported cases.

## Test matrix

| Area | QEMU | Real arm64 |
|---|---|---|
| Build correctness (`GOARCH=arm64`) | Required | Required |
| Wazevo package tests | Required | Required |
| Secure-mode OOB trap integration test | Required | Required |
| Full repository tests (`go test ./...`) | Optional (recommended) | Required |
| Signal ABI correctness confidence | Partial | Required |
| Release/merge confidence for arm64 trap handling | No | Yes |

## Recommended CI layout

- `arm64-emulated` job:
  - Runs on x86 host with QEMU.
  - Executes Tier 1 set quickly.
  - Catches basic regressions early.

- `arm64-native` job:
  - Runs on real arm64 hardware.
  - Executes Tier 2 set (`go test ./...`).
  - Acts as required status check for secure-mode arm64 changes.

## Minimum policy for arm64 secure-mode work

Before merging changes that touch arm64 trap/fault handling:

1. QEMU validation passes.
2. Native Linux/arm64 validation passes.
3. At least one end-to-end secure-mode memory OOB trap test passes on native arm64.

If native arm64 capacity is temporarily unavailable, arm64 secure-mode changes should remain behind explicit capability checks and be treated as experimental.

## Parity checklist vs amd64

Track Linux/arm64 against Linux/amd64 in these concrete areas:

- Signal installation parity:
  - Uses `rt_sigaction` with `SA_SIGINFO`-compatible handler shape.
  - Preserves and forwards to Go's original SIGSEGV handler for non-JIT faults.
- Fault classification parity:
  - JIT range table check uses faulting PC and registered executable ranges.
  - Non-JIT faults never get converted into Wasm traps.
- Trap conversion parity:
  - Sets `ExecutionContext.ExitCode` to memory OOB (`4`).
  - Restores original Go FP/SP from execution context.
  - Restores Go return address register (`LR`/x30 on arm64).
  - Redirects PC to fault return trampoline and returns from handler.
- Entry path parity:
  - Execution context pointer is held in a reserved register across JIT execution.
  - Reserved register is removed from allocator candidates.
- Capability gating parity:
  - `secureMode` memory isolation uses `signalHandlerSupported()` gate.
  - Unsupported targets fail closed to safe fallback mode.

Exit condition for parity:

- Linux/arm64 passes the same secure-mode memory fault integration semantics as Linux/amd64 on native hardware, including non-JIT fault forwarding behavior.
