package experimental_test

import (
	"context"
	"errors"
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
