package experimental

import (
	"context"
	"errors"
	"reflect"

	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/internal/expctxkeys"
	"github.com/tetratelabs/wazero/internal/wasmruntime"
)

// TrapCause identifies a runtime trap category.
type TrapCause string

const (
	TrapCauseStackOverflow              TrapCause = "stack_overflow"
	TrapCauseInvalidConversionToInteger TrapCause = "invalid_conversion_to_integer"
	TrapCauseIntegerOverflow            TrapCause = "integer_overflow"
	TrapCauseIntegerDivideByZero        TrapCause = "integer_divide_by_zero"
	TrapCauseUnreachable                TrapCause = "unreachable"
	TrapCauseOutOfBoundsMemoryAccess    TrapCause = "out_of_bounds_memory_access"
	TrapCauseInvalidTableAccess         TrapCause = "invalid_table_access"
	TrapCauseIndirectCallTypeMismatch   TrapCause = "indirect_call_type_mismatch"
	TrapCauseUnalignedAtomic            TrapCause = "unaligned_atomic"
	TrapCauseExpectedSharedMemory       TrapCause = "expected_shared_memory"
	TrapCauseTooManyWaiters             TrapCause = "too_many_waiters"
	TrapCauseFuelExhausted              TrapCause = "fuel_exhausted"
	TrapCausePolicyDenied               TrapCause = "policy_denied"
	TrapCauseMemoryFault                TrapCause = "memory_fault"
)

// TrapObservation describes a Wasm execution that terminated due to a runtime trap.
//
// This is intentionally small: embedders can correlate the module with their own
// request metadata via context.Context, and the wrapped Err preserves the usual
// wazero stack trace formatting.
type TrapObservation struct {
	Module api.Module
	Cause  TrapCause
	Err    error
}

// TrapObserver receives opt-in notifications when Wasm execution terminates due
// to a recognized runtime trap.
type TrapObserver interface {
	ObserveTrap(ctx context.Context, observation TrapObservation)
}

// TrapObserverFunc adapts a function into a TrapObserver.
type TrapObserverFunc func(context.Context, TrapObservation)

// ObserveTrap implements TrapObserver.
func (f TrapObserverFunc) ObserveTrap(ctx context.Context, observation TrapObservation) {
	if f != nil {
		f(ctx, observation)
	}
}

// WithTrapObserver returns a derived context with the given TrapObserver.
//
// If observer is nil, including a typed-nil TrapObserver value, ctx is returned
// unchanged.
func WithTrapObserver(ctx context.Context, observer TrapObserver) context.Context {
	if isNilTrapObserver(observer) {
		return ctx
	}
	return context.WithValue(ctx, expctxkeys.TrapObserverKey{}, observer)
}

// GetTrapObserver returns the TrapObserver from ctx, or nil if none is set.
func GetTrapObserver(ctx context.Context) TrapObserver {
	if ctx == nil {
		return nil
	}
	observer, _ := ctx.Value(expctxkeys.TrapObserverKey{}).(TrapObserver)
	if isNilTrapObserver(observer) {
		return nil
	}
	return observer
}

// TrapCauseOf returns the trap cause represented by err, if any.
func TrapCauseOf(err error) (TrapCause, bool) {
	switch {
	case errors.Is(err, wasmruntime.ErrRuntimeStackOverflow):
		return TrapCauseStackOverflow, true
	case errors.Is(err, wasmruntime.ErrRuntimeInvalidConversionToInteger):
		return TrapCauseInvalidConversionToInteger, true
	case errors.Is(err, wasmruntime.ErrRuntimeIntegerOverflow):
		return TrapCauseIntegerOverflow, true
	case errors.Is(err, wasmruntime.ErrRuntimeIntegerDivideByZero):
		return TrapCauseIntegerDivideByZero, true
	case errors.Is(err, wasmruntime.ErrRuntimeUnreachable):
		return TrapCauseUnreachable, true
	case errors.Is(err, wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess):
		return TrapCauseOutOfBoundsMemoryAccess, true
	case errors.Is(err, wasmruntime.ErrRuntimeInvalidTableAccess):
		return TrapCauseInvalidTableAccess, true
	case errors.Is(err, wasmruntime.ErrRuntimeIndirectCallTypeMismatch):
		return TrapCauseIndirectCallTypeMismatch, true
	case errors.Is(err, wasmruntime.ErrRuntimeUnalignedAtomic):
		return TrapCauseUnalignedAtomic, true
	case errors.Is(err, wasmruntime.ErrRuntimeExpectedSharedMemory):
		return TrapCauseExpectedSharedMemory, true
	case errors.Is(err, wasmruntime.ErrRuntimeTooManyWaiters):
		return TrapCauseTooManyWaiters, true
	case errors.Is(err, wasmruntime.ErrRuntimeFuelExhausted):
		return TrapCauseFuelExhausted, true
	case errors.Is(err, wasmruntime.ErrRuntimePolicyDenied):
		return TrapCausePolicyDenied, true
	case errors.Is(err, wasmruntime.ErrRuntimeMemoryFault):
		return TrapCauseMemoryFault, true
	default:
		return "", false
	}
}

func isNilTrapObserver(observer TrapObserver) bool {
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
