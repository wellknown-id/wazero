package experimental_test

import (
	"context"
	"errors"
	"testing"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/platform"
	"github.com/tetratelabs/wazero/internal/testing/require"
	"github.com/tetratelabs/wazero/internal/wasmruntime"
)

type fuelObservationSnapshot struct {
	Event     experimental.FuelEvent
	Budget    int64
	Consumed  int64
	Remaining int64
	Delta     int64
}

type forwardingFuelObserver struct {
	inner experimental.FuelObserver
}

func (f *forwardingFuelObserver) ObserveFuel(ctx context.Context, observation experimental.FuelObservation) {
	f.inner.ObserveFuel(ctx, observation)
}

func snapshotFuelObservations(observations []experimental.FuelObservation) []fuelObservationSnapshot {
	snapshots := make([]fuelObservationSnapshot, len(observations))
	for i, observation := range observations {
		snapshots[i] = fuelObservationSnapshot{
			Event:     observation.Event,
			Budget:    observation.Budget,
			Consumed:  observation.Consumed,
			Remaining: observation.Remaining,
			Delta:     observation.Delta,
		}
	}
	return snapshots
}

func TestMultiYieldObserver_ReyieldUsesResumedObserver(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			provider := newObservingTimeProvider()
			mod, rt, ctx := setupYieldTest(t, ec.cfg.WithTimeProvider(provider))
			defer rt.Close(ctx)

			initialLeft := &recordingYieldObserver{}
			initialRight := &recordingYieldObserver{}
			_, err := mod.ExportedFunction("run_twice").Call(
				experimental.WithYieldObserver(
					experimental.WithYielder(ctx),
					experimental.MultiYieldObserver(initialLeft, initialRight),
				),
			)
			firstResumer := requireYieldError(t, err).Resumer()

			resumeLeft := &recordingYieldObserver{}
			resumeRight := &recordingYieldObserver{}
			resumeProvider := newObservingTimeProvider()
			_, err = firstResumer.Resume(
				experimental.WithYieldObserver(
					experimental.WithTimeProvider(experimental.WithYielder(ctx), resumeProvider),
					experimental.MultiYieldObserver(resumeLeft, resumeRight),
				),
				[]uint64{40},
			)
			secondYield := requireYieldError(t, err)
			secondYield.Resumer().Cancel()

			wantInitial := []yieldObservationSnapshot{{
				Event:               experimental.YieldEventYielded,
				YieldCount:          1,
				ExpectedHostResults: 1,
				SuspendedNanos:      0,
			}}
			wantResumed := []yieldObservationSnapshot{
				{
					Event:               experimental.YieldEventResumed,
					YieldCount:          1,
					ExpectedHostResults: 1,
					SuspendedNanos:      1_000_000,
				},
				{
					Event:               experimental.YieldEventYielded,
					YieldCount:          2,
					ExpectedHostResults: 1,
					SuspendedNanos:      0,
				},
				{
					Event:               experimental.YieldEventCancelled,
					YieldCount:          2,
					ExpectedHostResults: 1,
					SuspendedNanos:      1_000_000,
				},
			}

			require.Equal(t, wantInitial, snapshotYieldObservations(initialLeft.snapshot()))
			require.Equal(t, wantInitial, snapshotYieldObservations(initialRight.snapshot()))
			require.Equal(t, wantResumed, snapshotYieldObservations(resumeLeft.snapshot()))
			require.Equal(t, wantResumed, snapshotYieldObservations(resumeRight.snapshot()))
		})
	}
}

func TestMultiFuelObserver_YieldResumeUsesResumedObserver(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			ctx := context.Background()
			host := &fuelAdjustingYieldingHostFunc{t: t, adjustment: 5}
			mod, rt, _ := setupYieldTestWithHost(t, ec.cfg, host)
			defer rt.Close(ctx)

			initialLeft := &recordingFuelObserver{}
			initialRight := &recordingFuelObserver{}
			ctrl := experimental.NewSimpleFuelController(20)
			_, err := mod.ExportedFunction("run_twice").Call(
				experimental.WithFuelObserver(
					experimental.WithFuelController(
						experimental.WithYielder(ctx),
						ctrl,
					),
					experimental.MultiFuelObserver(initialLeft, initialRight),
				),
			)
			firstResumer := requireYieldError(t, err).Resumer()

			resumeLeft := &recordingFuelObserver{}
			resumeRight := &recordingFuelObserver{}
			resumeMulti := &forwardingFuelObserver{inner: experimental.MultiFuelObserver(resumeLeft, resumeRight)}
			host.expectedFuelObserver = resumeMulti

			results, err := firstResumer.Resume(
				experimental.WithFuelObserver(experimental.WithYielder(ctx), resumeMulti),
				[]uint64{40},
			)
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)
			require.Equal(t, host.beforeAdjustment+int64(5), host.afterAdjustment)

			wantInitial := []fuelObservationSnapshot{{
				Event:     experimental.FuelEventBudgeted,
				Budget:    20,
				Remaining: 20,
			}}
			wantResumed := []fuelObservationSnapshot{
				{
					Event:     experimental.FuelEventRecharged,
					Remaining: host.afterAdjustment,
					Delta:     5,
				},
				{
					Event:     experimental.FuelEventConsumed,
					Budget:    25,
					Consumed:  ctrl.TotalConsumed(),
					Remaining: 25 - ctrl.TotalConsumed(),
				},
			}

			require.Equal(t, wantInitial, snapshotFuelObservations(initialLeft.snapshot()))
			require.Equal(t, wantInitial, snapshotFuelObservations(initialRight.snapshot()))
			require.Equal(t, wantResumed, snapshotFuelObservations(resumeLeft.snapshot()))
			require.Equal(t, wantResumed, snapshotFuelObservations(resumeRight.snapshot()))
		})
	}
}

func TestMultiHostCallPolicyObserver_PolicyDeniedAlsoNotifiesMultiTrapObserver(t *testing.T) {
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

			mod, err := rt.InstantiateWithConfig(ctx, trapObserverTestModuleBinary(), wazero.NewModuleConfig().WithName("guest"))
			require.NoError(t, err)

			policyLeft := &recordingHostCallPolicyObserver{}
			policyRight := &recordingHostCallPolicyObserver{}
			trapLeft := &recordingTrapObserver{}
			trapRight := &recordingTrapObserver{}
			callCtx := experimental.WithTrapObserver(
				experimental.WithHostCallPolicyObserver(
					experimental.WithHostCallPolicy(ctx, experimental.HostCallPolicyFunc(
						func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, "guest", caller.Name())
							require.Equal(t, "env.check", hostFunction.DebugName())
							return false
						},
					)),
					experimental.MultiHostCallPolicyObserver(policyLeft, policyRight),
				),
				experimental.MultiTrapObserver(trapLeft, trapRight),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)
			require.False(t, hostCalled)

			for _, observer := range []*recordingHostCallPolicyObserver{policyLeft, policyRight} {
				observations := observer.snapshot()
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.HostCallPolicyEventDenied, observations[0].Event)
				require.Equal(t, "guest", observations[0].Module.Name())
				require.Equal(t, "env.check", observations[0].HostFunction.DebugName())
			}

			for _, observer := range []*recordingTrapObserver{trapLeft, trapRight} {
				observation := observer.single(t)
				require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
				require.True(t, errors.Is(observation.Err, wasmruntime.ErrRuntimePolicyDenied))
				require.Equal(t, "guest", observation.Module.Name())
			}
		})
	}
}

func TestMultiYieldPolicyObserver_ResumeUsesResumedObserver(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			mod, rt, ctx := setupYieldTest(t, ec.cfg)
			defer rt.Close(ctx)

			initialLeft := &recordingYieldPolicyObserver{}
			initialRight := &recordingYieldPolicyObserver{}
			_, err := mod.ExportedFunction("run_twice").Call(
				experimental.WithYieldPolicyObserver(
					experimental.WithYieldPolicy(
						experimental.WithYielder(ctx),
						experimental.YieldPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
							return true
						}),
					),
					experimental.MultiYieldPolicyObserver(initialLeft, initialRight),
				),
			)
			firstResumer := requireYieldError(t, err).Resumer()

			resumeLeft := &recordingYieldPolicyObserver{}
			resumeRight := &recordingYieldPolicyObserver{}
			trapLeft := &recordingTrapObserver{}
			trapRight := &recordingTrapObserver{}
			resumeCtx := experimental.WithTrapObserver(
				experimental.WithYieldPolicyObserver(
					experimental.WithYieldPolicy(
						experimental.WithYielder(ctx),
						experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.NotNil(t, caller)
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return false
						}),
					),
					experimental.MultiYieldPolicyObserver(resumeLeft, resumeRight),
				),
				experimental.MultiTrapObserver(trapLeft, trapRight),
			)

			_, err = firstResumer.Resume(resumeCtx, []uint64{40})
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)

			for _, observer := range []*recordingYieldPolicyObserver{initialLeft, initialRight} {
				observations := observer.snapshot()
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.YieldPolicyEventAllowed, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
			}

			for _, observer := range []*recordingYieldPolicyObserver{resumeLeft, resumeRight} {
				observations := observer.snapshot()
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.YieldPolicyEventDenied, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
			}

			for _, observer := range []*recordingTrapObserver{trapLeft, trapRight} {
				observation := observer.single(t)
				require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
				require.True(t, errors.Is(observation.Err, wasmruntime.ErrRuntimePolicyDenied))
				require.Equal(t, mod.Name(), observation.Module.Name())
			}
		})
	}
}
