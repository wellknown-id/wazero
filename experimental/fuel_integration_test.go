package experimental_test

import (
	"context"
	"fmt"
	"sync"
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

func hostInspectModuleBinary() []byte {
	return binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		ImportSection:   []wasm.Import{{Module: "env", Name: "inspect", Type: wasm.ExternTypeFunc, DescFunc: 0}},
		FunctionSection: []wasm.Index{0},
		CodeSection:     []wasm.Code{{Body: []byte{wasm.OpcodeCall, 0, wasm.OpcodeEnd}}},
		ExportSection:   []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 1}},
	})
}

func hostAdjustThenCallModuleBinary() []byte {
	return binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		ImportSection:   []wasm.Import{{Module: "env", Name: "adjust", Type: wasm.ExternTypeFunc, DescFunc: 0}},
		FunctionSection: []wasm.Index{0, 0},
		CodeSection: []wasm.Code{
			{Body: []byte{wasm.OpcodeEnd}},
			{Body: []byte{wasm.OpcodeCall, 0, wasm.OpcodeCall, 1, wasm.OpcodeEnd}},
		},
		ExportSection: []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 2}},
	})
}

func fuelLoopModuleBinary() []byte {
	return binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		FunctionSection: []wasm.Index{0},
		CodeSection: []wasm.Code{{
			Body: []byte{
				wasm.OpcodeLoop, 0x40,
				wasm.OpcodeBr, 0x00,
				wasm.OpcodeEnd,
				wasm.OpcodeEnd,
			},
		}},
		ExportSection: []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 0}},
	})
}

type recordingFuelObserver struct {
	mu           sync.Mutex
	observations []experimental.FuelObservation
}

func (r *recordingFuelObserver) ObserveFuel(_ context.Context, observation experimental.FuelObservation) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.observations = append(r.observations, observation)
}

func (r *recordingFuelObserver) snapshot() []experimental.FuelObservation {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make([]experimental.FuelObservation, len(r.observations))
	copy(out, r.observations)
	return out
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

func TestWithFuelController_NonPositiveBudgetDisablesFuelMetering(t *testing.T) {
	if !platform.CompilerSupported() {
		t.Skip("compiler is not supported on this host")
	}

	for _, budget := range []int64{0, -5} {
		t.Run(fmt.Sprintf("budget_%d", budget), func(t *testing.T) {
			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfigCompiler().WithFuel(1))
			defer rt.Close(ctx)

			hostCalled := false
			var fuelErr error
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

			ctrl := experimental.NewSimpleFuelController(budget)
			callCtx := experimental.WithFuelController(ctx, ctrl)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			require.NoError(t, err)
			require.True(t, hostCalled)
			require.ErrorIs(t, fuelErr, experimental.ErrNoFuelAccessor)
			require.Zero(t, ctrl.TotalConsumed())
		})
	}
}

func TestAddFuel_HostAdjustmentControlsNextFuelCheck(t *testing.T) {
	if !platform.CompilerSupported() {
		t.Skip("compiler is not supported on this host")
	}

	tests := []struct {
		name       string
		budget     int64
		adjustment int64
	}{
		{name: "no recharge exhausts", budget: 1, adjustment: 0},
		{name: "recharge rescues execution", budget: 1, adjustment: 2},
		{name: "small debit still succeeds", budget: 3, adjustment: -1},
		{name: "debit forces exhaustion", budget: 2, adjustment: -2},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfigCompiler().WithFuel(1))
			defer rt.Close(ctx)

			hostCalled := false
			var before, after int64
			var remainingErr, addErr error
			_, err := rt.NewHostModuleBuilder("env").
				NewFunctionBuilder().
				WithGoModuleFunction(api.GoModuleFunc(func(ctx context.Context, mod api.Module, stack []uint64) {
					hostCalled = true
					before, remainingErr = experimental.RemainingFuel(ctx)
					if remainingErr != nil {
						return
					}
					addErr = experimental.AddFuel(ctx, tc.adjustment)
					if addErr != nil {
						return
					}
					after, remainingErr = experimental.RemainingFuel(ctx)
				}), nil, nil).
				Export("adjust").
				Instantiate(ctx)
			require.NoError(t, err)

			mod, err := rt.Instantiate(ctx, hostAdjustThenCallModuleBinary())
			require.NoError(t, err)

			ctrl := experimental.NewSimpleFuelController(tc.budget)
			callCtx := experimental.WithFuelController(ctx, ctrl)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			require.True(t, hostCalled)
			require.NoError(t, remainingErr)
			require.NoError(t, addErr)
			require.Equal(t, before+tc.adjustment, after)
			if after > 0 {
				require.NoError(t, err)
			} else {
				require.ErrorIs(t, err, wasmruntime.ErrRuntimeFuelExhausted)
			}
			require.True(t, ctrl.TotalConsumed() > 0)
		})
	}
}

func TestFuelObserver_CompilerLifecycle(t *testing.T) {
	if !platform.CompilerSupported() {
		t.Skip("compiler is not supported on this host")
	}

	ctx := context.Background()
	rt := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfigCompiler().WithFuel(1))
	defer rt.Close(ctx)

	var beforeRecharge, afterRecharge int64
	var fuelErr error
	_, err := rt.NewHostModuleBuilder("env").
		NewFunctionBuilder().
		WithGoModuleFunction(api.GoModuleFunc(func(ctx context.Context, mod api.Module, stack []uint64) {
			beforeRecharge, fuelErr = experimental.RemainingFuel(ctx)
			require.NoError(t, fuelErr)
			require.NoError(t, experimental.AddFuel(ctx, 5))
			afterRecharge, fuelErr = experimental.RemainingFuel(ctx)
			require.NoError(t, fuelErr)
		}), nil, nil).
		Export("inspect").
		Instantiate(ctx)
	require.NoError(t, err)

	mod, err := rt.InstantiateWithConfig(ctx, hostInspectModuleBinary(), wazero.NewModuleConfig().WithName("fuel-guest"))
	require.NoError(t, err)

	observer := &recordingFuelObserver{}
	ctrl := experimental.NewSimpleFuelController(10)
	callCtx := experimental.WithFuelObserver(experimental.WithFuelController(ctx, ctrl), observer)

	_, err = mod.ExportedFunction("run").Call(callCtx)
	require.NoError(t, err)
	require.NoError(t, fuelErr)
	require.True(t, afterRecharge > beforeRecharge)

	observations := observer.snapshot()
	require.Equal(t, 3, len(observations))
	require.Equal(t, experimental.FuelEventBudgeted, observations[0].Event)
	require.Equal(t, int64(10), observations[0].Budget)
	require.Equal(t, int64(10), observations[0].Remaining)

	require.Equal(t, experimental.FuelEventRecharged, observations[1].Event)
	require.Equal(t, int64(5), observations[1].Delta)
	require.Equal(t, afterRecharge, observations[1].Remaining)
	require.Equal(t, "fuel-guest", observations[1].Module.Name())

	require.Equal(t, experimental.FuelEventConsumed, observations[2].Event)
	require.Equal(t, int64(15), observations[2].Budget)
	require.True(t, observations[2].Consumed > 0)
	require.Equal(t, observations[2].Budget-observations[2].Consumed, observations[2].Remaining)
	require.Equal(t, observations[2].Consumed, ctrl.TotalConsumed())
}

func TestFuelObserver_CompilerExhaustion(t *testing.T) {
	if !platform.CompilerSupported() {
		t.Skip("compiler is not supported on this host")
	}

	ctx := context.Background()
	rt := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfigCompiler().WithFuel(3))
	defer rt.Close(ctx)

	mod, err := rt.InstantiateWithConfig(ctx, fuelLoopModuleBinary(), wazero.NewModuleConfig().WithName("fuel-loop"))
	require.NoError(t, err)

	observer := &recordingFuelObserver{}
	_, err = mod.ExportedFunction("run").Call(experimental.WithFuelObserver(ctx, observer))
	require.ErrorIs(t, err, wasmruntime.ErrRuntimeFuelExhausted)

	observations := observer.snapshot()
	require.Equal(t, 2, len(observations))
	require.Equal(t, experimental.FuelEventBudgeted, observations[0].Event)
	require.Equal(t, int64(3), observations[0].Budget)
	require.Equal(t, experimental.FuelEventExhausted, observations[1].Event)
	require.Equal(t, int64(3), observations[1].Budget)
	require.True(t, observations[1].Consumed >= observations[1].Budget)
	require.True(t, observations[1].Remaining <= 0)
	require.Equal(t, "fuel-loop", observations[1].Module.Name())
}
