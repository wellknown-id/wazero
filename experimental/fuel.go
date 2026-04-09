package experimental

import (
	"context"
	"errors"
	"reflect"
	"sync/atomic"

	"github.com/tetratelabs/wazero/api"
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

// FuelEvent identifies a fuel lifecycle transition.
type FuelEvent string

const (
	FuelEventBudgeted  FuelEvent = "budgeted"
	FuelEventConsumed  FuelEvent = "consumed"
	FuelEventRecharged FuelEvent = "recharged"
	FuelEventExhausted FuelEvent = "exhausted"
)

// FuelObservation describes an opt-in fuel lifecycle event.
//
// Budget reports the per-call budget chosen for this execution when known.
// Consumed and Remaining report the execution state at the time of observation.
// Delta is used by FuelEventRecharged and may be negative when AddFuel debits.
type FuelObservation struct {
	Module    api.Module
	Event     FuelEvent
	Budget    int64
	Consumed  int64
	Remaining int64
	Delta     int64
}

// FuelObserver receives opt-in notifications for fuel lifecycle events.
type FuelObserver interface {
	ObserveFuel(ctx context.Context, observation FuelObservation)
}

// FuelObserverFunc adapts a function into a FuelObserver.
type FuelObserverFunc func(context.Context, FuelObservation)

// ObserveFuel implements FuelObserver.
func (f FuelObserverFunc) ObserveFuel(ctx context.Context, observation FuelObservation) {
	if f != nil {
		f(ctx, observation)
	}
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

// WithFuelObserver returns a derived context with the given FuelObserver.
//
// If observer is nil, including a typed-nil FuelObserver value, ctx is returned
// unchanged.
func WithFuelObserver(ctx context.Context, observer FuelObserver) context.Context {
	if isNilFuelObserver(observer) {
		return ctx
	}
	return context.WithValue(ctx, expctxkeys.FuelObserverKey{}, observer)
}

// GetFuelController returns the FuelController from the context, or nil
// if none is set.
func GetFuelController(ctx context.Context) FuelController {
	if ctx == nil {
		return nil
	}
	fc, _ := ctx.Value(expctxkeys.FuelControllerKey{}).(FuelController)
	return fc
}

// GetFuelObserver returns the FuelObserver from ctx, or nil if none is set.
func GetFuelObserver(ctx context.Context) FuelObserver {
	if ctx == nil {
		return nil
	}
	observer, _ := ctx.Value(expctxkeys.FuelObserverKey{}).(FuelObserver)
	if isNilFuelObserver(observer) {
		return nil
	}
	return observer
}

// ErrNoFuelAccessor is returned by AddFuel and RemainingFuel when the
// context does not contain a fuel accessor. This happens when fuel metering
// is not enabled or when the function is called outside a host function
// callback (i.e., outside Wasm execution).
var ErrNoFuelAccessor = errors.New("no fuel accessor in context: fuel not enabled or not in a host function")

// AddFuel adds the given amount of fuel to the currently executing module's
// fuel budget. This is intended to be called from within a host function
// (api.GoFunction or api.GoModuleFunction) to recharge fuel mid-execution.
//
// The amount may be negative to debit fuel. If the resulting fuel counter
// drops below zero, the module will trap with ErrRuntimeFuelExhausted at
// the next fuel check (function entry or loop back-edge).
//
// Returns ErrNoFuelAccessor if the context does not have an active fuel
// accessor (fuel not enabled, or called outside a host function callback).
//
// Example (from a host function):
//
//	func myHostFn(ctx context.Context, mod api.Module, stack []uint64) {
//	    // Add 10,000 more fuel units to the executing module.
//	    if err := experimental.AddFuel(ctx, 10_000); err != nil {
//	        // fuel not enabled — handle or ignore
//	    }
//	}
func AddFuel(ctx context.Context, amount int64) error {
	accessor, _ := ctx.Value(expctxkeys.FuelAccessorKey{}).(*expctxkeys.FuelAccessor)
	if accessor == nil || accessor.Ptr == nil {
		return ErrNoFuelAccessor
	}
	*accessor.Ptr += amount
	if accessor.Added != nil {
		*accessor.Added += amount
	}
	if observer := GetFuelObserver(ctx); observer != nil && amount != 0 {
		observer.ObserveFuel(ctx, FuelObservation{
			Module:    accessor.Module,
			Event:     FuelEventRecharged,
			Remaining: *accessor.Ptr,
			Delta:     amount,
		})
	}
	return nil
}

// RemainingFuel returns the remaining fuel for the currently executing module.
// This is intended to be called from within a host function to inspect how
// much fuel the module has left before it will exhaust.
//
// Returns (0, ErrNoFuelAccessor) if the context does not have an active fuel
// accessor.
//
// Example (from a host function):
//
//	func myHostFn(ctx context.Context, mod api.Module, stack []uint64) {
//	    remaining, err := experimental.RemainingFuel(ctx)
//	    if err == nil && remaining < 1000 {
//	        experimental.AddFuel(ctx, 10_000) // recharge
//	    }
//	}
func RemainingFuel(ctx context.Context) (int64, error) {
	accessor, _ := ctx.Value(expctxkeys.FuelAccessorKey{}).(*expctxkeys.FuelAccessor)
	if accessor == nil || accessor.Ptr == nil {
		return 0, ErrNoFuelAccessor
	}
	return *accessor.Ptr, nil
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

func isNilFuelObserver(observer FuelObserver) bool {
	if observer == nil {
		return true
	}
	v := reflect.ValueOf(observer)
	switch v.Kind() {
	case reflect.Chan, reflect.Func, reflect.Interface, reflect.Map, reflect.Pointer, reflect.Slice:
		return v.IsNil()
	default:
		return false
	}
}
