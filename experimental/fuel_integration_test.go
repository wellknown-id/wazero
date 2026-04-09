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

type fuelAdjustingYieldingHostFunc struct {
	t                    *testing.T
	expectedFuelObserver experimental.FuelObserver
	adjustment           int64
	calls                int
	beforeAdjustment     int64
	afterAdjustment      int64
}

func (f *fuelAdjustingYieldingHostFunc) Call(ctx context.Context, _ api.Module, stack []uint64) {
	f.calls++
	switch f.calls {
	case 1:
		yielder := experimental.GetYielder(ctx)
		if yielder == nil {
			f.t.Fatal("expected yielder in context")
		}
		yielder.Yield()
	case 2:
		if got := experimental.GetFuelObserver(ctx); got != f.expectedFuelObserver {
			f.t.Fatalf("fuel observer = %#v, want %#v", got, f.expectedFuelObserver)
		}
		var err error
		f.beforeAdjustment, err = experimental.RemainingFuel(ctx)
		require.NoError(f.t, err)
		require.NoError(f.t, experimental.AddFuel(ctx, f.adjustment))
		f.afterAdjustment, err = experimental.RemainingFuel(ctx)
		require.NoError(f.t, err)
		stack[0] = 2
	default:
		f.t.Fatalf("unexpected host call %d", f.calls)
	}
}

func TestWithFuel_InterpreterExposesFuelAccessor(t *testing.T) {
	ctx := context.Background()
	rt := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfigInterpreter().WithFuel(5))
	defer rt.Close(ctx)

	var remaining int64
	var fuelErr error
	hostCalled := false
	_, err := rt.NewHostModuleBuilder("env").
		NewFunctionBuilder().
		WithGoModuleFunction(api.GoModuleFunc(func(ctx context.Context, mod api.Module, stack []uint64) {
			hostCalled = true
			remaining, fuelErr = experimental.RemainingFuel(ctx)
		}), nil, nil).
		Export("inspect").
		Instantiate(ctx)
	require.NoError(t, err)

	mod, err := rt.Instantiate(ctx, hostInspectModuleBinary())
	require.NoError(t, err)

	_, err = mod.ExportedFunction("run").Call(ctx)
	require.NoError(t, err)
	require.True(t, hostCalled)
	require.NoError(t, fuelErr)
	require.True(t, remaining > 0 && remaining < 5, "remaining fuel = %d", remaining)
}

func TestWithFuelController_OverridesRuntimeBudget(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg.WithFuel(1))
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
		})
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
		for _, ec := range engineConfigs() {
			t.Run(fmt.Sprintf("%s/%s", ec.name, tc.name), func(t *testing.T) {
				if ec.name == "compiler" && !platform.CompilerSupported() {
					t.Skip("compiler is not supported on this host")
				}

				ctx := context.Background()
				rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg.WithFuel(1))
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
}

func TestFuelObserver_Lifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg.WithFuel(1))
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
		})
	}
}

func TestFuelObserver_Exhaustion(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg.WithFuel(3))
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
		})
	}
}

func TestFuelObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			ctx := context.Background()
			mod, rt, _ := setupYieldTest(t, ec.cfg)
			defer rt.Close(ctx)

			observer := &recordingFuelObserver{}
			ctrl := experimental.NewSimpleFuelController(20)
			callCtx := experimental.WithFuelObserver(
				experimental.WithFuelController(
					experimental.WithYielder(ctx),
					ctrl,
				),
				observer,
			)

			_, err := mod.ExportedFunction("run").Call(callCtx)
			resumer := requireYieldError(t, err).Resumer()

			observations := observer.snapshot()
			require.Equal(t, 1, len(observations))
			require.Equal(t, experimental.FuelEventBudgeted, observations[0].Event)
			require.Equal(t, int64(20), observations[0].Budget)
			require.Equal(t, int64(20), observations[0].Remaining)

			results, err := resumer.Resume(experimental.WithYielder(ctx), []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			observations = observer.snapshot()
			require.Equal(t, 2, len(observations))
			require.Equal(t, experimental.FuelEventConsumed, observations[1].Event)
			require.Equal(t, int64(20), observations[1].Budget)
			require.Equal(t, observations[1].Consumed, ctrl.TotalConsumed())
			require.Equal(t, observations[1].Budget-observations[1].Consumed, observations[1].Remaining)
		})
	}
}

func TestFuelObserver_YieldResumeUsesResumedObserver(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			ctx := context.Background()
			host := &fuelAdjustingYieldingHostFunc{t: t, adjustment: 5}
			mod, rt, _ := setupYieldTestWithHost(t, ec.cfg, host)
			defer rt.Close(ctx)

			initialObserver := &recordingFuelObserver{}
			ctrl := experimental.NewSimpleFuelController(20)
			_, err := mod.ExportedFunction("run_twice").Call(
				experimental.WithFuelObserver(
					experimental.WithFuelController(
						experimental.WithYielder(ctx),
						ctrl,
					),
					initialObserver,
				),
			)
			firstResumer := requireYieldError(t, err).Resumer()

			resumeObserver := &recordingFuelObserver{}
			host.expectedFuelObserver = resumeObserver
			results, err := firstResumer.Resume(
				experimental.WithFuelObserver(experimental.WithYielder(ctx), resumeObserver),
				[]uint64{40},
			)
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)
			require.Equal(t, host.beforeAdjustment+int64(5), host.afterAdjustment)

			initialObservations := initialObserver.snapshot()
			require.Equal(t, 1, len(initialObservations))
			require.Equal(t, experimental.FuelEventBudgeted, initialObservations[0].Event)
			require.Equal(t, int64(20), initialObservations[0].Budget)

			resumeObservations := resumeObserver.snapshot()
			require.Equal(t, 2, len(resumeObservations))
			require.Equal(t, experimental.FuelEventRecharged, resumeObservations[0].Event)
			require.Equal(t, int64(5), resumeObservations[0].Delta)
			require.Equal(t, host.afterAdjustment, resumeObservations[0].Remaining)
			require.Equal(t, experimental.FuelEventConsumed, resumeObservations[1].Event)
			require.Equal(t, int64(25), resumeObservations[1].Budget)
			require.Equal(t, resumeObservations[1].Consumed, ctrl.TotalConsumed())
		})
	}
}

func TestFuelObserver_YieldReyieldUsesResumedObserver(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			ctx := context.Background()
			mod, rt, _ := setupYieldTest(t, ec.cfg)
			defer rt.Close(ctx)

			initialObserver := &recordingFuelObserver{}
			ctrl := experimental.NewSimpleFuelController(20)
			_, err := mod.ExportedFunction("run_twice").Call(
				experimental.WithFuelObserver(
					experimental.WithFuelController(
						experimental.WithYielder(ctx),
						ctrl,
					),
					initialObserver,
				),
			)
			firstResumer := requireYieldError(t, err).Resumer()

			resumeObserver := &recordingFuelObserver{}
			_, err = firstResumer.Resume(
				experimental.WithFuelObserver(experimental.WithYielder(ctx), resumeObserver),
				[]uint64{40},
			)
			secondYield := requireYieldError(t, err)

			require.Zero(t, len(resumeObserver.snapshot()))

			results, err := secondYield.Resumer().Resume(experimental.WithYielder(ctx), []uint64{2})
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)

			initialObservations := initialObserver.snapshot()
			require.Equal(t, 1, len(initialObservations))
			require.Equal(t, experimental.FuelEventBudgeted, initialObservations[0].Event)
			require.Equal(t, int64(20), initialObservations[0].Budget)

			resumeObservations := resumeObserver.snapshot()
			require.Equal(t, 1, len(resumeObservations))
			require.Equal(t, experimental.FuelEventConsumed, resumeObservations[0].Event)
			require.Equal(t, int64(20), resumeObservations[0].Budget)
			require.Equal(t, resumeObservations[0].Consumed, ctrl.TotalConsumed())
			require.Equal(t, resumeObservations[0].Budget-resumeObservations[0].Consumed, resumeObservations[0].Remaining)
		})
	}
}
