package experimental

import (
	"context"

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
func (f HostCallPolicyFunc) AllowHostCall(ctx context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
	return f(ctx, caller, hostFunction)
}

// WithHostCallPolicy returns a derived context with the given HostCallPolicy.
//
// If policy is nil, ctx is returned unchanged.
func WithHostCallPolicy(ctx context.Context, policy HostCallPolicy) context.Context {
	if policy != nil {
		return context.WithValue(ctx, expctxkeys.HostCallPolicyKey{}, policy)
	}
	return ctx
}

// GetHostCallPolicy returns the HostCallPolicy from ctx, or nil if none is set.
func GetHostCallPolicy(ctx context.Context) HostCallPolicy {
	policy, _ := ctx.Value(expctxkeys.HostCallPolicyKey{}).(HostCallPolicy)
	return policy
}
