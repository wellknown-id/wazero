# se-wazero Phase 2: Deterministic CPU Metering ("Fuel")

## Background

Phase 1 established hardware-assisted memory sandboxing and the secure-mode infrastructure. Phase 2 adds **deterministic execution budgeting** so that untrusted Wasm modules can be terminated based on instruction count, not wall-clock time.

### Cost Model

The initial cost model assigns **1 fuel unit per function entry** and **1 fuel unit per loop back-edge**. This is the simplest model that guarantees termination for any Wasm program: every infinite loop must contain a loop header (cost 1), and every recursive call chain must enter a function (cost 1).

More granular models (per-instruction, weighted by opcode) can be layered on top without changing the injection mechanism, since the fuel check infrastructure is opcode-agnostic — it just decrements and checks.

> [!NOTE]
> **Why 1:1 cost?** A uniform cost model avoids opcode-dependent weighting tables that are hard to audit and may change across compiler versions. For an embedder who wants to guarantee "no more than N loop iterations", uniform cost is directly interpretable. Weighted models can be introduced as a `FuelCostModel` strategy interface in a future phase.

### Two-Level Fuel Architecture

The existing `ensureTermination` pattern uses a trampoline per loop header — it exits compiled code entirely, checks `ModuleInstance.Closed()` in Go, then re-enters. This is too expensive for fuel because fuel checks happen at every function entry and loop back-edge.

Instead, we use a **two-level scheme**:

1. **Fast path (inline native code)**: A `fuel int64` field lives in the `executionContext` struct at a known offset. Compiled code emits ~5 native instructions (load/sub/store/cmp/branch) at each checkpoint — no Go-side trampoline overhead.
2. **Slow path (exit code)**: When the counter drops below zero, the compiled code triggers `ExitCodeFuelExhausted` (already wired from Phase 1). The call engine's Go-side loop handles it by surfacing `ErrRuntimeFuelExhausted`.

```
                 ┌──────────────────────────────────────────┐
                 │           RuntimeConfig                  │
                 │  .WithFuel(1_000_000)                    │
                 └──────────────┬───────────────────────────┘
                                │ sets initial fuel on
                                ▼
                 ┌──────────────────────────────────────────┐
                 │         executionContext                  │
                 │  fuel int64 (at known offset)             │
                 └──────────────┬───────────────────────────┘
                                │ read/updated by
                                ▼
┌──────────────────────────────────────────────────────────┐
│  Compiled Wasm (function entry / loop back-edge)         │
│                                                          │
│  load  fuel, [execCtx + FuelOffset]   // fast path       │
│  sub   fuel, 1                                           │
│  store fuel, [execCtx + FuelOffset]                      │
│  cmp   fuel, 0                                           │
│  b.lt  exit_fuel_exhausted            // → slow path     │
│  ... continue execution ...                              │
└──────────────────────────────────────────────────────────┘
```

---

## Contextual FuelController Design

### Motivation: Caller-Pays in Multi-Tenant Runtimes

In a multi-tenant runtime where modules communicate via host-bridged function calls (local-local, local-remote, RPC mesh), fuel must support **cross-tenant accounting**:

- **Tenant A (Alice)** instantiates a module and calls a function.
- The module calls a host function that bridges to **Tenant B (Bob)**.
- Alice is "subletting" compute to Bob. Alice's budget should decrease by Bob's consumption.
- The host runtime needs visibility into per-scope consumption for billing and quotas.

This requires **nested, context-scoped fuel controllers** where outer scopes aggregate inner scope consumption.

### Interface Design

```go
package experimental

// FuelController manages fuel budgets for Wasm execution within a context scope.
// It is set on the context via WithFuelController and retrieved via
// GetFuelController.
//
// FuelControllers can be nested: a host function that bridges to another
// tenant can create a child context with a sub-controller. When the child
// scope completes, its consumption is reported to the parent via the
// parent's Consumed method.
//
// This design supports caller-pays metrics: an outer controller sees the
// total fuel consumed by all nested calls, enabling billing and quota
// enforcement across trust boundaries.
type FuelController interface {
    // Budget returns the fuel budget to set on the executionContext for
    // the next Wasm function call. This is called by the call engine
    // before each Call/CallWithStack invocation.
    //
    // Returning 0 means unlimited (no fuel metering for this call).
    // Returning a negative value is invalid and will be treated as 0.
    Budget() int64

    // Consumed is called by the call engine after each Call/CallWithStack
    // completes (whether normally, by trap, or by fuel exhaustion).
    // The amount is the fuel consumed during that call (budget - remaining).
    //
    // Implementations may aggregate this into a parent controller,
    // update billing records, emit metrics, etc.
    Consumed(amount int64)
}

// WithFuelController returns a derived context with the given FuelController.
// If the context already has a FuelController, the new one takes precedence
// for Wasm calls made with this context, but the caller is responsible for
// propagating consumption to the parent (see AggregatingFuelController).
func WithFuelController(ctx context.Context, fc FuelController) context.Context

// GetFuelController returns the FuelController from the context, or nil
// if none is set.
func GetFuelController(ctx context.Context) FuelController
```

### Built-in Implementations

```go
// SimpleFuelController provides a fixed budget per call with
// optional consumption tracking.
type SimpleFuelController struct {
    budget    int64
    consumed  atomic.Int64 // total consumed across all calls
}

func NewSimpleFuelController(budget int64) *SimpleFuelController
func (s *SimpleFuelController) Budget() int64
func (s *SimpleFuelController) Consumed(amount int64)
func (s *SimpleFuelController) TotalConsumed() int64

// AggregatingFuelController wraps a parent FuelController, enforcing a
// sub-budget for the current scope while reporting consumption to the parent.
// This is the key building block for caller-pays accounting.
//
// Example: Alice has budget=1M. A host bridge creates a child scope for Bob
// with sub-budget=100K. Bob's execution consumes 50K. When the child scope
// completes, 50K is reported to Alice's controller.
type AggregatingFuelController struct {
    parent    FuelController
    subBudget int64
    consumed  atomic.Int64
}

func NewAggregatingFuelController(parent FuelController, subBudget int64) *AggregatingFuelController
func (a *AggregatingFuelController) Budget() int64
func (a *AggregatingFuelController) Consumed(amount int64) // reports to parent
func (a *AggregatingFuelController) TotalConsumed() int64
```

### Worked Example: Cross-Tenant Caller-Pays

```go
// Alice's scope: budget of 1M fuel units.
aliceCtrl := experimental.NewSimpleFuelController(1_000_000)
aliceCtx := experimental.WithFuelController(ctx, aliceCtrl)

// Alice calls her module.
result, err := aliceModule.ExportedFunction("process").Call(aliceCtx, input)
// aliceCtrl.TotalConsumed() == fuel used by Alice's module

// Inside the host bridge function (called by Alice's module):
func bridgeToBob(ctx context.Context, mod api.Module, stack []uint64) {
    // Create a sub-scope for Bob with a 100K sub-budget, paid by Alice.
    parentCtrl := experimental.GetFuelController(ctx)
    bobCtrl := experimental.NewAggregatingFuelController(parentCtrl, 100_000)
    bobCtx := experimental.WithFuelController(ctx, bobCtrl)

    // Bob's execution consumes from his sub-budget.
    bobResult, err := bobModule.ExportedFunction("compute").Call(bobCtx, ...)
    // bobCtrl.TotalConsumed() == fuel used by Bob
    // parentCtrl (Alice) also sees Bob's consumption via Consumed callback
}
```

---

## Proposed Changes

### Component 1 — Fuel Counter in executionContext

#### [MODIFY] [call_engine.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/call_engine.go)

Add `fuel int64` after the last field in `executionContext`. This field is read and written by compiled native code at a known offset via the execution context pointer.

> [!NOTE]
> **Why int64?** Signed arithmetic simplifies the exhaustion check in generated code: `sub; cmp 0; b.lt` is a single conditional branch on both amd64 and arm64. With uint64, underflow wrapping would require a separate overflow check. The trade-off is max budget of ~9.2×10¹⁸ which is sufficient for any practical workload.

#### [MODIFY] [offsetdata.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/wazevoapi/offsetdata.go)

Add the offset constant. Based on the current struct layout, the field after `memoryNotifyTrampolineAddress` (offset 1176, size 8) is at offset **1184**:

```go
ExecutionContextOffsetFuel Offset = 1184
```

---

### Component 2 — FuelController in experimental package

#### [NEW] [experimental/fuel.go](file:///mnt/faststorage/repos/se-wazero/experimental/fuel.go)

The `FuelController` interface, `WithFuelController`, `GetFuelController`, `SimpleFuelController`, and `AggregatingFuelController` implementations as described above.

#### [NEW] [internal/expctxkeys/fuel.go](file:///mnt/faststorage/repos/se-wazero/internal/expctxkeys/fuel.go)

Context key for the FuelController.

---

### Component 3 — SSA-Level Fuel Injection

#### [MODIFY] [frontend.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/frontend/frontend.go)

Add `fuelEnabled bool` to `Compiler` struct and `NewFrontendCompiler` signature.

#### [MODIFY] [lower.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/frontend/lower.go)

Add `c.insertFuelCheck()` helper and call it at:
1. **Function entry** — in `LowerToSSA`, after parameter setup
2. **Loop back-edge** — in `OpcodeLoop` case, after `c.switchTo(originalLen, loopHeader)`

The helper uses the **branch-to-exit-block** pattern (Option A):
- Load fuel from `[execCtx + FuelOffset]`
- Subtract 1
- Store back
- If result < 0: branch to a new basic block containing `ExitWithCode(FuelExhausted)`
- Otherwise: fall through to the next block

---

### Component 4 — Configuration and Runtime Wiring

#### [MODIFY] [config.go](file:///mnt/faststorage/repos/se-wazero/config.go)

Add `WithFuel(int64) RuntimeConfig` to the interface and `fuel int64` to `runtimeConfig`.

#### [MODIFY] [runtime.go](file:///mnt/faststorage/repos/se-wazero/runtime.go)

Pass fuel config to runtime struct. In `InstantiateModule`, the fuel configuration flows through to the compiled module.

#### [MODIFY] [engine.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/engine.go)

Thread `fuelEnabled` through `compileModule` → `NewFrontendCompiler`.

#### [MODIFY] [call_engine.go](file:///mnt/faststorage/repos/se-wazero/internal/engine/wazevo/call_engine.go)

In `callWithStack`, before entrypoint invocation:
1. Check for `FuelController` in context → use its `Budget()`
2. Else check `compiledModule.fuel` → use that
3. Set `c.execCtx.fuel`
4. After execution, compute consumed = initial - remaining, call `FuelController.Consumed(consumed)`

---

### Component 5 — Roadmap Update

#### [MODIFY] [SE-ROADMAP.md](file:///mnt/faststorage/repos/se-wazero/SE-ROADMAP.md)

Add to a future phase item:
- **SSA ExitIfTrue instruction**: Consider adding a first-class `ExitIfTrue(cond, exitCode)` SSA opcode to replace the current branch-to-exit-block pattern for fuel checks. This would allow the SSA optimizer to better reason about fuel check elimination at compile time (e.g., when consecutive checks can be coalesced).

---

### Component 6 — Tests and Benchmarks

#### [NEW] [experimental/fuel_test.go](file:///mnt/faststorage/repos/se-wazero/experimental/fuel_test.go)

Unit tests for `SimpleFuelController` and `AggregatingFuelController`.

#### [NEW] or [MODIFY] [internal/secbench/fuel_bench_test.go](file:///mnt/faststorage/repos/se-wazero/internal/secbench/fuel_bench_test.go)

- `BenchmarkFuelOverhead`: fac-ssa(20) with fuel=MAX vs. without → measures per-checkpoint cost
- `BenchmarkFuelExhaustion`: time to trigger + recover from exhaustion

#### Integration tests

1. Infinite loop + `WithFuel(1000)` → terminates with `ErrRuntimeFuelExhausted`
2. Adequate fuel → completes normally
3. `WithFuel(0)` → unlimited (backward compatible, no overhead)
4. `AggregatingFuelController` → parent sees child consumption

---

## Verification Plan

### Automated Tests

```bash
# FuelController unit tests
go test ./experimental/ -run TestFuel -v

# SSA fuel injection correctness
go test ./internal/engine/wazevo/frontend/ -run TestFuel -v

# Integration: fuel exhaustion and cross-tenant accounting
go test ./internal/integration_test/... -run TestFuel -v

# Benchmarks: fuel overhead measurement
go test ./internal/secbench/ -bench=BenchmarkFuel -benchmem -count=3

# Full regression suite
go test ./... -timeout 300s
```

### Manual Verification

- Confirm deterministic termination: a module with an infinite loop and `WithFuel(N)` terminates at the same point regardless of CPU speed or scheduling.
- Confirm `WithFuel(0)` produces identical behavior to upstream wazero with zero performance delta.
