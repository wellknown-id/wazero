package experimental_test

import (
	"context"
	"testing"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/platform"
	"github.com/tetratelabs/wazero/internal/testing/binaryencoding"
	"github.com/tetratelabs/wazero/internal/testing/require"
	"github.com/tetratelabs/wazero/internal/wasm"
)

func hostInspectModuleBinary() []byte {
	return binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		ImportSection:   []wasm.Import{{Module: "env", Name: "inspect", Type: wasm.ExternTypeFunc, DescFunc: 0}},
		FunctionSection: []wasm.Index{0},
		CodeSection:     []wasm.Code{{Body: []byte{wasm.OpcodeCall, 0, wasm.OpcodeEnd}}},
		ExportSection:   []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 1}},
	})
}

func TestWithFuel_InterpreterIgnoresFuel(t *testing.T) {
	ctx := context.Background()
	rt := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfigInterpreter().WithFuel(5))
	defer rt.Close(ctx)

	var fuelErr error
	hostCalled := false
	_, err := rt.NewHostModuleBuilder("env").
		NewFunctionBuilder().
		WithGoModuleFunction(api.GoModuleFunc(func(ctx context.Context, mod api.Module, stack []uint64) {
			hostCalled = true
			_, fuelErr = experimental.RemainingFuel(ctx)
		}), nil, nil).
		Export("inspect").
		Instantiate(ctx)
	require.NoError(t, err)

	mod, err := rt.Instantiate(ctx, hostInspectModuleBinary())
	require.NoError(t, err)

	_, err = mod.ExportedFunction("run").Call(ctx)
	require.NoError(t, err)
	require.True(t, hostCalled)
	require.ErrorIs(t, fuelErr, experimental.ErrNoFuelAccessor)
}

func TestWithFuelController_OverridesRuntimeBudget(t *testing.T) {
	if !platform.CompilerSupported() {
		t.Skip("compiler is not supported on this host")
	}

	ctx := context.Background()
	rt := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfigCompiler().WithFuel(1))
	defer rt.Close(ctx)

	var remaining int64
	var fuelErr error
	_, err := rt.NewHostModuleBuilder("env").
		NewFunctionBuilder().
		WithGoModuleFunction(api.GoModuleFunc(func(ctx context.Context, mod api.Module, stack []uint64) {
			remaining, fuelErr = experimental.RemainingFuel(ctx)
		}), nil, nil).
		Export("inspect").
		Instantiate(ctx)
	require.NoError(t, err)

	mod, err := rt.Instantiate(ctx, hostInspectModuleBinary())
	require.NoError(t, err)

	ctrl := experimental.NewSimpleFuelController(10)
	callCtx := experimental.WithFuelController(ctx, ctrl)

	_, err = mod.ExportedFunction("run").Call(callCtx)
	require.NoError(t, err)
	require.NoError(t, fuelErr)
	if remaining <= 1 {
		t.Fatalf("expected controller budget to override runtime fuel budget, remaining fuel = %d", remaining)
	}
	if ctrl.TotalConsumed() <= 0 {
		t.Fatalf("expected fuel controller to record consumption, got %d", ctrl.TotalConsumed())
	}
}
