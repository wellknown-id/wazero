package experimental_test

import (
	"context"
	"errors"
	"os"
	"runtime"
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

type importResolverObservationSnapshot struct {
	Event              experimental.ImportResolverEvent
	ModuleName         string
	ImportModule       string
	ResolvedModuleName string
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

func snapshotImportResolverObservations(observations []experimental.ImportResolverObservation) []importResolverObservationSnapshot {
	snapshots := make([]importResolverObservationSnapshot, len(observations))
	for i, observation := range observations {
		snapshots[i] = importResolverObservationSnapshot{
			Event:        observation.Event,
			ModuleName:   observation.Module.Name(),
			ImportModule: observation.ImportModule,
		}
		if observation.ResolvedModule != nil {
			snapshots[i].ResolvedModuleName = observation.ResolvedModule.Name()
		}
	}
	return snapshots
}

func runImportResolverInstantiationScenario(
	t *testing.T,
	registerStoreEnv bool,
	configureContext func(context.Context, api.Module) context.Context,
	observer experimental.ImportResolverObserver,
) (err error, storeCalls, resolvedCalls int) {
	t.Helper()

	ctx := context.Background()
	r := wazero.NewRuntime(ctx)
	defer r.Close(ctx)

	if registerStoreEnv {
		_, err = instantiateStartModule(ctx, r, "env", func(context.Context) { storeCalls++ })
		require.NoError(t, err)
	}

	resolved, err := instantiateStartModule(ctx, r, "resolved-env", func(context.Context) { resolvedCalls++ })
	require.NoError(t, err)

	modMain, err := r.CompileModule(ctx, testImportResolverModule())
	require.NoError(t, err)

	callCtx := configureContext(ctx, resolved)
	if observer != nil {
		callCtx = experimental.WithImportResolverObserver(callCtx, observer)
	}

	_, err = r.InstantiateModule(callCtx, modMain, wazero.NewModuleConfig().WithName("guest"))
	return err, storeCalls, resolvedCalls
}

func requireEquivalentImportResolverOutcome(
	t *testing.T,
	baselineErr, observedErr error,
	baselineStoreCalls, observedStoreCalls int,
	baselineResolvedCalls, observedResolvedCalls int,
) {
	t.Helper()

	require.Equal(t, baselineStoreCalls, observedStoreCalls)
	require.Equal(t, baselineResolvedCalls, observedResolvedCalls)
	if baselineErr == nil || observedErr == nil {
		require.Equal(t, baselineErr == nil, observedErr == nil)
		return
	}
	require.Equal(t, baselineErr.Error(), observedErr.Error())
}

func requireEquivalentTrapObservation(t *testing.T, want trapObservationSnapshot, wantErr error, observers ...*recordingTrapObserver) {
	t.Helper()

	wantSnapshots := []trapObservationSnapshot{want}
	for _, observer := range observers {
		require.Equal(t, wantSnapshots, snapshotTrapObservations(observer))
		observation := observer.single(t)
		require.True(t, errors.Is(observation.Err, wantErr))
	}
}

func TestMultiImportResolverObserver_EquivalentSequences(t *testing.T) {
	tests := []struct {
		name             string
		registerStoreEnv bool
		configureContext func(context.Context, api.Module) context.Context
		wantSnapshots    []importResolverObservationSnapshot
	}{
		{
			name:             "acl allows store fallback",
			registerStoreEnv: true,
			configureContext: func(ctx context.Context, _ api.Module) context.Context {
				return experimental.WithImportResolverACL(ctx, experimental.NewImportACL().AllowModules("env"))
			},
			wantSnapshots: []importResolverObservationSnapshot{
				{Event: experimental.ImportResolverEventACLAllowed, ModuleName: "guest", ImportModule: "env"},
				{Event: experimental.ImportResolverEventStoreFallback, ModuleName: "guest", ImportModule: "env", ResolvedModuleName: "env"},
			},
		},
		{
			name:             "acl deny",
			registerStoreEnv: true,
			configureContext: func(ctx context.Context, _ api.Module) context.Context {
				return experimental.WithImportResolverConfig(ctx, experimental.ImportResolverConfig{
					ACL: experimental.NewImportACL().DenyModules("env"),
					Resolver: func(string) api.Module {
						t.Fatal("resolver should not run after ACL denial")
						return nil
					},
				})
			},
			wantSnapshots: []importResolverObservationSnapshot{
				{Event: experimental.ImportResolverEventACLDenied, ModuleName: "guest", ImportModule: "env"},
			},
		},
		{
			name:             "fail closed denial",
			registerStoreEnv: true,
			configureContext: func(ctx context.Context, _ api.Module) context.Context {
				return experimental.WithImportResolverConfig(ctx, experimental.ImportResolverConfig{
					ACL:        experimental.NewImportACL().AllowModules("env"),
					FailClosed: true,
				})
			},
			wantSnapshots: []importResolverObservationSnapshot{
				{Event: experimental.ImportResolverEventACLAllowed, ModuleName: "guest", ImportModule: "env"},
				{Event: experimental.ImportResolverEventFailClosedDenied, ModuleName: "guest", ImportModule: "env"},
			},
		},
		{
			name:             "resolver hit takes precedence over store",
			registerStoreEnv: true,
			configureContext: func(ctx context.Context, resolved api.Module) context.Context {
				return experimental.WithImportResolverConfig(ctx, experimental.ImportResolverConfig{
					ACL: experimental.NewImportACL().AllowModules("env"),
					Resolver: func(name string) api.Module {
						if name == "env" {
							return resolved
						}
						return nil
					},
				})
			},
			wantSnapshots: []importResolverObservationSnapshot{
				{Event: experimental.ImportResolverEventACLAllowed, ModuleName: "guest", ImportModule: "env"},
				{Event: experimental.ImportResolverEventResolverResolved, ModuleName: "guest", ImportModule: "env", ResolvedModuleName: "resolved-env"},
			},
		},
	}

	for _, tc := range tests {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			baselineErr, baselineStoreCalls, baselineResolvedCalls := runImportResolverInstantiationScenario(
				t,
				tc.registerStoreEnv,
				tc.configureContext,
				nil,
			)

			left := &recordingImportResolverObserver{}
			right := &recordingImportResolverObserver{}
			multiErr, multiStoreCalls, multiResolvedCalls := runImportResolverInstantiationScenario(
				t,
				tc.registerStoreEnv,
				tc.configureContext,
				experimental.MultiImportResolverObserver(left, right),
			)

			requireEquivalentImportResolverOutcome(
				t,
				baselineErr,
				multiErr,
				baselineStoreCalls,
				multiStoreCalls,
				baselineResolvedCalls,
				multiResolvedCalls,
			)
			require.Equal(t, tc.wantSnapshots, snapshotImportResolverObservations(left.snapshot()))
			require.Equal(t, tc.wantSnapshots, snapshotImportResolverObservations(right.snapshot()))
		})
	}
}

func TestMultiYieldObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			provider := newObservingTimeProvider()
			mod, rt, ctx := setupYieldTest(t, ec.cfg.WithTimeProvider(provider))
			defer rt.Close(ctx)

			left := &recordingYieldObserver{}
			right := &recordingYieldObserver{}
			_, err := mod.ExportedFunction("run").Call(
				experimental.WithYieldObserver(
					experimental.WithYielder(ctx),
					experimental.MultiYieldObserver(left, right),
				),
			)
			resumer := requireYieldError(t, err).Resumer()

			results, err := resumer.Resume(experimental.WithYielder(ctx), []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			want := []yieldObservationSnapshot{
				{
					Event:               experimental.YieldEventYielded,
					YieldCount:          1,
					ExpectedHostResults: 1,
					SuspendedNanos:      0,
				},
				{
					Event:               experimental.YieldEventResumed,
					YieldCount:          1,
					ExpectedHostResults: 1,
					SuspendedNanos:      1_000_000,
				},
			}
			require.Equal(t, want, snapshotYieldObservations(left.snapshot()))
			require.Equal(t, want, snapshotYieldObservations(right.snapshot()))
		})
	}
}

func TestMultiYieldObserver_YieldResumeOrdering(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			provider := newObservingTimeProvider()
			mod, rt, ctx := setupYieldTest(t, ec.cfg.WithTimeProvider(provider))
			defer rt.Close(ctx)

			left := &recordingYieldObserver{}
			right := &recordingYieldObserver{}
			var order []string
			callCtx := experimental.WithYieldObserver(
				experimental.WithYielder(ctx),
				experimental.MultiYieldObserver(
					experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
						left.ObserveYield(ctx, observation)
						order = append(order, "left "+string(observation.Event))
					}),
					experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
						right.ObserveYield(ctx, observation)
						order = append(order, "right "+string(observation.Event))
					}),
				),
			)

			_, err := mod.ExportedFunction("run").Call(callCtx)
			resumer := requireYieldError(t, err).Resumer()

			results, err := resumer.Resume(callCtx, []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			want := []yieldObservationSnapshot{
				{
					Event:               experimental.YieldEventYielded,
					YieldCount:          1,
					ExpectedHostResults: 1,
					SuspendedNanos:      0,
				},
				{
					Event:               experimental.YieldEventResumed,
					YieldCount:          1,
					ExpectedHostResults: 1,
					SuspendedNanos:      1_000_000,
				},
			}
			require.Equal(t, want, snapshotYieldObservations(left.snapshot()))
			require.Equal(t, want, snapshotYieldObservations(right.snapshot()))
			require.Equal(t, []string{
				"left yielded",
				"right yielded",
				"left resumed",
				"right resumed",
			}, order)
		})
	}
}

func TestMultiYieldObserverWithMultiYieldPolicyObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			provider := newObservingTimeProvider()
			mod, rt, ctx := setupYieldTest(t, ec.cfg.WithTimeProvider(provider))
			defer rt.Close(ctx)

			yieldLeft := &recordingYieldObserver{}
			yieldRight := &recordingYieldObserver{}
			policyLeft := &recordingYieldPolicyObserver{}
			policyRight := &recordingYieldPolicyObserver{}
			callCtx := experimental.WithYieldObserver(
				experimental.WithYieldPolicyObserver(
					experimental.WithYieldPolicy(
						experimental.WithYielder(ctx),
						experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return true
						}),
					),
					experimental.MultiYieldPolicyObserver(policyLeft, policyRight),
				),
				experimental.MultiYieldObserver(yieldLeft, yieldRight),
			)

			_, err := mod.ExportedFunction("run").Call(callCtx)
			resumer := requireYieldError(t, err).Resumer()

			results, err := resumer.Resume(callCtx, []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			wantYield := []yieldObservationSnapshot{
				{
					Event:               experimental.YieldEventYielded,
					YieldCount:          1,
					ExpectedHostResults: 1,
					SuspendedNanos:      0,
				},
				{
					Event:               experimental.YieldEventResumed,
					YieldCount:          1,
					ExpectedHostResults: 1,
					SuspendedNanos:      1_000_000,
				},
			}
			require.Equal(t, wantYield, snapshotYieldObservations(yieldLeft.snapshot()))
			require.Equal(t, wantYield, snapshotYieldObservations(yieldRight.snapshot()))

			for _, observations := range [][]experimental.YieldPolicyObservation{policyLeft.snapshot(), policyRight.snapshot()} {
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.YieldPolicyEventAllowed, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
			}
		})
	}
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

func TestMultiFuelObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			ctx := context.Background()
			mod, rt, _ := setupYieldTest(t, ec.cfg)
			defer rt.Close(ctx)

			left := &recordingFuelObserver{}
			right := &recordingFuelObserver{}
			ctrl := experimental.NewSimpleFuelController(20)
			callCtx := experimental.WithFuelObserver(
				experimental.WithFuelController(
					experimental.WithYielder(ctx),
					ctrl,
				),
				experimental.MultiFuelObserver(left, right),
			)

			_, err := mod.ExportedFunction("run").Call(callCtx)
			resumer := requireYieldError(t, err).Resumer()

			results, err := resumer.Resume(experimental.WithYielder(ctx), []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			want := []fuelObservationSnapshot{
				{
					Event:     experimental.FuelEventBudgeted,
					Budget:    20,
					Remaining: 20,
				},
				{
					Event:     experimental.FuelEventConsumed,
					Budget:    20,
					Consumed:  ctrl.TotalConsumed(),
					Remaining: 20 - ctrl.TotalConsumed(),
				},
			}
			require.Equal(t, want, snapshotFuelObservations(left.snapshot()))
			require.Equal(t, want, snapshotFuelObservations(right.snapshot()))
		})
	}
}

func TestMultiFuelObserver_YieldResumeOrdering(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			ctx := context.Background()
			mod, rt, _ := setupYieldTest(t, ec.cfg)
			defer rt.Close(ctx)

			left := &recordingFuelObserver{}
			right := &recordingFuelObserver{}
			var order []string
			ctrl := experimental.NewSimpleFuelController(20)
			callCtx := experimental.WithFuelObserver(
				experimental.WithFuelController(
					experimental.WithYielder(ctx),
					ctrl,
				),
				experimental.MultiFuelObserver(
					experimental.FuelObserverFunc(func(ctx context.Context, observation experimental.FuelObservation) {
						left.ObserveFuel(ctx, observation)
						order = append(order, "left "+string(observation.Event))
					}),
					experimental.FuelObserverFunc(func(ctx context.Context, observation experimental.FuelObservation) {
						right.ObserveFuel(ctx, observation)
						order = append(order, "right "+string(observation.Event))
					}),
				),
			)

			_, err := mod.ExportedFunction("run").Call(callCtx)
			resumer := requireYieldError(t, err).Resumer()

			results, err := resumer.Resume(experimental.WithYielder(ctx), []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			want := []fuelObservationSnapshot{
				{
					Event:     experimental.FuelEventBudgeted,
					Budget:    20,
					Remaining: 20,
				},
				{
					Event:     experimental.FuelEventConsumed,
					Budget:    20,
					Consumed:  ctrl.TotalConsumed(),
					Remaining: 20 - ctrl.TotalConsumed(),
				},
			}
			require.Equal(t, want, snapshotFuelObservations(left.snapshot()))
			require.Equal(t, want, snapshotFuelObservations(right.snapshot()))
			require.Equal(t, []string{
				"left budgeted",
				"right budgeted",
				"left consumed",
				"right consumed",
			}, order)
		})
	}
}

func TestMultiFuelObserver_FuelExhausted(t *testing.T) {
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

			left := &recordingFuelObserver{}
			right := &recordingFuelObserver{}
			_, err = mod.ExportedFunction("run").Call(
				experimental.WithFuelObserver(ctx, experimental.MultiFuelObserver(left, right)),
			)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimeFuelExhausted)

			want := []fuelObservationSnapshot{
				{
					Event:     experimental.FuelEventBudgeted,
					Budget:    3,
					Remaining: 3,
				},
				{
					Event:     experimental.FuelEventExhausted,
					Budget:    3,
					Consumed:  snapshotFuelObservations(left.snapshot())[1].Consumed,
					Remaining: snapshotFuelObservations(left.snapshot())[1].Remaining,
				},
			}
			require.Equal(t, want, snapshotFuelObservations(left.snapshot()))
			require.Equal(t, want, snapshotFuelObservations(right.snapshot()))
		})
	}
}

func TestMultiTrapObserver_YieldResumeUsesResumedObserver(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			mod, rt, ctx := setupYieldTest(t, ec.cfg)
			defer rt.Close(ctx)

			resumePolicy := experimental.HostCallPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
				return false
			})

			baselineInitial := &recordingTrapObserver{}
			_, err := mod.ExportedFunction("run_twice").Call(
				experimental.WithTrapObserver(experimental.WithYielder(ctx), baselineInitial),
			)
			baselineResumer := requireYieldError(t, err).Resumer()

			baselineResume := &recordingTrapObserver{}
			baselineResumeCtx := experimental.WithTrapObserver(
				experimental.WithHostCallPolicy(experimental.WithYielder(ctx), resumePolicy),
				baselineResume,
			)
			_, baselineErr := baselineResumer.Resume(baselineResumeCtx, []uint64{1})
			require.ErrorIs(t, baselineErr, wasmruntime.ErrRuntimePolicyDenied)
			require.Zero(t, baselineInitial.count())

			wantSnapshots := []trapObservationSnapshot{{
				Cause:        experimental.TrapCausePolicyDenied,
				ModuleName:   mod.Name(),
				PolicyDenied: true,
			}}
			require.Equal(t, wantSnapshots, snapshotTrapObservations(baselineResume))

			initialLeft := &recordingTrapObserver{}
			initialRight := &recordingTrapObserver{}
			_, err = mod.ExportedFunction("run_twice").Call(
				experimental.WithTrapObserver(
					experimental.WithYielder(ctx),
					experimental.MultiTrapObserver(initialLeft, initialRight),
				),
			)
			resumer := requireYieldError(t, err).Resumer()

			resumeLeft := &recordingTrapObserver{}
			resumeRight := &recordingTrapObserver{}
			resumeCtx := experimental.WithTrapObserver(
				experimental.WithHostCallPolicy(experimental.WithYielder(ctx), resumePolicy),
				experimental.MultiTrapObserver(resumeLeft, resumeRight),
			)

			_, err = resumer.Resume(resumeCtx, []uint64{1})
			require.EqualError(t, err, baselineErr.Error())
			require.Zero(t, initialLeft.count())
			require.Zero(t, initialRight.count())

			for _, observer := range []*recordingTrapObserver{resumeLeft, resumeRight} {
				require.Equal(t, wantSnapshots, snapshotTrapObservations(observer))
				observation := observer.single(t)
				require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
				require.EqualError(t, observation.Err, baselineErr.Error())
				require.Equal(t, mod.Name(), observation.Module.Name())
			}
		})
	}
}

func TestMultiHostCallPolicyObserver_PolicyDeniedOrdering(t *testing.T) {
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

			left := &recordingHostCallPolicyObserver{}
			right := &recordingHostCallPolicyObserver{}
			var order []string
			callCtx := experimental.WithHostCallPolicyObserver(
				experimental.WithHostCallPolicy(ctx, experimental.HostCallPolicyFunc(
					func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
						require.Equal(t, "guest", caller.Name())
						require.Equal(t, "env.check", hostFunction.DebugName())
						return false
					},
				)),
				experimental.MultiHostCallPolicyObserver(
					experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
						left.ObserveHostCallPolicy(ctx, observation)
						order = append(order, "left "+string(observation.Event))
					}),
					experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
						right.ObserveHostCallPolicy(ctx, observation)
						order = append(order, "right "+string(observation.Event))
					}),
				),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)
			require.False(t, hostCalled)
			require.Equal(t, []string{"left denied", "right denied"}, order)

			for _, observer := range []*recordingHostCallPolicyObserver{left, right} {
				observations := observer.snapshot()
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.HostCallPolicyEventDenied, observations[0].Event)
				require.Equal(t, "guest", observations[0].Module.Name())
				require.Equal(t, "env.check", observations[0].HostFunction.DebugName())
			}
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

			requireEquivalentTrapObservation(t, trapObservationSnapshot{
				Cause:        experimental.TrapCausePolicyDenied,
				ModuleName:   "guest",
				PolicyDenied: true,
			}, wasmruntime.ErrRuntimePolicyDenied, trapLeft, trapRight)
		})
	}
}

func TestMultiHostCallPolicyObserver_ResumeUsesResumedObserver(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			mod, rt, ctx := setupYieldTest(t, ec.cfg)
			defer rt.Close(ctx)

			initialLeft := &recordingHostCallPolicyObserver{}
			initialRight := &recordingHostCallPolicyObserver{}
			_, err := mod.ExportedFunction("run_twice").Call(
				experimental.WithHostCallPolicyObserver(
					experimental.WithHostCallPolicy(
						experimental.WithYielder(ctx),
						experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return true
						}),
					),
					experimental.MultiHostCallPolicyObserver(initialLeft, initialRight),
				),
			)
			firstResumer := requireYieldError(t, err).Resumer()

			resumeLeft := &recordingHostCallPolicyObserver{}
			resumeRight := &recordingHostCallPolicyObserver{}
			trapLeft := &recordingTrapObserver{}
			trapRight := &recordingTrapObserver{}
			resumeCtx := experimental.WithTrapObserver(
				experimental.WithHostCallPolicyObserver(
					experimental.WithHostCallPolicy(
						experimental.WithYielder(ctx),
						experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return false
						}),
					),
					experimental.MultiHostCallPolicyObserver(resumeLeft, resumeRight),
				),
				experimental.MultiTrapObserver(trapLeft, trapRight),
			)

			_, err = firstResumer.Resume(resumeCtx, []uint64{40})
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)

			for _, observer := range []*recordingHostCallPolicyObserver{initialLeft, initialRight} {
				observations := observer.snapshot()
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.HostCallPolicyEventAllowed, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
			}

			for _, observer := range []*recordingHostCallPolicyObserver{resumeLeft, resumeRight} {
				observations := observer.snapshot()
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.HostCallPolicyEventDenied, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
			}

			requireEquivalentTrapObservation(t, trapObservationSnapshot{
				Cause:        experimental.TrapCausePolicyDenied,
				ModuleName:   mod.Name(),
				PolicyDenied: true,
			}, wasmruntime.ErrRuntimePolicyDenied, trapLeft, trapRight)
		})
	}
}

func TestMultiHostCallPolicyObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			mod, rt, ctx := setupYieldTest(t, ec.cfg)
			defer rt.Close(ctx)

			left := &recordingHostCallPolicyObserver{}
			right := &recordingHostCallPolicyObserver{}
			callCtx := experimental.WithHostCallPolicyObserver(
				experimental.WithHostCallPolicy(
					experimental.WithYielder(ctx),
					experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
						require.Equal(t, mod.Name(), caller.Name())
						require.Equal(t, "example.async_work", hostFunction.DebugName())
						return true
					}),
				),
				experimental.MultiHostCallPolicyObserver(left, right),
			)

			_, err := mod.ExportedFunction("run_twice").Call(callCtx)
			firstResumer := requireYieldError(t, err).Resumer()

			_, err = firstResumer.Resume(callCtx, []uint64{40})
			secondResumer := requireYieldError(t, err).Resumer()

			results, err := secondResumer.Resume(callCtx, []uint64{2})
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)

			for _, observations := range [][]experimental.HostCallPolicyObservation{left.snapshot(), right.snapshot()} {
				require.Equal(t, 2, len(observations))
				require.Equal(t, experimental.HostCallPolicyEventAllowed, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
				require.Equal(t, experimental.HostCallPolicyEventAllowed, observations[1].Event)
				require.Equal(t, mod.Name(), observations[1].Module.Name())
				require.Equal(t, "example.async_work", observations[1].HostFunction.DebugName())
			}
		})
	}
}

func TestMultiHostCallPolicy_ComposesAllowAndDenyFlows(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			hostCalls := 0
			_, err := rt.NewHostModuleBuilder("env").
				NewFunctionBuilder().
				WithFunc(func() { hostCalls++ }).
				Export("check").
				Instantiate(ctx)
			require.NoError(t, err)

			mod, err := rt.InstantiateWithConfig(ctx, trapObserverTestModuleBinary(), wazero.NewModuleConfig().WithName("guest"))
			require.NoError(t, err)

			_, err = mod.ExportedFunction("run").Call(
				experimental.WithHostCallPolicy(
					ctx,
					experimental.MultiHostCallPolicy(
						experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, "guest", caller.Name())
							require.Equal(t, "env.check", hostFunction.DebugName())
							return true
						}),
						experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, "guest", caller.Name())
							require.Equal(t, "env.check", hostFunction.DebugName())
							return true
						}),
					),
				),
			)
			require.NoError(t, err)
			require.Equal(t, 1, hostCalls)

			trapObserver := &recordingTrapObserver{}
			_, err = mod.ExportedFunction("run").Call(
				experimental.WithTrapObserver(
					experimental.WithHostCallPolicy(
						ctx,
						experimental.MultiHostCallPolicy(
							experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
								require.Equal(t, "guest", caller.Name())
								require.Equal(t, "env.check", hostFunction.DebugName())
								return true
							}),
							experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
								require.Equal(t, "guest", caller.Name())
								require.Equal(t, "env.check", hostFunction.DebugName())
								return false
							}),
						),
					),
					trapObserver,
				),
			)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)
			require.Equal(t, 1, hostCalls)

			observation := trapObserver.single(t)
			require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
			require.True(t, errors.Is(observation.Err, wasmruntime.ErrRuntimePolicyDenied))
			require.Equal(t, "guest", observation.Module.Name())
		})
	}
}

func TestMultiTrapObserver_FuelExhausted(t *testing.T) {
	if !platform.CompilerSupported() {
		t.Skip("compiler is not supported on this host")
	}

	ctx := context.Background()
	rt := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfigCompiler().WithFuel(1))
	defer rt.Close(ctx)

	mod, err := rt.InstantiateWithConfig(ctx, trapObserverFuelLoopBinary(), wazero.NewModuleConfig().WithName("fuel-guest"))
	require.NoError(t, err)

	left := &recordingTrapObserver{}
	right := &recordingTrapObserver{}
	_, err = mod.ExportedFunction("run").Call(
		experimental.WithTrapObserver(ctx, experimental.MultiTrapObserver(left, right)),
	)
	require.ErrorIs(t, err, wasmruntime.ErrRuntimeFuelExhausted)

	requireEquivalentTrapObservation(t, trapObservationSnapshot{
		Cause:      experimental.TrapCauseFuelExhausted,
		ModuleName: "fuel-guest",
	}, wasmruntime.ErrRuntimeFuelExhausted, left, right)
}

func TestMultiTrapObserver_MemoryFault(t *testing.T) {
	if !platform.CompilerSupported() || runtime.GOOS != "linux" || (runtime.GOARCH != "amd64" && runtime.GOARCH != "arm64") {
		t.Skip("memory fault trap path is only expected on supported compiler targets")
	}

	ctx := context.Background()
	bin, err := os.ReadFile("../testdata/oob_load.wasm")
	require.NoError(t, err)

	rt := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfigCompiler().WithSecureMode(true))
	defer rt.Close(ctx)

	mod, err := rt.InstantiateWithConfig(ctx, bin, wazero.NewModuleConfig().WithName("secure-guest"))
	require.NoError(t, err)

	left := &recordingTrapObserver{}
	right := &recordingTrapObserver{}
	_, err = mod.ExportedFunction("oob").Call(
		experimental.WithTrapObserver(ctx, experimental.MultiTrapObserver(left, right)),
	)
	require.ErrorIs(t, err, wasmruntime.ErrRuntimeMemoryFault)

	requireEquivalentTrapObservation(t, trapObservationSnapshot{
		Cause:      experimental.TrapCauseMemoryFault,
		ModuleName: "secure-guest",
	}, wasmruntime.ErrRuntimeMemoryFault, left, right)
}

func TestMultiTrapObserver_ArithmeticTraps(t *testing.T) {
	testCases := []struct {
		name         string
		cfg          func() wazero.RuntimeConfig
		supported    func() bool
		moduleBinary func() []byte
		wantErr      error
		wantCause    experimental.TrapCause
	}{
		{
			name:         "interpreter/unreachable",
			cfg:          wazero.NewRuntimeConfigInterpreter,
			supported:    func() bool { return true },
			moduleBinary: trapRuntimeUnreachableBinary,
			wantErr:      wasmruntime.ErrRuntimeUnreachable,
			wantCause:    experimental.TrapCauseUnreachable,
		},
		{
			name:         "compiler/unreachable",
			cfg:          wazero.NewRuntimeConfigCompiler,
			supported:    platform.CompilerSupported,
			moduleBinary: trapRuntimeUnreachableBinary,
			wantErr:      wasmruntime.ErrRuntimeUnreachable,
			wantCause:    experimental.TrapCauseUnreachable,
		},
		{
			name:         "interpreter/integer-divide-by-zero",
			cfg:          wazero.NewRuntimeConfigInterpreter,
			supported:    func() bool { return true },
			moduleBinary: trapRuntimeIntegerDivideByZeroBinary,
			wantErr:      wasmruntime.ErrRuntimeIntegerDivideByZero,
			wantCause:    experimental.TrapCauseIntegerDivideByZero,
		},
		{
			name:         "compiler/integer-divide-by-zero",
			cfg:          wazero.NewRuntimeConfigCompiler,
			supported:    platform.CompilerSupported,
			moduleBinary: trapRuntimeIntegerDivideByZeroBinary,
			wantErr:      wasmruntime.ErrRuntimeIntegerDivideByZero,
			wantCause:    experimental.TrapCauseIntegerDivideByZero,
		},
		{
			name:         "interpreter/integer-overflow",
			cfg:          wazero.NewRuntimeConfigInterpreter,
			supported:    func() bool { return true },
			moduleBinary: trapRuntimeIntegerOverflowBinary,
			wantErr:      wasmruntime.ErrRuntimeIntegerOverflow,
			wantCause:    experimental.TrapCauseIntegerOverflow,
		},
		{
			name:         "compiler/integer-overflow",
			cfg:          wazero.NewRuntimeConfigCompiler,
			supported:    platform.CompilerSupported,
			moduleBinary: trapRuntimeIntegerOverflowBinary,
			wantErr:      wasmruntime.ErrRuntimeIntegerOverflow,
			wantCause:    experimental.TrapCauseIntegerOverflow,
		},
		{
			name:         "interpreter/invalid-conversion-to-integer",
			cfg:          wazero.NewRuntimeConfigInterpreter,
			supported:    func() bool { return true },
			moduleBinary: trapRuntimeInvalidConversionToIntegerBinary,
			wantErr:      wasmruntime.ErrRuntimeInvalidConversionToInteger,
			wantCause:    experimental.TrapCauseInvalidConversionToInteger,
		},
		{
			name:         "compiler/invalid-conversion-to-integer",
			cfg:          wazero.NewRuntimeConfigCompiler,
			supported:    platform.CompilerSupported,
			moduleBinary: trapRuntimeInvalidConversionToIntegerBinary,
			wantErr:      wasmruntime.ErrRuntimeInvalidConversionToInteger,
			wantCause:    experimental.TrapCauseInvalidConversionToInteger,
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			if !tc.supported() {
				t.Skip("engine is not supported on this host")
			}

			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, tc.cfg())
			defer rt.Close(ctx)

			mod, err := rt.InstantiateWithConfig(ctx, tc.moduleBinary(), wazero.NewModuleConfig().WithName("guest"))
			require.NoError(t, err)

			left := &recordingTrapObserver{}
			right := &recordingTrapObserver{}
			_, err = mod.ExportedFunction("run").Call(
				experimental.WithTrapObserver(ctx, experimental.MultiTrapObserver(left, right)),
			)
			require.ErrorIs(t, err, tc.wantErr)

			requireEquivalentTrapObservation(t, trapObservationSnapshot{
				Cause:      tc.wantCause,
				ModuleName: "guest",
			}, tc.wantErr, left, right)
		})
	}
}

func TestMultiTrapObserver_ResumeArithmeticTraps(t *testing.T) {
	testCases := []struct {
		name      string
		wantErr   error
		wantCause experimental.TrapCause
	}{
		{
			name:      "integer-divide-by-zero",
			wantErr:   wasmruntime.ErrRuntimeIntegerDivideByZero,
			wantCause: experimental.TrapCauseIntegerDivideByZero,
		},
		{
			name:      "integer-overflow",
			wantErr:   wasmruntime.ErrRuntimeIntegerOverflow,
			wantCause: experimental.TrapCauseIntegerOverflow,
		},
		{
			name:      "invalid-conversion-to-integer",
			wantErr:   wasmruntime.ErrRuntimeInvalidConversionToInteger,
			wantCause: experimental.TrapCauseInvalidConversionToInteger,
		},
	}

	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			for _, tc := range testCases {
				t.Run(tc.name, func(t *testing.T) {
					ctx := context.Background()
					rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
					defer rt.Close(ctx)

					_, err := rt.NewHostModuleBuilder("example").
						NewFunctionBuilder().
						WithGoModuleFunction(&yieldingHostFunc{t: t}, nil, []api.ValueType{api.ValueTypeI32}).
						Export("async_work").
						Instantiate(ctx)
					require.NoError(t, err)

					mod, err := rt.InstantiateWithConfig(ctx, resumeArithmeticTrapModuleBinary(tc.wantCause), wazero.NewModuleConfig().WithName("guest"))
					require.NoError(t, err)

					initialLeft := &recordingTrapObserver{}
					initialRight := &recordingTrapObserver{}
					_, err = mod.ExportedFunction("run").Call(
						experimental.WithTrapObserver(
							experimental.WithYielder(ctx),
							experimental.MultiTrapObserver(initialLeft, initialRight),
						),
					)
					yieldErr := requireYieldError(t, err)
					require.Zero(t, initialLeft.count())
					require.Zero(t, initialRight.count())

					resumeLeft := &recordingTrapObserver{}
					resumeRight := &recordingTrapObserver{}
					_, err = yieldErr.Resumer().Resume(
						experimental.WithTrapObserver(
							experimental.WithYielder(ctx),
							experimental.MultiTrapObserver(resumeLeft, resumeRight),
						),
						[]uint64{1},
					)
					require.ErrorIs(t, err, tc.wantErr)

					cause, ok := experimental.TrapCauseOf(err)
					require.True(t, ok)
					require.Equal(t, tc.wantCause, cause)

					requireEquivalentTrapObservation(t, trapObservationSnapshot{
						Cause:      tc.wantCause,
						ModuleName: mod.Name(),
					}, tc.wantErr, resumeLeft, resumeRight)
				})
			}
		})
	}
}

func TestMultiYieldPolicy_ComposesAllowAndDenyFlows(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			mod, rt, ctx := setupYieldTest(t, ec.cfg)
			defer rt.Close(ctx)

			allowConsulted := 0
			_, err := mod.ExportedFunction("run").Call(
				experimental.WithYieldPolicy(
					experimental.WithYielder(ctx),
					experimental.MultiYieldPolicy(
						experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							allowConsulted++
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return true
						}),
						experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							allowConsulted++
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return true
						}),
					),
				),
			)
			resumer := requireYieldError(t, err).Resumer()

			results, err := resumer.Resume(experimental.WithYielder(ctx), []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)
			require.Equal(t, 2, allowConsulted)

			denyConsulted := 0
			trapObserver := &recordingTrapObserver{}
			_, err = mod.ExportedFunction("run").Call(
				experimental.WithTrapObserver(
					experimental.WithYieldPolicy(
						experimental.WithYielder(ctx),
						experimental.MultiYieldPolicy(
							experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
								denyConsulted++
								require.Equal(t, mod.Name(), caller.Name())
								require.Equal(t, "example.async_work", hostFunction.DebugName())
								return true
							}),
							experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
								denyConsulted++
								require.Equal(t, mod.Name(), caller.Name())
								require.Equal(t, "example.async_work", hostFunction.DebugName())
								return false
							}),
						),
					),
					trapObserver,
				),
			)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)
			require.Equal(t, 2, denyConsulted)

			observation := trapObserver.single(t)
			require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
			require.True(t, errors.Is(observation.Err, wasmruntime.ErrRuntimePolicyDenied))
			require.Equal(t, mod.Name(), observation.Module.Name())
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

func TestMultiYieldPolicyObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			mod, rt, ctx := setupYieldTest(t, ec.cfg)
			defer rt.Close(ctx)

			left := &recordingYieldPolicyObserver{}
			right := &recordingYieldPolicyObserver{}
			callCtx := experimental.WithYieldPolicyObserver(
				experimental.WithYieldPolicy(
					experimental.WithYielder(ctx),
					experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
						require.Equal(t, mod.Name(), caller.Name())
						require.Equal(t, "example.async_work", hostFunction.DebugName())
						return true
					}),
				),
				experimental.MultiYieldPolicyObserver(left, right),
			)

			_, err := mod.ExportedFunction("run_twice").Call(callCtx)
			firstResumer := requireYieldError(t, err).Resumer()

			_, err = firstResumer.Resume(callCtx, []uint64{40})
			secondResumer := requireYieldError(t, err).Resumer()

			results, err := secondResumer.Resume(callCtx, []uint64{2})
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)

			for _, observations := range [][]experimental.YieldPolicyObservation{left.snapshot(), right.snapshot()} {
				require.Equal(t, 2, len(observations))
				require.Equal(t, experimental.YieldPolicyEventAllowed, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
				require.Equal(t, experimental.YieldPolicyEventAllowed, observations[1].Event)
				require.Equal(t, mod.Name(), observations[1].Module.Name())
				require.Equal(t, "example.async_work", observations[1].HostFunction.DebugName())
			}
		})
	}
}
