package experimental

import (
	"context"
	"reflect"

	"github.com/tetratelabs/wazero/internal/expctxkeys"
)

// TimeProvider supplies host-visible time capabilities for a Wasm call.
//
// Embedders can attach a provider to a call context with WithTimeProvider or
// configure a runtime default with wazero.RuntimeConfig.WithTimeProvider.
// When no provider is set, existing behavior is preserved.
type TimeProvider interface {
	Walltime() (sec int64, nsec int32)
	Nanotime() int64
	Nanosleep(ns int64)
}

// WithTimeProvider returns a derived context with the given TimeProvider.
//
// If provider is nil, including a typed-nil provider value, ctx is returned
// unchanged.
func WithTimeProvider(ctx context.Context, provider TimeProvider) context.Context {
	if !isNilTimeProvider(provider) {
		return context.WithValue(ctx, expctxkeys.TimeProviderKey{}, provider)
	}
	return ctx
}

// GetTimeProvider returns the TimeProvider from ctx, or nil if none is set.
func GetTimeProvider(ctx context.Context) TimeProvider {
	if ctx == nil {
		return nil
	}
	provider, _ := ctx.Value(expctxkeys.TimeProviderKey{}).(TimeProvider)
	if isNilTimeProvider(provider) {
		return nil
	}
	return provider
}

func isNilTimeProvider(provider TimeProvider) bool {
	if provider == nil {
		return true
	}
	v := reflect.ValueOf(provider)
	switch v.Kind() {
	case reflect.Chan, reflect.Func, reflect.Interface, reflect.Map, reflect.Pointer, reflect.Slice:
		return v.IsNil()
	default:
		return false
	}
}
