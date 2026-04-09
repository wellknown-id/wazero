package wasm

import (
	"context"

	"github.com/tetratelabs/wazero/experimental"
)

// ApplyCallContextDefaults injects module-scoped runtime defaults into ctx only
// when the call context does not already define them explicitly.
func ApplyCallContextDefaults(ctx context.Context, m *ModuleInstance) context.Context {
	if m == nil {
		return ctx
	}
	if experimental.GetTimeProvider(ctx) == nil && m.TimeProvider != nil {
		ctx = experimental.WithTimeProvider(ctx, m.TimeProvider)
	}
	if experimental.GetHostCallPolicy(ctx) == nil && m.HostCallPolicy != nil {
		ctx = experimental.WithHostCallPolicy(ctx, m.HostCallPolicy)
	}
	if experimental.GetYieldPolicy(ctx) == nil && m.YieldPolicy != nil {
		ctx = experimental.WithYieldPolicy(ctx, m.YieldPolicy)
	}
	return ctx
}
