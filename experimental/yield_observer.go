package experimental

import (
	"context"
	"reflect"

	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/internal/expctxkeys"
)

// YieldEvent identifies a cooperative yield lifecycle transition.
type YieldEvent string

const (
	YieldEventYielded   YieldEvent = "yielded"
	YieldEventResumed   YieldEvent = "resumed"
	YieldEventCancelled YieldEvent = "cancelled"
)

// YieldObservation describes an opt-in async yield lifecycle event.
//
// YieldCount is 1-based for each yield of the same Wasm execution. When a time
// provider is configured, SuspendedNanos reports how long the execution stayed
// suspended before a resume or cancel event.
type YieldObservation struct {
	Module              api.Module
	Event               YieldEvent
	YieldCount          uint64
	ExpectedHostResults int
	SuspendedNanos      int64
}

// YieldObserver receives opt-in notifications for async yield lifecycle events.
type YieldObserver interface {
	ObserveYield(ctx context.Context, observation YieldObservation)
}

// YieldObserverFunc adapts a function into a YieldObserver.
type YieldObserverFunc func(context.Context, YieldObservation)

// ObserveYield implements YieldObserver.
func (f YieldObserverFunc) ObserveYield(ctx context.Context, observation YieldObservation) {
	if f != nil {
		f(ctx, observation)
	}
}

// WithYieldObserver returns a derived context with the given YieldObserver.
//
// If observer is nil, including a typed-nil YieldObserver value, ctx is
// returned unchanged.
func WithYieldObserver(ctx context.Context, observer YieldObserver) context.Context {
	if isNilYieldObserver(observer) {
		return ctx
	}
	return context.WithValue(ctx, expctxkeys.YieldObserverKey{}, observer)
}

// GetYieldObserver returns the YieldObserver from ctx, or nil if none is set.
func GetYieldObserver(ctx context.Context) YieldObserver {
	if ctx == nil {
		return nil
	}
	observer, _ := ctx.Value(expctxkeys.YieldObserverKey{}).(YieldObserver)
	if isNilYieldObserver(observer) {
		return nil
	}
	return observer
}

func isNilYieldObserver(observer YieldObserver) bool {
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
