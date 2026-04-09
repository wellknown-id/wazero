package experimental

import (
	"context"
	"reflect"

	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/internal/expctxkeys"
)

// ImportResolver is an experimental func type that, if set,
// will be used as the first step in resolving imports.
// See issue 2294.
// If the import name is not found, it should return nil.
type ImportResolver func(name string) api.Module

// ImportResolverConfig controls import resolution during instantiation.
//
// Resolver is consulted before the runtime falls back to resolving imports from
// instantiated modules in the store. When FailClosed is true, returning nil
// from Resolver explicitly denies that fallback and instantiation fails.
type ImportResolverConfig struct {
	Resolver   ImportResolver
	FailClosed bool
}

// WithImportResolver returns a new context with the given ImportResolver.
func WithImportResolver(ctx context.Context, resolver ImportResolver) context.Context {
	if isNilImportResolver(resolver) {
		return ctx
	}
	return context.WithValue(ctx, expctxkeys.ImportResolverKey{}, resolver)
}

// GetImportResolver returns the ImportResolver from ctx, or nil if none is set.
//
// If ctx contains an ImportResolverConfig, its Resolver is returned.
func GetImportResolver(ctx context.Context) ImportResolver {
	if ctx == nil {
		return nil
	}
	switch v := ctx.Value(expctxkeys.ImportResolverKey{}).(type) {
	case ImportResolver:
		if isNilImportResolver(v) {
			return nil
		}
		return v
	case ImportResolverConfig:
		if isNilImportResolver(v.Resolver) {
			return nil
		}
		return v.Resolver
	default:
		return nil
	}
}

// WithImportResolverConfig returns a derived context with the given
// ImportResolverConfig.
//
// If cfg.Resolver is nil, including a typed-nil resolver value, ctx is returned
// unchanged.
func WithImportResolverConfig(ctx context.Context, cfg ImportResolverConfig) context.Context {
	if isNilImportResolver(cfg.Resolver) {
		return ctx
	}
	return context.WithValue(ctx, expctxkeys.ImportResolverKey{}, cfg)
}

// GetImportResolverConfig returns the ImportResolverConfig from ctx, or nil if
// none is set.
//
// If ctx contains only an ImportResolver, it is surfaced as a config with
// FailClosed set to false.
func GetImportResolverConfig(ctx context.Context) *ImportResolverConfig {
	if ctx == nil {
		return nil
	}
	switch v := ctx.Value(expctxkeys.ImportResolverKey{}).(type) {
	case ImportResolverConfig:
		if isNilImportResolver(v.Resolver) {
			return nil
		}
		cfg := v
		return &cfg
	case ImportResolver:
		if isNilImportResolver(v) {
			return nil
		}
		cfg := ImportResolverConfig{Resolver: v}
		return &cfg
	default:
		return nil
	}
}

func isNilImportResolver(resolver ImportResolver) bool {
	if resolver == nil {
		return true
	}
	v := reflect.ValueOf(resolver)
	return v.Kind() == reflect.Func && v.IsNil()
}
