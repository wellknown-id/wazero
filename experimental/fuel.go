package experimental

import (
	"context"
	"sync/atomic"

	"github.com/tetratelabs/wazero/internal/expctxkeys"
)

// FuelController manages fuel budgets for Wasm execution within a context scope.
//
// Fuel is a deterministic CPU metering mechanism: compiled code decrements a
// counter at function entries and loop back-edges, and when the counter drops
// below zero, execution terminates with ErrRuntimeFuelExhausted. This provides
// wall-clock-independent execution budgeting for untrusted modules.
//
// FuelControllers can be nested via Go contexts to support caller-pays
// accounting in multi-tenant runtimes. A host function that bridges to
// another tenant can create a child context with a sub-controller
// (see AggregatingFuelController). When the child scope completes, its
// consumption is reported to the parent via the Consumed callback.
//
// See WithFuelController and GetFuelController for context integration.
type FuelController interface {
	// Budget returns the fuel budget to set on the execution context for
	// the next Wasm function call. This is called by the call engine before
	// each Call/CallWithStack invocation.
	//
	// Returning 0 means unlimited (no fuel metering for this call).
	// Returning a negative value is treated as 0.
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
//
// If the context already has a FuelController, the new one takes precedence
// for Wasm calls made with this context. The caller is responsible for
// propagating consumption to the parent (see NewAggregatingFuelController).
func WithFuelController(ctx context.Context, fc FuelController) context.Context {
	if fc != nil {
		return context.WithValue(ctx, expctxkeys.FuelControllerKey{}, fc)
	}
	return ctx
}

// GetFuelController returns the FuelController from the context, or nil
// if none is set.
func GetFuelController(ctx context.Context) FuelController {
	fc, _ := ctx.Value(expctxkeys.FuelControllerKey{}).(FuelController)
	return fc
}

// SimpleFuelController provides a fixed budget per call with
// optional consumption tracking. It is safe for concurrent use.
//
// Example:
//
//	ctrl := experimental.NewSimpleFuelController(1_000_000)
//	ctx := experimental.WithFuelController(ctx, ctrl)
//	result, err := mod.ExportedFunction("process").Call(ctx, input)
//	fmt.Println("fuel consumed:", ctrl.TotalConsumed())
type SimpleFuelController struct {
	budget   int64
	consumed atomic.Int64
}

// NewSimpleFuelController creates a FuelController with a fixed per-call
// budget. Each Wasm function call receives this budget. The total consumed
// fuel is tracked across all calls.
func NewSimpleFuelController(budget int64) *SimpleFuelController {
	return &SimpleFuelController{budget: budget}
}

// Budget implements FuelController.
func (s *SimpleFuelController) Budget() int64 {
	return s.budget
}

// Consumed implements FuelController.
func (s *SimpleFuelController) Consumed(amount int64) {
	s.consumed.Add(amount)
}

// TotalConsumed returns the total fuel consumed across all calls managed
// by this controller.
func (s *SimpleFuelController) TotalConsumed() int64 {
	return s.consumed.Load()
}

// AggregatingFuelController wraps a parent FuelController, enforcing a
// sub-budget for the current scope while reporting consumption to the parent.
// This is the key building block for caller-pays accounting in multi-tenant
// runtimes.
//
// When a Wasm function completes, consumed fuel is reported to both this
// controller (for local tracking) and the parent (for aggregate billing).
//
// Example (cross-tenant caller-pays):
//
//	// Alice's scope: budget of 1M fuel units.
//	aliceCtrl := experimental.NewSimpleFuelController(1_000_000)
//	aliceCtx := experimental.WithFuelController(ctx, aliceCtrl)
//
//	// Inside a host bridge function called by Alice's module:
//	func bridgeToBob(ctx context.Context, mod api.Module, stack []uint64) {
//	    parentCtrl := experimental.GetFuelController(ctx)
//	    bobCtrl := experimental.NewAggregatingFuelController(parentCtrl, 100_000)
//	    bobCtx := experimental.WithFuelController(ctx, bobCtrl)
//	    bobModule.ExportedFunction("compute").Call(bobCtx, ...)
//	    // bobCtrl.TotalConsumed() == fuel used by Bob
//	    // aliceCtrl.TotalConsumed() also includes Bob's consumption
//	}
type AggregatingFuelController struct {
	parent    FuelController
	subBudget int64
	consumed  atomic.Int64
}

// NewAggregatingFuelController creates a FuelController that enforces
// subBudget for its scope while reporting consumption to parent.
func NewAggregatingFuelController(parent FuelController, subBudget int64) *AggregatingFuelController {
	return &AggregatingFuelController{
		parent:    parent,
		subBudget: subBudget,
	}
}

// Budget implements FuelController.
func (a *AggregatingFuelController) Budget() int64 {
	return a.subBudget
}

// Consumed implements FuelController. It reports consumption to both
// the local tracker and the parent controller.
func (a *AggregatingFuelController) Consumed(amount int64) {
	a.consumed.Add(amount)
	if a.parent != nil {
		a.parent.Consumed(amount)
	}
}

// TotalConsumed returns the total fuel consumed across all calls managed
// by this controller (not including fuel consumed by the parent outside
// this scope).
func (a *AggregatingFuelController) TotalConsumed() int64 {
	return a.consumed.Load()
}
