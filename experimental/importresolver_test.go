package experimental_test

import (
	"context"
	"fmt"
	"reflect"
	"testing"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/testing/binaryencoding"
	"github.com/tetratelabs/wazero/internal/testing/require"
	"github.com/tetratelabs/wazero/internal/wasm"
)

func TestWithImportResolver_NilDoesNothing(t *testing.T) {
	ctx := context.Background()

	result := experimental.WithImportResolver(ctx, nil)
	if result != ctx {
		t.Fatal("WithImportResolver(ctx, nil) should return the same context")
	}

	var resolver experimental.ImportResolver
	result = experimental.WithImportResolver(ctx, resolver)
	if result != ctx {
		t.Fatal("WithImportResolver should ignore typed-nil ImportResolver values")
	}
}

func TestImportResolverConfig_NilDoesNothing(t *testing.T) {
	ctx := context.Background()

	result := experimental.WithImportResolverConfig(ctx, experimental.ImportResolverConfig{})
	if result != ctx {
		t.Fatal("WithImportResolverConfig should ignore empty configs")
	}
}

func TestWithImportResolverACL_NilDoesNothing(t *testing.T) {
	ctx := context.Background()

	if result := experimental.WithImportResolverACL(ctx, nil); result != ctx {
		t.Fatal("WithImportResolverACL(ctx, nil) should return the same context")
	}

	if result := experimental.WithImportResolverACL(ctx, experimental.NewImportACL()); result != ctx {
		t.Fatal("WithImportResolverACL should ignore empty ACLs")
	}
}

func TestImportResolverConfig_ACLOnlyRoundTrip(t *testing.T) {
	ctx := context.Background()
	acl := experimental.NewImportACL().DenyModules("env")
	cfgCtx := experimental.WithImportResolverConfig(ctx, experimental.ImportResolverConfig{
		ACL: acl,
	})

	got := experimental.GetImportResolverConfig(cfgCtx)
	require.NotNil(t, got)
	require.True(t, got.ACL == acl)
	require.True(t, experimental.GetImportResolver(cfgCtx) == nil)
}

func TestWithImportResolverACL_PreservesExistingResolver(t *testing.T) {
	ctx := context.Background()
	resolver := experimental.ImportResolver(func(string) api.Module { return nil })
	acl := experimental.NewImportACL().AllowModules("env")

	got := experimental.GetImportResolverConfig(experimental.WithImportResolverACL(
		experimental.WithImportResolver(ctx, resolver),
		acl,
	))
	require.NotNil(t, got)
	require.True(t, reflect.ValueOf(got.Resolver).Pointer() == reflect.ValueOf(resolver).Pointer())
	require.True(t, got.ACL == acl)
}

func TestImportResolver(t *testing.T) {
	tests := []struct {
		name              string
		registerStoreEnv  bool
		configureContext  func(context.Context, api.Module) context.Context
		wantStoreCalls    int
		wantResolvedCalls int
		wantErrSubstring  string
	}{
		{
			name:             "success",
			registerStoreEnv: false,
			configureContext: func(ctx context.Context, resolved api.Module) context.Context {
				return experimental.WithImportResolver(ctx, func(name string) api.Module {
					if name == "env" {
						return resolved
					}
					return nil
				})
			},
			wantResolvedCalls: 1,
		},
		{
			name:             "acl allows fallback by exact name",
			registerStoreEnv: true,
			configureContext: func(ctx context.Context, _ api.Module) context.Context {
				return experimental.WithImportResolverACL(ctx, experimental.NewImportACL().AllowModules("env"))
			},
			wantStoreCalls: 1,
		},
		{
			name:             "fail closed blocks store fallback without resolver",
			registerStoreEnv: true,
			configureContext: func(ctx context.Context, _ api.Module) context.Context {
				return experimental.WithImportResolverConfig(ctx, experimental.ImportResolverConfig{
					ACL:        experimental.NewImportACL().AllowModules("env"),
					FailClosed: true,
				})
			},
			wantErrSubstring: "module[env] unresolved by import resolver",
		},
		{
			name:             "acl denies store import by exact name",
			registerStoreEnv: true,
			configureContext: func(ctx context.Context, _ api.Module) context.Context {
				return experimental.WithImportResolverACL(ctx, experimental.NewImportACL().DenyModules("env"))
			},
			wantErrSubstring: "module[env] denied by import ACL",
		},
		{
			name:             "acl allowlist blocks unlisted import",
			registerStoreEnv: true,
			configureContext: func(ctx context.Context, _ api.Module) context.Context {
				return experimental.WithImportResolverACL(ctx, experimental.NewImportACL().AllowModules("wasi_snapshot_preview1"))
			},
			wantErrSubstring: "module[env] not allowed by import ACL",
		},
		{
			name:             "acl allows prefix match",
			registerStoreEnv: true,
			configureContext: func(ctx context.Context, _ api.Module) context.Context {
				return experimental.WithImportResolverACL(ctx, experimental.NewImportACL().AllowModulePrefixes("en"))
			},
			wantStoreCalls: 1,
		},
		{
			name:             "denial",
			registerStoreEnv: true,
			configureContext: func(ctx context.Context, _ api.Module) context.Context {
				return experimental.WithImportResolverConfig(ctx, experimental.ImportResolverConfig{
					Resolver:   func(string) api.Module { return nil },
					FailClosed: true,
				})
			},
			wantErrSubstring: "module[env] unresolved by import resolver",
		},
		{
			name:             "resolver takes precedence over store",
			registerStoreEnv: true,
			configureContext: func(ctx context.Context, resolved api.Module) context.Context {
				return experimental.WithImportResolverConfig(ctx, experimental.ImportResolverConfig{
					Resolver: func(name string) api.Module {
						if name == "env" {
							return resolved
						}
						return nil
					},
				})
			},
			wantResolvedCalls: 1,
		},
	}

	for _, tc := range tests {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()

			r := wazero.NewRuntime(ctx)
			defer r.Close(ctx)

			var storeCalls, resolvedCalls int
			if tc.registerStoreEnv {
				_, err := instantiateStartModule(ctx, r, "env", func(context.Context) { storeCalls++ })
				require.NoError(t, err)
			}

			resolved, err := instantiateStartModule(ctx, r, "", func(context.Context) { resolvedCalls++ })
			require.NoError(t, err)

			modMain, err := r.CompileModule(ctx, testImportResolverModule())
			require.NoError(t, err)

			callCtx := tc.configureContext(ctx, resolved)
			_, err = r.InstantiateModule(callCtx, modMain, wazero.NewModuleConfig())
			if tc.wantErrSubstring != "" {
				require.Error(t, err)
				require.Contains(t, err.Error(), tc.wantErrSubstring)
			} else {
				require.NoError(t, err)
			}

			require.Equal(t, tc.wantStoreCalls, storeCalls)
			require.Equal(t, tc.wantResolvedCalls, resolvedCalls)
		})
	}
}

func instantiateStartModule(ctx context.Context, r wazero.Runtime, name string, start func(context.Context)) (api.Module, error) {
	mod, err := r.NewHostModuleBuilder(fmt.Sprintf("env-%s", name)).
		NewFunctionBuilder().WithFunc(start).Export("start").
		Compile(ctx)
	if err != nil {
		return nil, err
	}
	return r.InstantiateModule(ctx, mod, wazero.NewModuleConfig().WithName(name))
}

func testImportResolverModule() []byte {
	one := uint32(1)
	return binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		ImportSection:   []wasm.Import{{Module: "env", Name: "start", Type: wasm.ExternTypeFunc, DescFunc: 0}},
		FunctionSection: []wasm.Index{0},
		CodeSection: []wasm.Code{
			{Body: []byte{wasm.OpcodeCall, 0, wasm.OpcodeEnd}},
		},
		StartSection: &one,
	})
}
