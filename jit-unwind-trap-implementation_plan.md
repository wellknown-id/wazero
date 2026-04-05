# Hardware Memory Isolation: Revised Architecture

## Root Cause Analysis

After exhaustive study of Go 1.26's runtime internals (`signal_unix.go`, `traceback.go`, `panic.go`), the crash in `TestSecureMode_HardwareFaultToTrap` is caused by a **fundamental incompatibility between Go's panic recovery mechanism and JIT-compiled code**.

### The Failure Chain

When an OOB memory access hits a `PROT_NONE` guard page:

1. **SIGSEGV** fires → Go's signal handler (`sighandler`) sees `gp.paniconfault == true`
2. Signal handler injects **`sigpanic`** by modifying the signal context (PC → sigpanic, faulting PC pushed as return address)
3. `sigpanic` → `panicmemAddr` → `gopanic`
4. `gopanic` → `_panic.start()` → `_panic.nextDefer()` → `_panic.nextFrame()`
5. **`nextFrame` uses Go's stack unwinder** to walk frames and match defers
6. The unwinder calls `findfunc(faulting_PC)` — **this returns invalid** because the faulting PC is in mmap'd JIT code, not registered in Go's function tables
7. The unwinder stops (prints "unexpected return pc" warning, sets `frame.lr = 0`)
8. **`nextDefer` never finds the `callWithStack` defer** (can't walk past JIT frames to the Go frame containing `recover()`)
9. `gopanic` falls through to `fatalpanic` → process crash

### Why This Is Unfixable In Pure Go

Go's runtime requires **every stack frame** to be resolvable via `findfunc()` (i.e., registered in `runtime.moduledata`). JIT code is allocated via `mmap` and is invisible to Go's function tables.

There is **no public API** to register JIT code with Go's runtime. The alternatives considered:

| Approach | Problem |
|---|---|
| Run JIT on Go stack (current) | `findfunc(JIT_PC)` fails → unwinder stops → `recover()` unreachable |
| Run JIT on separate stack | Same `findfunc` problem, plus SP not on goroutine stack |
| `signal.Notify` for SIGSEGV | Doesn't work for synchronous signals; no signal context access |
| Skip JIT frame setup (frameless) | Issue is the PC, not the frame structure — `findfunc` still fails |
| CGO setjmp/longjmp | Breaks wazero's pure-Go guarantee |

> [!IMPORTANT]
> **Hardware fault-to-trap conversion is not possible in pure Go with JIT-compiled code.** This is a fundamental limitation of Go's runtime, not a bug in our implementation.

## Revised Architecture: Defense-in-Depth

Instead of converting hardware faults to Wasm traps, we adopt a **defense-in-depth** strategy:

1. **Software bounds checks remain active** — OOB accesses are caught by the existing wazevo software checks and translated to proper Wasm traps (this works today)
2. **Guard pages serve as a safety net** — If a bug in the software bounds checking allows an OOB access through, the guard page SIGSEGV crashes the process (fail-safe, not exploitable)
3. **mmap-backed linear memory** — The secmem allocator still provides isolated, mmap-backed memory regions (better than plain Go slice allocation)

### Security Properties

| Property | Without secureMode | With secureMode (revised) |
|---|---|---|
| Linear memory allocation | Go slices | mmap with guard pages |
| OOB detection | Software bounds check | Software bounds check + guard page crash |
| OOB result | Wasm trap (recoverable) | Wasm trap (recoverable) |
| Bounds check bypass bug | Potential memory corruption | Process crash (fail-safe) |

## Proposed Changes

### Engine Pipeline

#### [MODIFY] [engine.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/engine.go)
- Set `memoryIsolationEnabled: false` in `compiledModule` regardless of `secureMode`
- Remove `DisableStackCheck()` calls
- Remove `secureMode` parameter from `compileLocalWasmFunction`

#### [MODIFY] [call_engine.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/call_engine.go)
- Remove `debug.SetPanicOnFault(true)` from `callWithStack`
- Remove the fault-detection `runtime.Error` recovery in the defer block
- Remove debug print statements

#### [MODIFY] [abi_entry_preamble.go (amd64)](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/backend/isa/amd64/abi_entry_preamble.go)
- Remove `useGoStack` parameter — always use the separate stack
- Revert `CompileEntryPreamble` signature to `(sig *ssa.Signature) []byte`

#### [MODIFY] [abi_entry_preamble.go (arm64)](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/backend/isa/arm64/abi_entry_preamble.go)
- Same revert as amd64

#### [MODIFY] [abi_entry_amd64.s](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/backend/isa/amd64/abi_entry_amd64.s)
- Revert from `CALL R11` back to `JMP R11`

### Interface Cleanup

#### [MODIFY] [machine.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/backend/machine.go)
- Revert `CompileEntryPreamble` signature (remove `useGoStack`)

#### [MODIFY] [compiler.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/backend/compiler.go)
- Remove `DisableStackCheck()` from Compiler interface and implementation

### Tests

#### [MODIFY] [secure_test.go](file:///mnt/faststorage/repos/se-wazero/secure_test.go)
- Rename to `TestSecureMode_OOBTrappedWithGuardPages`
- Verify that OOB access produces proper Wasm trap (same as non-secureMode)
- Add a test that verifies guard pages exist (mmap-level test)

#### [MODIFY] Mock compilers (amd64/arm64 util_test.go)
- Remove `DisableStackCheck()` stubs

### Documentation

#### [MODIFY] [THREAT_MODEL.md](file:///mnt/faststorage/repos/se-wazero/THREAT_MODEL.md)
- Document that guard pages are defense-in-depth, not the primary OOB detection mechanism
- Note the Go runtime limitation that prevents hardware fault-to-trap conversion

## Open Questions

> [!WARNING]
> **Performance impact**: With software bounds checks remaining active, secureMode provides the same OOB-detection performance as non-secureMode. The benefit is purely the guard-page safety net. Is this acceptable, or should we explore CGO-based signal handling in the future?

## Verification Plan

### Automated Tests
- `go test -v -run TestSecureMode ./...` — Verify OOB is caught by software bounds check
- `go test ./internal/platform/ -run TestMmapLinearMemory` — Verify guard pages are correctly allocated
- `go test ./internal/engine/wazevo/... ` — Verify no regressions in existing tests

### Manual Verification
- The guard page crash path can be verified by temporarily disabling the software bounds check and confirming SIGSEGV kills the process (not exploitable)
