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

// YieldPolicy decides whether a host function may cooperatively suspend Wasm
// execution via Yielder.Yield.
//
// Returning false denies the yield and causes the runtime to terminate the
// current Wasm execution with wasmruntime.ErrRuntimePolicyDenied.
//
// This is intentionally narrow: it only governs suspension at the explicit
// async yield boundary, giving embedders a concrete hook to require allow/deny
// decisions for host-driven suspension without changing default behavior when
// no policy is configured.
type YieldPolicy interface {
	AllowYield(ctx context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool
}

// HostCallPolicyFunc is an adapter to allow ordinary functions to implement
// HostCallPolicy.
type HostCallPolicyFunc func(ctx context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool

// YieldPolicyFunc is an adapter to allow ordinary functions to implement
// YieldPolicy.
type YieldPolicyFunc func(ctx context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool

// AllowHostCall implements HostCallPolicy.
//
// A nil HostCallPolicyFunc is treated as absent and therefore allows the call.
func (f HostCallPolicyFunc) AllowHostCall(ctx context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
	if f == nil {
		return true
	}
	return f(ctx, caller, hostFunction)
}

// AllowYield implements YieldPolicy.
//
// A nil YieldPolicyFunc is treated as absent and therefore allows the yield.
func (f YieldPolicyFunc) AllowYield(ctx context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
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

// WithYieldPolicy returns a derived context with the given YieldPolicy.
//
// If policy is nil, including a typed-nil policy value, ctx is returned
// unchanged.
func WithYieldPolicy(ctx context.Context, policy YieldPolicy) context.Context {
	if !isNilYieldPolicy(policy) {
		return context.WithValue(ctx, expctxkeys.YieldPolicyKey{}, policy)
	}
	return ctx
}

// GetYieldPolicy returns the YieldPolicy from ctx, or nil if none is set.
func GetYieldPolicy(ctx context.Context) YieldPolicy {
	if ctx == nil {
		return nil
	}
	policy, _ := ctx.Value(expctxkeys.YieldPolicyKey{}).(YieldPolicy)
	if isNilYieldPolicy(policy) {
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

func isNilYieldPolicy(policy YieldPolicy) bool {
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
