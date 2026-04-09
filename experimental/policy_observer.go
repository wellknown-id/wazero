package experimental

import (
	"context"
	"reflect"

	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/internal/expctxkeys"
)

// HostCallPolicyEvent identifies a host-call policy decision.
type HostCallPolicyEvent string

const (
	HostCallPolicyEventAllowed HostCallPolicyEvent = "allowed"
	HostCallPolicyEventDenied  HostCallPolicyEvent = "denied"
)

// HostCallPolicyObservation describes an opt-in host-call policy decision.
//
// Notifications are emitted only when a HostCallPolicy is configured and
// consulted for an imported host function call.
type HostCallPolicyObservation struct {
	Module       api.Module
	HostFunction api.FunctionDefinition
	Event        HostCallPolicyEvent
}

// YieldPolicyEvent identifies a yield-policy decision.
type YieldPolicyEvent string

const (
	YieldPolicyEventAllowed YieldPolicyEvent = "allowed"
	YieldPolicyEventDenied  YieldPolicyEvent = "denied"
)

// YieldPolicyObservation describes an opt-in yield-policy decision.
//
// Notifications are emitted only when a YieldPolicy is configured and consulted
// because a host function actually calls Yielder.Yield.
type YieldPolicyObservation struct {
	Module       api.Module
	HostFunction api.FunctionDefinition
	Event        YieldPolicyEvent
}

// HostCallPolicyObserver receives opt-in notifications for host-call policy
// decisions.
type HostCallPolicyObserver interface {
	ObserveHostCallPolicy(ctx context.Context, observation HostCallPolicyObservation)
}

// HostCallPolicyObserverFunc adapts a function into a HostCallPolicyObserver.
type HostCallPolicyObserverFunc func(context.Context, HostCallPolicyObservation)

// ObserveHostCallPolicy implements HostCallPolicyObserver.
func (f HostCallPolicyObserverFunc) ObserveHostCallPolicy(ctx context.Context, observation HostCallPolicyObservation) {
	if f != nil {
		f(ctx, observation)
	}
}

// WithHostCallPolicyObserver returns a derived context with the given
// HostCallPolicyObserver.
//
// If observer is nil, including a typed-nil HostCallPolicyObserver value, ctx
// is returned unchanged.
func WithHostCallPolicyObserver(ctx context.Context, observer HostCallPolicyObserver) context.Context {
	if isNilHostCallPolicyObserver(observer) {
		return ctx
	}
	return context.WithValue(ctx, expctxkeys.HostCallPolicyObserverKey{}, observer)
}

// GetHostCallPolicyObserver returns the HostCallPolicyObserver from ctx, or nil
// if none is set.
func GetHostCallPolicyObserver(ctx context.Context) HostCallPolicyObserver {
	if ctx == nil {
		return nil
	}
	observer, _ := ctx.Value(expctxkeys.HostCallPolicyObserverKey{}).(HostCallPolicyObserver)
	if isNilHostCallPolicyObserver(observer) {
		return nil
	}
	return observer
}

// YieldPolicyObserver receives opt-in notifications for yield-policy
// decisions.
type YieldPolicyObserver interface {
	ObserveYieldPolicy(ctx context.Context, observation YieldPolicyObservation)
}

// YieldPolicyObserverFunc adapts a function into a YieldPolicyObserver.
type YieldPolicyObserverFunc func(context.Context, YieldPolicyObservation)

// ObserveYieldPolicy implements YieldPolicyObserver.
func (f YieldPolicyObserverFunc) ObserveYieldPolicy(ctx context.Context, observation YieldPolicyObservation) {
	if f != nil {
		f(ctx, observation)
	}
}

// WithYieldPolicyObserver returns a derived context with the given
// YieldPolicyObserver.
//
// If observer is nil, including a typed-nil YieldPolicyObserver value, ctx is
// returned unchanged.
func WithYieldPolicyObserver(ctx context.Context, observer YieldPolicyObserver) context.Context {
	if isNilYieldPolicyObserver(observer) {
		return ctx
	}
	return context.WithValue(ctx, expctxkeys.YieldPolicyObserverKey{}, observer)
}

// GetYieldPolicyObserver returns the YieldPolicyObserver from ctx, or nil if
// none is set.
func GetYieldPolicyObserver(ctx context.Context) YieldPolicyObserver {
	if ctx == nil {
		return nil
	}
	observer, _ := ctx.Value(expctxkeys.YieldPolicyObserverKey{}).(YieldPolicyObserver)
	if isNilYieldPolicyObserver(observer) {
		return nil
	}
	return observer
}

func isNilHostCallPolicyObserver(observer HostCallPolicyObserver) bool {
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

func isNilYieldPolicyObserver(observer YieldPolicyObserver) bool {
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
