package experimental

import (
	"context"
	"reflect"

	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/internal/expctxkeys"
)

// HostCallPolicy decides whether a Wasm module may invoke a host function.
//
// Returning false denies the host call and causes the runtime to terminate the
// current Wasm execution with wasmruntime.ErrRuntimePolicyDenied.
//
// This is intentionally narrow: it only governs imported host function calls,
// giving embedders a concrete hook to enforce explicit allow/deny decisions
// without introducing a broader capability framework.
type HostCallPolicy interface {
	AllowHostCall(ctx context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool
}

// HostCallPolicyFunc is an adapter to allow ordinary functions to implement
// HostCallPolicy.
type HostCallPolicyFunc func(ctx context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool

// AllowHostCall implements HostCallPolicy.
//
// A nil HostCallPolicyFunc is treated as absent and therefore allows the call.
func (f HostCallPolicyFunc) AllowHostCall(ctx context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
	if f == nil {
		return true
	}
	return f(ctx, caller, hostFunction)
}

// WithHostCallPolicy returns a derived context with the given HostCallPolicy.
//
// If policy is nil, including a typed-nil policy value, ctx is returned
// unchanged.
func WithHostCallPolicy(ctx context.Context, policy HostCallPolicy) context.Context {
	if !isNilHostCallPolicy(policy) {
		return context.WithValue(ctx, expctxkeys.HostCallPolicyKey{}, policy)
	}
	return ctx
}

// GetHostCallPolicy returns the HostCallPolicy from ctx, or nil if none is set.
func GetHostCallPolicy(ctx context.Context) HostCallPolicy {
	if ctx == nil {
		return nil
	}
	policy, _ := ctx.Value(expctxkeys.HostCallPolicyKey{}).(HostCallPolicy)
	if isNilHostCallPolicy(policy) {
		return nil
	}
	return policy
}

func isNilHostCallPolicy(policy HostCallPolicy) bool {
	if policy == nil {
		return true
	}
	v := reflect.ValueOf(policy)
	switch v.Kind() {
	case reflect.Chan, reflect.Func, reflect.Interface, reflect.Map, reflect.Pointer, reflect.Slice:
		return v.IsNil()
	default:
		return false
	}
}
