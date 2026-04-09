package expctxkeys

import "github.com/tetratelabs/wazero/api"

// FuelControllerKey is a context.Context key for the experimental fuel controller.
type FuelControllerKey struct{}

// FuelObserverKey is a context.Context key for the experimental fuel observer.
type FuelObserverKey struct{}

// FuelAccessorKey is a context.Context key for the runtime fuel accessor.
// The value is a *FuelAccessor that provides AddFuel/RemainingFuel during host
// function calls. It is set by the call engine and only valid during the host
// function's execution scope.
type FuelAccessorKey struct{}

// FuelAccessor provides direct access to the execution context's fuel counter.
// This is set by the call engine before dispatching to host functions and
// allows hosts to add or inspect fuel mid-execution.
type FuelAccessor struct {
	Ptr    *int64 // points to executionContext.fuel
	Module api.Module
	Added  *int64
}
