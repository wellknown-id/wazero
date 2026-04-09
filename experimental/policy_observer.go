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
