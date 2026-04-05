# Implementation Plan: Hardware Memory Isolation (Guard Pages)

Finalize the "Secure Mode" implementation for the `wazevo` engine by utilizing hardware guard pages to elide software bounds checks. This improves performance and provides strong hardware-level isolation for memory access.

## User Review Required

> [!IMPORTANT]
> This change introduces a runtime dependency on `runtime.SetPanicOnFault(true)` during WebAssembly execution in secure mode. While safe within the `wazevo` execution wrapper, it affects signal handling for the duration of the call.

> [!WARNING]
> Hardware isolation relies on the platform correctly implementing `mmap` with `PROT_NONE` and the Go runtime correctly converting these faults into recoverable panics. This is supported on Linux, MacOS, and Windows, but will remain disabled (falling back to software checks) on unsupported platforms.

## Proposed Changes

### Core Engine API

#### [MODIFY] [engine.go](file:///mnt/faststorage/repos/se-wazero/internal/wasm/engine.go)
- Update `CompileModule` signature to include `secureMode bool`.

#### [MODIFY] [runtime.go](file:///mnt/faststorage/repos/se-wazero/runtime.go)
- Pass `r.secureMode` to `r.store.Engine.CompileModule`.

---

### Wazevo Compiler

#### [MODIFY] [engine.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/engine.go)
- Update `CompileModule` to accept `secureMode`.
- Store `memoryIsolationEnabled` in the `compiledModule` struct.
- Pass `cm.memoryIsolationEnabled` to `frontend.NewFrontendCompiler`.

#### [MODIFY] [frontend.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/frontend/frontend.go)
- Add `memoryIsolationEnabled bool` to the `Compiler` struct.
- Update `NewFrontendCompiler` to initialize this field.

#### [MODIFY] [lower.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/frontend/lower.go)
- In `memOpSetup`, wrap the bounds check emission logic in `if !c.memoryIsolationEnabled { ... }`.
- When elided, the compiler will generate a direct `memBase + extBaseAddr` calculation, relying on the 4 GiB guard region to catch OOB access.

---

### Wazevo Runtime

#### [MODIFY] [call_engine.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/call_engine.go)
- Import `runtime/debug`.
- In `Call` / `CallWithStack`, if `c.parent.memoryIsolationEnabled` is true:
  - Call `debug.SetPanicOnFault(true)`.
  - Use `defer debug.SetPanicOnFault(old)` to restore the previous state.
- Ensure the existing `recover()` logic (which already handles "fault" message translation) remains correctly wired.

---

## Verification Plan

### Automated Tests
- **[NEW]** `internal/integration_test/engine/secure_test.go`:
  - **Verify Software Mode**: Instantiate with `WithSecureMode(false)`, call a function that accesses index `2,147,483,647` (2GiB), and assert it returns `ErrRuntimeOutOfBoundsMemoryAccess` (via software check).
  - **Verify Secure Mode**: Instantiate with `WithSecureMode(true)`, call the same function. 
    - Assert it returns `ErrRuntimeOutOfBoundsMemoryAccess`.
    - Use a debugger or log to verify that the `lower.go` bounds check was actually elided (optional).
    - Verify the process does not crash.
  - **Verify Small OOB**: Access just past the end of a 1-page memory (e.g. offset `65537`). Assert it traps.

### Manual Verification
- Run the full suite on Linux and (if possible) MacOS to ensure platform-specific signal handling works as expected.
- Inspect generated assembly for a simple `i32.load` to confirm the `cmp/exit` instructions are gone in secure mode.
