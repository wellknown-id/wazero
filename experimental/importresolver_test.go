package experimental_test

import (
	"context"
	"fmt"
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
