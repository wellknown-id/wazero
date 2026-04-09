package experimental_test

import (
	"context"
	"errors"
	"sync"
	"testing"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/platform"
	"github.com/tetratelabs/wazero/internal/testing/require"
	"github.com/tetratelabs/wazero/internal/wasmruntime"
)

type listenerEvent struct {
	phase    string
	function string
	err      error
}

type recordingFunctionListener struct {
	mu     sync.Mutex
	events []listenerEvent
}

func (r *recordingFunctionListener) Before(_ context.Context, _ api.Module, def api.FunctionDefinition, _ []uint64, _ experimental.StackIterator) {
	r.record(listenerEvent{phase: "before", function: listenerFunctionName(def)})
}

func (r *recordingFunctionListener) After(_ context.Context, _ api.Module, def api.FunctionDefinition, _ []uint64) {
	r.record(listenerEvent{phase: "after", function: listenerFunctionName(def)})
}

func (r *recordingFunctionListener) Abort(_ context.Context, _ api.Module, def api.FunctionDefinition, err error) {
	r.record(listenerEvent{phase: "abort", function: listenerFunctionName(def), err: err})
}

func (r *recordingFunctionListener) record(event listenerEvent) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.events = append(r.events, event)
}

func (r *recordingFunctionListener) snapshot() []listenerEvent {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]listenerEvent(nil), r.events...)
}

func listenerFunctionName(def api.FunctionDefinition) string {
	if exports := def.ExportNames(); len(exports) > 0 {
		return exports[0]
	}
	return def.DebugName()
}

type expectedListenerEvent struct {
	phase       string
	function    string
	errIs       error
	errContains string
}

func newFunctionListenerFactory(listener experimental.FunctionListener) experimental.FunctionListenerFactory {
	return experimental.FunctionListenerFactoryFunc(func(api.FunctionDefinition) experimental.FunctionListener {
		return listener
	})
}

func instantiateListenerHostModule(t *testing.T, ctx context.Context, rt wazero.Runtime, fn func()) {
	t.Helper()
	_, err := rt.NewHostModuleBuilder("env").
		NewFunctionBuilder().
		WithFunc(fn).
		Export("check").
		Instantiate(ctx)
	require.NoError(t, err)
}

func instantiateListenerGuestModule(t *testing.T, ctx context.Context, rt wazero.Runtime, bin []byte) api.Module {
	t.Helper()
	mod, err := rt.InstantiateWithConfig(ctx, bin, wazero.NewModuleConfig().WithName("guest"))
	require.NoError(t, err)
	return mod
}

func assertListenerEvents(t *testing.T, events []listenerEvent, want []expectedListenerEvent) {
	t.Helper()
	require.Equal(t, len(want), len(events))
	for i := range want {
		got, wantEvent := events[i], want[i]
		require.Equal(t, wantEvent.phase, got.phase)
		require.Equal(t, wantEvent.function, got.function)
		if wantEvent.errIs != nil {
			require.True(t, errors.Is(got.err, wantEvent.errIs), "event %d error = %v, want %v", i, got.err, wantEvent.errIs)
		} else if wantEvent.errContains == "" {
			require.Nil(t, got.err)
		}
		if wantEvent.errContains != "" {
			require.Contains(t, got.err.Error(), wantEvent.errContains)
		}
	}
}

func assertBeforeCompletionPairing(t *testing.T, events []listenerEvent) {
	assertBeforeCompletionPairingExcept(t, events)
}

func assertBeforeCompletionPairingExcept(t *testing.T, events []listenerEvent, allowedOrphanAbortFunctions ...string) {
	t.Helper()

	type counts struct {
		before int
		after  int
		abort  int
	}

	perFunction := map[string]counts{}
	for _, event := range events {
		current := perFunction[event.function]
		switch event.phase {
		case "before":
			current.before++
		case "after":
			current.after++
		case "abort":
			current.abort++
		default:
			t.Fatalf("unexpected phase %q", event.phase)
		}
		perFunction[event.function] = current
	}

	allowedOrphanAbort := map[string]struct{}{}
	for _, function := range allowedOrphanAbortFunctions {
		allowedOrphanAbort[function] = struct{}{}
	}

	for function, current := range perFunction {
		if _, ok := allowedOrphanAbort[function]; ok {
			require.Equal(t, current.before+1, current.after+current.abort, "unpaired listener events for %s", function)
		} else {
			require.Equal(t, current.before, current.after+current.abort, "unpaired listener events for %s", function)
		}
		require.True(t, current.after == 0 || current.abort == 0, "function %s completed with both after and abort", function)
	}
}

func TestFunctionListener_BeforeAfterAndAbortPairing(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, newFunctionListenerFactory(recorder))

			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			instantiateListenerHostModule(t, instantiateCtx, rt, func() {})
			mod := instantiateListenerGuestModule(t, instantiateCtx, rt, trapObserverTestModuleBinary())

			_, err := mod.ExportedFunction("run").Call(ctx)
			require.NoError(t, err)

			events := recorder.snapshot()
			assertListenerEvents(t, events, []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "check"},
				{phase: "after", function: "check"},
				{phase: "after", function: "run"},
			})
			assertBeforeCompletionPairing(t, events)
		})
	}
}

func TestFunctionListener_AbortTrapPaths(t *testing.T) {
	testCases := []struct {
		name                        string
		cfg                         func() wazero.RuntimeConfig
		supported                   func() bool
		moduleBinary                func(t *testing.T) []byte
		exportName                  string
		setupHost                   func(t *testing.T, ctx context.Context, rt wazero.Runtime)
		callCtx                     func(context.Context) context.Context
		wantErr                     error
		wantTrapCount               int
		wantEvents                  []expectedListenerEvent
		allowedOrphanAbortFunctions []string
	}{
		{
			name:          "interpreter/unreachable",
			cfg:           wazero.NewRuntimeConfigInterpreter,
			supported:     func() bool { return true },
			moduleBinary:  func(t *testing.T) []byte { return trapRuntimeUnreachableBinary() },
			exportName:    "run",
			wantErr:       wasmruntime.ErrRuntimeUnreachable,
			wantTrapCount: 1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeUnreachable},
			},
		},
		{
			name:          "compiler/unreachable",
			cfg:           wazero.NewRuntimeConfigCompiler,
			supported:     platform.CompilerSupported,
			moduleBinary:  func(t *testing.T) []byte { return trapRuntimeUnreachableBinary() },
			exportName:    "run",
			wantErr:       wasmruntime.ErrRuntimeUnreachable,
			wantTrapCount: 1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeUnreachable},
			},
		},
		{
			name:          "interpreter/memory-oob",
			cfg:           wazero.NewRuntimeConfigInterpreter,
			supported:     func() bool { return true },
			moduleBinary:  func(t *testing.T) []byte { return trapRuntimeOutOfBoundsLoadBinary() },
			exportName:    "run",
			wantErr:       wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess,
			wantTrapCount: 1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess},
			},
		},
		{
			name:          "compiler/memory-oob",
			cfg:           wazero.NewRuntimeConfigCompiler,
			supported:     platform.CompilerSupported,
			moduleBinary:  func(t *testing.T) []byte { return trapRuntimeOutOfBoundsLoadBinary() },
			exportName:    "run",
			wantErr:       wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess,
			wantTrapCount: 1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess},
			},
		},
		{
			name:          "compiler/memory-fault",
			cfg:           func() wazero.RuntimeConfig { return wazero.NewRuntimeConfigCompiler().WithSecureMode(true) },
			supported:     supportsGuardedMemoryFaultTrap,
			moduleBinary:  trapRuntimeMemoryFaultFixture,
			exportName:    "oob",
			wantErr:       wasmruntime.ErrRuntimeMemoryFault,
			wantTrapCount: 1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "oob"},
				{phase: "abort", function: "oob", errIs: wasmruntime.ErrRuntimeMemoryFault},
			},
		},
		{
			name:          "compiler/fuel-exhausted",
			cfg:           func() wazero.RuntimeConfig { return wazero.NewRuntimeConfigCompiler().WithFuel(1) },
			supported:     platform.CompilerSupported,
			moduleBinary:  func(t *testing.T) []byte { return trapObserverFuelLoopBinary() },
			exportName:    "run",
			wantErr:       wasmruntime.ErrRuntimeFuelExhausted,
			wantTrapCount: 1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeFuelExhausted},
			},
		},
		{
			name:         "interpreter/policy-denied",
			cfg:          wazero.NewRuntimeConfigInterpreter,
			supported:    func() bool { return true },
			moduleBinary: func(t *testing.T) []byte { return trapObserverTestModuleBinary() },
			exportName:   "run",
			setupHost: func(t *testing.T, ctx context.Context, rt wazero.Runtime) {
				instantiateListenerHostModule(t, ctx, rt, func() {})
			},
			callCtx: func(ctx context.Context) context.Context {
				return experimental.WithHostCallPolicy(ctx, experimental.HostCallPolicyFunc(
					func(context.Context, api.Module, api.FunctionDefinition) bool { return false },
				))
			},
			wantErr:       wasmruntime.ErrRuntimePolicyDenied,
			wantTrapCount: 1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimePolicyDenied},
			},
		},
		{
			name:         "compiler/policy-denied",
			cfg:          wazero.NewRuntimeConfigCompiler,
			supported:    platform.CompilerSupported,
			moduleBinary: func(t *testing.T) []byte { return trapObserverTestModuleBinary() },
			exportName:   "run",
			setupHost: func(t *testing.T, ctx context.Context, rt wazero.Runtime) {
				instantiateListenerHostModule(t, ctx, rt, func() {})
			},
			callCtx: func(ctx context.Context) context.Context {
				return experimental.WithHostCallPolicy(ctx, experimental.HostCallPolicyFunc(
					func(context.Context, api.Module, api.FunctionDefinition) bool { return false },
				))
			},
			wantErr:       wasmruntime.ErrRuntimePolicyDenied,
			wantTrapCount: 1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "check", errIs: wasmruntime.ErrRuntimePolicyDenied},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimePolicyDenied},
			},
			allowedOrphanAbortFunctions: []string{"check"},
		},
		{
			name:         "interpreter/host-panic",
			cfg:          wazero.NewRuntimeConfigInterpreter,
			supported:    func() bool { return true },
			moduleBinary: func(t *testing.T) []byte { return trapObserverTestModuleBinary() },
			exportName:   "run",
			setupHost: func(t *testing.T, ctx context.Context, rt wazero.Runtime) {
				instantiateListenerHostModule(t, ctx, rt, func() { panic("boom") })
			},
			wantTrapCount: 0,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "check"},
				{phase: "abort", function: "check", errContains: "boom"},
				{phase: "abort", function: "run", errContains: "boom"},
			},
		},
		{
			name:         "compiler/host-panic",
			cfg:          wazero.NewRuntimeConfigCompiler,
			supported:    platform.CompilerSupported,
			moduleBinary: func(t *testing.T) []byte { return trapObserverTestModuleBinary() },
			exportName:   "run",
			setupHost: func(t *testing.T, ctx context.Context, rt wazero.Runtime) {
				instantiateListenerHostModule(t, ctx, rt, func() { panic("boom") })
			},
			wantTrapCount: 0,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "check"},
				{phase: "abort", function: "check", errContains: "boom"},
				{phase: "abort", function: "run", errContains: "boom"},
			},
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			if !tc.supported() {
				t.Skip("trap path is not supported on this host")
			}

			ctx := context.Background()
			recorder := &recordingFunctionListener{}
			trapObserver := &recordingTrapObserver{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, newFunctionListenerFactory(recorder))

			rt := wazero.NewRuntimeWithConfig(ctx, tc.cfg())
			defer rt.Close(ctx)

			if tc.setupHost != nil {
				tc.setupHost(t, instantiateCtx, rt)
			}

			mod := instantiateListenerGuestModule(t, instantiateCtx, rt, tc.moduleBinary(t))

			callCtx := experimental.WithTrapObserver(ctx, trapObserver)
			if tc.callCtx != nil {
				callCtx = tc.callCtx(callCtx)
			}

			_, err := mod.ExportedFunction(tc.exportName).Call(callCtx)
			require.Error(t, err)
			if tc.wantErr != nil {
				require.True(t, errors.Is(err, tc.wantErr), "expected %v, got %v", tc.wantErr, err)
			}

			events := recorder.snapshot()
			assertListenerEvents(t, events, tc.wantEvents)
			assertBeforeCompletionPairingExcept(t, events, tc.allowedOrphanAbortFunctions...)
			require.Equal(t, tc.wantTrapCount, trapObserver.count())

			if tc.wantTrapCount == 0 {
				for _, event := range events {
					if event.phase != "abort" {
						continue
					}
					_, ok := experimental.TrapCauseOf(event.err)
					require.False(t, ok, "host panic abort was misclassified as a trap: %v", event.err)
				}
			}
		})
	}
}
