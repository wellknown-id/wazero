package experimental_test

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/platform"
	"github.com/tetratelabs/wazero/internal/testing/binaryencoding"
	"github.com/tetratelabs/wazero/internal/testing/require"
	"github.com/tetratelabs/wazero/internal/wasm"
	"github.com/tetratelabs/wazero/internal/wasmruntime"
)

func TestHostCallPolicy_DeniesImportedHostFunction(t *testing.T) {
	moduleBinary := binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		ImportSection:   []wasm.Import{{Module: "env", Name: "check", Type: wasm.ExternTypeFunc, DescFunc: 0}},
		FunctionSection: []wasm.Index{0},
		CodeSection:     []wasm.Code{{Body: []byte{wasm.OpcodeCall, 0, wasm.OpcodeEnd}}},
		ExportSection:   []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 1}},
	})

	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			hostCalled := false
			_, err := rt.NewHostModuleBuilder("env").
				NewFunctionBuilder().
				WithFunc(func() { hostCalled = true }).
				Export("check").
				Instantiate(ctx)
			require.NoError(t, err)

			mod, err := rt.Instantiate(ctx, moduleBinary)
			require.NoError(t, err)

			callCtx := experimental.WithHostCallPolicy(ctx, experimental.HostCallPolicyFunc(
				func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
					require.NotNil(t, caller)
					require.Equal(t, "env.check", hostFunction.DebugName())
					return false
				},
			))

			_, err = mod.ExportedFunction("run").Call(callCtx)
			require.Error(t, err)
			require.False(t, hostCalled)
			require.True(t, errors.Is(err, wasmruntime.ErrRuntimePolicyDenied), "expected ErrRuntimePolicyDenied, got %v", err)
		})
	}
}

func TestHostCallPolicy_DoesNotAffectPureWasmExecution(t *testing.T) {
	moduleBinary := binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		FunctionSection: []wasm.Index{0},
		CodeSection:     []wasm.Code{{Body: []byte{wasm.OpcodeEnd}}},
		ExportSection:   []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 0}},
	})

	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			mod, err := rt.Instantiate(ctx, moduleBinary)
			require.NoError(t, err)

			callCtx := experimental.WithHostCallPolicy(ctx, experimental.HostCallPolicyFunc(
				func(context.Context, api.Module, api.FunctionDefinition) bool {
					t.Fatal("host call policy should not be consulted when the module makes no imported host calls")
					return false
				},
			))

			_, err = mod.ExportedFunction("run").Call(callCtx)
			require.NoError(t, err)
		})
	}
}

func TestHostCallPolicy_TypedNilIsIgnored(t *testing.T) {
	moduleBinary := binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		ImportSection:   []wasm.Import{{Module: "env", Name: "check", Type: wasm.ExternTypeFunc, DescFunc: 0}},
		FunctionSection: []wasm.Index{0},
		CodeSection:     []wasm.Code{{Body: []byte{wasm.OpcodeCall, 0, wasm.OpcodeEnd}}},
		ExportSection:   []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 1}},
	})

	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			hostCalled := false
			_, err := rt.NewHostModuleBuilder("env").
				NewFunctionBuilder().
				WithFunc(func() { hostCalled = true }).
				Export("check").
				Instantiate(ctx)
			require.NoError(t, err)

			mod, err := rt.Instantiate(ctx, moduleBinary)
			require.NoError(t, err)

			var nilPolicy experimental.HostCallPolicyFunc
			callCtx := experimental.WithHostCallPolicy(ctx, nilPolicy)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			require.NoError(t, err)
			require.True(t, hostCalled)
		})
	}
}

func TestHostCallPolicy_SelectivelyAllowsImportedHostFunction(t *testing.T) {
	moduleBinary := binaryencoding.EncodeModule(&wasm.Module{
		TypeSection: []wasm.FunctionType{{}},
		ImportSection: []wasm.Import{
			{Module: "env", Name: "allow", Type: wasm.ExternTypeFunc, DescFunc: 0},
			{Module: "env", Name: "deny", Type: wasm.ExternTypeFunc, DescFunc: 0},
		},
		FunctionSection: []wasm.Index{0},
		CodeSection: []wasm.Code{{Body: []byte{
			wasm.OpcodeCall, 0,
			wasm.OpcodeCall, 1,
			wasm.OpcodeEnd,
		}}},
		ExportSection: []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 2}},
	})

	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			allowedCalled := false
			deniedCalled := false
			_, err := rt.NewHostModuleBuilder("env").
				NewFunctionBuilder().
				WithFunc(func() { allowedCalled = true }).
				Export("allow").
				NewFunctionBuilder().
				WithFunc(func() { deniedCalled = true }).
				Export("deny").
				Instantiate(ctx)
			require.NoError(t, err)

			mod, err := rt.Instantiate(ctx, moduleBinary)
			require.NoError(t, err)

			callCtx := experimental.WithHostCallPolicy(ctx, experimental.HostCallPolicyFunc(
				func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
					require.NotNil(t, caller)
					require.Equal(t, "env", hostFunction.ModuleName())
					return slices.Contains(hostFunction.ExportNames(), "allow")
				},
			))

			_, err = mod.ExportedFunction("run").Call(callCtx)
			require.Error(t, err)
			require.True(t, allowedCalled)
			require.False(t, deniedCalled)
			require.True(t, errors.Is(err, wasmruntime.ErrRuntimePolicyDenied), "expected ErrRuntimePolicyDenied, got %v", err)
		})
	}
}
