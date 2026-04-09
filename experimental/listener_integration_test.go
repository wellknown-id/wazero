package experimental_test

import (
	"context"
	"errors"
	"fmt"
	"strings"
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

type orderedEventRecorder struct {
	mu     sync.Mutex
	events []string
}

func (r *orderedEventRecorder) add(format string, args ...any) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.events = append(r.events, fmt.Sprintf(format, args...))
}

func (r *orderedEventRecorder) snapshot() []string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]string(nil), r.events...)
}

type orderedFunctionListener struct {
	recorder *orderedEventRecorder
}

func (l *orderedFunctionListener) Before(_ context.Context, _ api.Module, def api.FunctionDefinition, _ []uint64, _ experimental.StackIterator) {
	l.recorder.add("listener before %s", listenerFunctionName(def))
}

func (l *orderedFunctionListener) After(_ context.Context, _ api.Module, def api.FunctionDefinition, _ []uint64) {
	l.recorder.add("listener after %s", listenerFunctionName(def))
}

func (l *orderedFunctionListener) Abort(_ context.Context, _ api.Module, def api.FunctionDefinition, err error) {
	cause, ok := experimental.TrapCauseOf(err)
	if ok {
		l.recorder.add("listener abort %s (%s)", listenerFunctionName(def), cause)
		return
	}
	l.recorder.add("listener abort %s", listenerFunctionName(def))
}

type orderedRecordingFunctionListener struct {
	recorder *orderedEventRecorder
	events   *recordingFunctionListener
}

func (l *orderedRecordingFunctionListener) Before(ctx context.Context, mod api.Module, def api.FunctionDefinition, params []uint64, stack experimental.StackIterator) {
	l.events.Before(ctx, mod, def, params, stack)
	l.recorder.add("listener before %s", listenerFunctionName(def))
}

func (l *orderedRecordingFunctionListener) After(ctx context.Context, mod api.Module, def api.FunctionDefinition, results []uint64) {
	l.events.After(ctx, mod, def, results)
	l.recorder.add("listener after %s", listenerFunctionName(def))
}

func (l *orderedRecordingFunctionListener) Abort(ctx context.Context, mod api.Module, def api.FunctionDefinition, err error) {
	l.events.Abort(ctx, mod, def, err)
	cause, ok := experimental.TrapCauseOf(err)
	if ok {
		l.recorder.add("listener abort %s (%s)", listenerFunctionName(def), cause)
		return
	}
	l.recorder.add("listener abort %s", listenerFunctionName(def))
}

type stackSnapshot struct {
	function string
	frames   []string
}

type stackRecordingFunctionListener struct {
	mu     sync.Mutex
	events []listenerEvent
	stacks []stackSnapshot
}

func (l *stackRecordingFunctionListener) Before(_ context.Context, _ api.Module, def api.FunctionDefinition, _ []uint64, stack experimental.StackIterator) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.events = append(l.events, listenerEvent{phase: "before", function: listenerFunctionName(def)})
	l.stacks = append(l.stacks, stackSnapshot{function: listenerFunctionName(def), frames: snapshotStackIterator(stack)})
}

func (l *stackRecordingFunctionListener) After(_ context.Context, _ api.Module, def api.FunctionDefinition, _ []uint64) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.events = append(l.events, listenerEvent{phase: "after", function: listenerFunctionName(def)})
}

func (l *stackRecordingFunctionListener) Abort(_ context.Context, _ api.Module, def api.FunctionDefinition, err error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.events = append(l.events, listenerEvent{phase: "abort", function: listenerFunctionName(def), err: err})
}

func (l *stackRecordingFunctionListener) snapshotEvents() []listenerEvent {
	l.mu.Lock()
	defer l.mu.Unlock()
	return append([]listenerEvent(nil), l.events...)
}

func (l *stackRecordingFunctionListener) snapshotStacks() []stackSnapshot {
	l.mu.Lock()
	defer l.mu.Unlock()
	snapshots := make([]stackSnapshot, len(l.stacks))
	for i := range l.stacks {
		snapshots[i] = stackSnapshot{
			function: l.stacks[i].function,
			frames:   append([]string(nil), l.stacks[i].frames...),
		}
	}
	return snapshots
}

func snapshotStackIterator(stack experimental.StackIterator) []string {
	var frames []string
	for stack.Next() {
		frames = append(frames, listenerFunctionName(stack.Function().Definition()))
	}
	return frames
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

func assertAbortTrapCauseClassification(t *testing.T, events []listenerEvent, wantCause experimental.TrapCause, wantTrap bool) {
	t.Helper()

	abortCount := 0
	for _, event := range events {
		if event.phase != "abort" {
			continue
		}

		abortCount++
		cause, ok := experimental.TrapCauseOf(event.err)
		require.Equal(t, wantTrap, ok, "abort %s trap classification mismatch: %v", event.function, event.err)
		if wantTrap {
			require.Equal(t, wantCause, cause, "abort %s trap cause mismatch", event.function)
		} else {
			require.Equal(t, experimental.TrapCause(""), cause, "abort %s should not expose a trap cause", event.function)
		}
	}

	require.True(t, abortCount > 0, "expected at least one abort event")
}

func assertBeforeCompletionPairing(t *testing.T, events []listenerEvent) {
	assertBeforeCompletionPairingExcept(t, events)
}

func assertBeforeCompletionStackPairing(t *testing.T, events []listenerEvent) {
	t.Helper()

	type frame struct {
		function string
	}

	var stack []frame
	for _, event := range events {
		switch event.phase {
		case "before":
			stack = append(stack, frame{function: event.function})
		case "after", "abort":
			require.True(t, len(stack) > 0, "unexpected %s event for %s with empty listener stack", event.phase, event.function)
			top := stack[len(stack)-1]
			require.Equal(t, top.function, event.function, "listener %s for %s did not match innermost before", event.phase, event.function)
			stack = stack[:len(stack)-1]
		default:
			t.Fatalf("unexpected phase %q", event.phase)
		}
	}

	require.Zero(t, len(stack), "unpaired listener events remain on stack: %v", stack)
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

func assertOrderedEvents(t *testing.T, got, want []string) {
	t.Helper()
	require.Equal(t, want, got)
}

func assertOrderedSubsequence(t *testing.T, got, want []string) {
	t.Helper()
	index := 0
	for _, event := range got {
		if index < len(want) && event == want[index] {
			index++
		}
	}
	require.Equal(t, len(want), index, "got events %v, want subsequence %v", got, want)
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

func TestFunctionListener_MultiFunctionListenerFactoryStackIteratorIndependence(t *testing.T) {
	testCases := []struct {
		name       string
		hostFunc   func()
		wantErr    string
		wantEvents []expectedListenerEvent
	}{
		{
			name:     "completion",
			hostFunc: func() {},
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "check"},
				{phase: "after", function: "check"},
				{phase: "after", function: "run"},
			},
		},
		{
			name:     "abort",
			hostFunc: func() { panic("boom") },
			wantErr:  "boom",
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "check"},
				{phase: "abort", function: "check", errContains: "boom"},
				{phase: "abort", function: "run", errContains: "boom"},
			},
		},
	}

	for _, tc := range testCases {
		for _, ec := range engineConfigs() {
			t.Run(ec.name+"/"+tc.name, func(t *testing.T) {
				if ec.name == "compiler" && !platform.CompilerSupported() {
					t.Skip("compiler is not supported on this host")
				}

				ctx := context.Background()
				first := &stackRecordingFunctionListener{}
				second := &stackRecordingFunctionListener{}
				instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
					newFunctionListenerFactory(first),
					newFunctionListenerFactory(second),
				))

				rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
				defer rt.Close(ctx)

				instantiateListenerHostModule(t, instantiateCtx, rt, tc.hostFunc)
				mod := instantiateListenerGuestModule(t, instantiateCtx, rt, trapObserverTestModuleBinary())

				_, err := mod.ExportedFunction("run").Call(ctx)
				if tc.wantErr == "" {
					require.NoError(t, err)
				} else {
					require.Error(t, err)
					require.Contains(t, err.Error(), tc.wantErr)
				}

				firstEvents := first.snapshotEvents()
				secondEvents := second.snapshotEvents()
				assertListenerEvents(t, firstEvents, tc.wantEvents)
				assertListenerEvents(t, secondEvents, tc.wantEvents)
				require.Equal(t, firstEvents, secondEvents)

				assertBeforeCompletionPairing(t, firstEvents)
				assertBeforeCompletionPairing(t, secondEvents)

				firstStacks := first.snapshotStacks()
				secondStacks := second.snapshotStacks()
				require.Equal(t, firstStacks, secondStacks)
				require.Equal(t, 2, len(firstStacks))
				require.Equal(t, []stackSnapshot{
					{function: "run", frames: []string{"run"}},
				}, firstStacks[:1])
				require.Equal(t, "check", firstStacks[1].function)
				require.Equal(t, 2, len(firstStacks[1].frames))
				require.Equal(t, "run", firstStacks[1].frames[len(firstStacks[1].frames)-1])
			})
		}
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
		wantCause                   experimental.TrapCause
		wantTrapClassification      bool
		wantTrapCount               int
		wantEvents                  []expectedListenerEvent
		allowedOrphanAbortFunctions []string
	}{
		{
			name:                   "interpreter/unreachable",
			cfg:                    wazero.NewRuntimeConfigInterpreter,
			supported:              func() bool { return true },
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeUnreachableBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeUnreachable,
			wantCause:              experimental.TrapCauseUnreachable,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeUnreachable},
			},
		},
		{
			name:                   "compiler/unreachable",
			cfg:                    wazero.NewRuntimeConfigCompiler,
			supported:              platform.CompilerSupported,
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeUnreachableBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeUnreachable,
			wantCause:              experimental.TrapCauseUnreachable,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeUnreachable},
			},
		},
		{
			name:                   "interpreter/memory-oob",
			cfg:                    wazero.NewRuntimeConfigInterpreter,
			supported:              func() bool { return true },
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeOutOfBoundsLoadBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess,
			wantCause:              experimental.TrapCauseOutOfBoundsMemoryAccess,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess},
			},
		},
		{
			name:                   "compiler/memory-oob",
			cfg:                    wazero.NewRuntimeConfigCompiler,
			supported:              platform.CompilerSupported,
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeOutOfBoundsLoadBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess,
			wantCause:              experimental.TrapCauseOutOfBoundsMemoryAccess,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess},
			},
		},
		{
			name:                   "compiler/memory-fault",
			cfg:                    func() wazero.RuntimeConfig { return wazero.NewRuntimeConfigCompiler().WithSecureMode(true) },
			supported:              supportsGuardedMemoryFaultTrap,
			moduleBinary:           trapRuntimeMemoryFaultFixture,
			exportName:             "oob",
			wantErr:                wasmruntime.ErrRuntimeMemoryFault,
			wantCause:              experimental.TrapCauseMemoryFault,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "oob"},
				{phase: "abort", function: "oob", errIs: wasmruntime.ErrRuntimeMemoryFault},
			},
		},
		{
			name:                   "compiler/fuel-exhausted",
			cfg:                    func() wazero.RuntimeConfig { return wazero.NewRuntimeConfigCompiler().WithFuel(1) },
			supported:              platform.CompilerSupported,
			moduleBinary:           func(t *testing.T) []byte { return trapObserverFuelLoopBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeFuelExhausted,
			wantCause:              experimental.TrapCauseFuelExhausted,
			wantTrapClassification: true,
			wantTrapCount:          1,
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
			wantErr:                wasmruntime.ErrRuntimePolicyDenied,
			wantCause:              experimental.TrapCausePolicyDenied,
			wantTrapClassification: true,
			wantTrapCount:          1,
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
			wantErr:                wasmruntime.ErrRuntimePolicyDenied,
			wantCause:              experimental.TrapCausePolicyDenied,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimePolicyDenied},
			},
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
			wantTrapClassification: false,
			wantTrapCount:          0,
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
			wantTrapClassification: false,
			wantTrapCount:          0,
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
			assertAbortTrapCauseClassification(t, events, tc.wantCause, tc.wantTrapClassification)
			assertBeforeCompletionPairingExcept(t, events, tc.allowedOrphanAbortFunctions...)
			require.Equal(t, tc.wantTrapCount, trapObserver.count())
		})
	}
}

func TestFunctionListener_TrapObserverOrdering(t *testing.T) {
	testCases := []struct {
		name            string
		cfg             func() wazero.RuntimeConfig
		supported       func() bool
		moduleBinary    func(t *testing.T) []byte
		exportName      string
		setupHost       func(t *testing.T, ctx context.Context, rt wazero.Runtime)
		callCtx         func(context.Context) context.Context
		wantErr         error
		wantCause       experimental.TrapCause
		wantSubsequence []string
	}{
		{
			name:         "interpreter/unreachable",
			cfg:          wazero.NewRuntimeConfigInterpreter,
			supported:    func() bool { return true },
			moduleBinary: func(t *testing.T) []byte { return trapRuntimeUnreachableBinary() },
			exportName:   "run",
			wantErr:      wasmruntime.ErrRuntimeUnreachable,
			wantCause:    experimental.TrapCauseUnreachable,
			wantSubsequence: []string{
				"listener before run",
				"listener abort run (unreachable)",
				"trap unreachable",
			},
		},
		{
			name:         "interpreter/memory-oob",
			cfg:          wazero.NewRuntimeConfigInterpreter,
			supported:    func() bool { return true },
			moduleBinary: func(t *testing.T) []byte { return trapRuntimeOutOfBoundsLoadBinary() },
			exportName:   "run",
			wantErr:      wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess,
			wantCause:    experimental.TrapCauseOutOfBoundsMemoryAccess,
			wantSubsequence: []string{
				"listener before run",
				"listener abort run (out_of_bounds_memory_access)",
				"trap out_of_bounds_memory_access",
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
			wantErr:   wasmruntime.ErrRuntimePolicyDenied,
			wantCause: experimental.TrapCausePolicyDenied,
			wantSubsequence: []string{
				"listener before run",
				"listener abort run (policy_denied)",
				"trap policy_denied",
			},
		},
		{
			name:         "compiler/unreachable",
			cfg:          wazero.NewRuntimeConfigCompiler,
			supported:    platform.CompilerSupported,
			moduleBinary: func(t *testing.T) []byte { return trapRuntimeUnreachableBinary() },
			exportName:   "run",
			wantErr:      wasmruntime.ErrRuntimeUnreachable,
			wantCause:    experimental.TrapCauseUnreachable,
			wantSubsequence: []string{
				"listener before run",
				"listener abort run (unreachable)",
				"trap unreachable",
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
			wantErr:   wasmruntime.ErrRuntimePolicyDenied,
			wantCause: experimental.TrapCausePolicyDenied,
			wantSubsequence: []string{
				"listener before run",
				"listener abort run (policy_denied)",
				"trap policy_denied",
			},
		},
		{
			name:         "compiler/fuel-exhausted",
			cfg:          func() wazero.RuntimeConfig { return wazero.NewRuntimeConfigCompiler().WithFuel(1) },
			supported:    platform.CompilerSupported,
			moduleBinary: func(t *testing.T) []byte { return trapObserverFuelLoopBinary() },
			exportName:   "run",
			wantErr:      wasmruntime.ErrRuntimeFuelExhausted,
			wantCause:    experimental.TrapCauseFuelExhausted,
			wantSubsequence: []string{
				"listener before run",
				"listener abort run (fuel_exhausted)",
				"trap fuel_exhausted",
			},
		},
		{
			name:         "compiler/memory-fault",
			cfg:          func() wazero.RuntimeConfig { return wazero.NewRuntimeConfigCompiler().WithSecureMode(true) },
			supported:    supportsGuardedMemoryFaultTrap,
			moduleBinary: trapRuntimeMemoryFaultFixture,
			exportName:   "oob",
			wantErr:      wasmruntime.ErrRuntimeMemoryFault,
			wantCause:    experimental.TrapCauseMemoryFault,
			wantSubsequence: []string{
				"listener before oob",
				"listener abort oob (memory_fault)",
				"trap memory_fault",
			},
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			if !tc.supported() {
				t.Skip("trap path is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			trapObserver := &recordingTrapObserver{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, newFunctionListenerFactory(&orderedFunctionListener{recorder: recorder}))

			rt := wazero.NewRuntimeWithConfig(ctx, tc.cfg())
			defer rt.Close(ctx)

			if tc.setupHost != nil {
				tc.setupHost(t, instantiateCtx, rt)
			}

			mod := instantiateListenerGuestModule(t, instantiateCtx, rt, tc.moduleBinary(t))

			callCtx := experimental.WithTrapObserver(ctx, experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
				trapObserver.ObserveTrap(ctx, observation)
				recorder.add("trap %s", observation.Cause)
			}))
			if tc.callCtx != nil {
				callCtx = tc.callCtx(callCtx)
			}

			_, err := mod.ExportedFunction(tc.exportName).Call(callCtx)
			require.ErrorIs(t, err, tc.wantErr)

			cause, ok := experimental.TrapCauseOf(err)
			require.True(t, ok)
			require.Equal(t, tc.wantCause, cause)

			observation := trapObserver.single(t)
			require.Equal(t, tc.wantCause, observation.Cause)
			require.ErrorIs(t, observation.Err, tc.wantErr)
			require.Equal(t, "guest", observation.Module.Name())

			events := recorder.snapshot()
			assertOrderedSubsequence(t, events, tc.wantSubsequence)

			trapEvent := fmt.Sprintf("trap %s", tc.wantCause)
			trapCount, trapIndex, abortCount := 0, -1, 0
			for i, event := range events {
				if event == trapEvent {
					trapCount++
					trapIndex = i
					continue
				}
				if strings.HasPrefix(event, "listener abort ") {
					abortCount++
					require.True(t, trapIndex == -1, "listener abort observed after trap event: %v", events)
				}
			}

			require.Equal(t, 1, trapCount)
			require.True(t, trapIndex >= 0)
			require.True(t, abortCount > 0)
		})
	}
}

func TestFunctionListener_PolicyObserverOrdering(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, newFunctionListenerFactory(&orderedFunctionListener{recorder: recorder}))

			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			instantiateListenerHostModule(t, instantiateCtx, rt, func() {})
			mod := instantiateListenerGuestModule(t, instantiateCtx, rt, trapObserverTestModuleBinary())

			callCtx := experimental.WithTrapObserver(
				experimental.WithHostCallPolicyObserver(
					experimental.WithHostCallPolicy(ctx, experimental.HostCallPolicyFunc(
						func(context.Context, api.Module, api.FunctionDefinition) bool { return false },
					)),
					experimental.HostCallPolicyObserverFunc(func(_ context.Context, observation experimental.HostCallPolicyObservation) {
						recorder.add("policy %s %s", observation.Event, observation.HostFunction.DebugName())
					}),
				),
				experimental.TrapObserverFunc(func(_ context.Context, observation experimental.TrapObservation) {
					recorder.add("trap %s", observation.Cause)
				}),
			)

			_, err := mod.ExportedFunction("run").Call(callCtx)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)

			want := []string{
				"listener before run",
				"policy denied env.check",
				"listener abort run (policy_denied)",
				"trap policy_denied",
			}
			assertOrderedEvents(t, recorder.snapshot(), want)
		})
	}
}

func TestFunctionListener_YieldPolicyDeniedTrapObserverOrdering(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			listenerEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, newFunctionListenerFactory(&orderedRecordingFunctionListener{
				recorder: recorder,
				events:   listenerEvents,
			}))

			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			_, err := rt.NewHostModuleBuilder("example").
				NewFunctionBuilder().
				WithGoModuleFunction(&yieldingHostFunc{t: t}, nil, []api.ValueType{api.ValueTypeI32}).
				Export("async_work").
				Instantiate(instantiateCtx)
			require.NoError(t, err)

			mod, err := rt.InstantiateWithConfig(instantiateCtx, yieldWasm, wazero.NewModuleConfig().WithName("guest"))
			require.NoError(t, err)

			trapObserver := &recordingTrapObserver{}
			callCtx := experimental.WithTrapObserver(
				experimental.WithYieldPolicy(
					experimental.WithYielder(ctx),
					experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
						require.NotNil(t, caller)
						require.Equal(t, "example.async_work", hostFunction.DebugName())
						return false
					}),
				),
				experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
					trapObserver.ObserveTrap(ctx, observation)
					recorder.add("trap %s", observation.Cause)
				}),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)

			cause, ok := experimental.TrapCauseOf(err)
			require.True(t, ok)
			require.Equal(t, experimental.TrapCausePolicyDenied, cause)

			events := listenerEvents.snapshot()
			assertListenerEvents(t, events, []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "abort", function: "async_work", errIs: wasmruntime.ErrRuntimePolicyDenied},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimePolicyDenied},
			})
			assertAbortTrapCauseClassification(t, events, experimental.TrapCausePolicyDenied, true)
			assertBeforeCompletionPairing(t, events)

			observation := trapObserver.single(t)
			require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
			require.ErrorIs(t, observation.Err, wasmruntime.ErrRuntimePolicyDenied)
			require.Equal(t, mod.Name(), observation.Module.Name())

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener before run",
				"listener before async_work",
				"listener abort async_work (policy_denied)",
				"listener abort run (policy_denied)",
				"trap policy_denied",
			})
		})
	}
}

func TestFunctionListener_ResumeYieldPolicyDeniedTrapObserverOrdering(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			listenerEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, newFunctionListenerFactory(&orderedRecordingFunctionListener{
				recorder: recorder,
				events:   listenerEvents,
			}))

			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			_, err := rt.NewHostModuleBuilder("example").
				NewFunctionBuilder().
				WithGoModuleFunction(&yieldingHostFunc{t: t}, nil, []api.ValueType{api.ValueTypeI32}).
				Export("async_work").
				Instantiate(instantiateCtx)
			require.NoError(t, err)

			mod, err := rt.InstantiateWithConfig(instantiateCtx, yieldWasm, wazero.NewModuleConfig().WithName("guest"))
			require.NoError(t, err)

			_, err = mod.ExportedFunction("run_twice").Call(experimental.WithYielder(ctx))
			yieldErr := requireYieldError(t, err)

			trapObserver := &recordingTrapObserver{}
			_, err = yieldErr.Resumer().Resume(
				experimental.WithTrapObserver(
					experimental.WithYieldPolicy(
						experimental.WithYielder(ctx),
						experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.NotNil(t, caller)
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return false
						}),
					),
					experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
						trapObserver.ObserveTrap(ctx, observation)
						recorder.add("trap %s", observation.Cause)
					}),
				),
				[]uint64{1},
			)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)

			cause, ok := experimental.TrapCauseOf(err)
			require.True(t, ok)
			require.Equal(t, experimental.TrapCausePolicyDenied, cause)

			events := listenerEvents.snapshot()
			assertListenerEvents(t, events, []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "before", function: "async_work"},
				{phase: "abort", function: "async_work", errIs: wasmruntime.ErrRuntimePolicyDenied},
				{phase: "abort", function: "run_twice", errIs: wasmruntime.ErrRuntimePolicyDenied},
			})
			assertAbortTrapCauseClassification(t, events, experimental.TrapCausePolicyDenied, true)
			assertBeforeCompletionStackPairing(t, events)

			observation := trapObserver.single(t)
			require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
			require.ErrorIs(t, observation.Err, wasmruntime.ErrRuntimePolicyDenied)
			require.Equal(t, mod.Name(), observation.Module.Name())

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener before run_twice",
				"listener before async_work",
				"listener after async_work",
				"listener before async_work",
				"listener abort async_work (policy_denied)",
				"listener abort run_twice (policy_denied)",
				"trap policy_denied",
			})
		})
	}
}

func TestFunctionListener_YieldObserverOrdering(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, newFunctionListenerFactory(&orderedFunctionListener{recorder: recorder}))

			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			_, err := rt.NewHostModuleBuilder("example").
				NewFunctionBuilder().
				WithGoModuleFunction(&yieldingHostFunc{t: t}, nil, []api.ValueType{api.ValueTypeI32}).
				Export("async_work").
				Instantiate(instantiateCtx)
			require.NoError(t, err)

			mod, err := rt.InstantiateWithConfig(instantiateCtx, yieldWasm, wazero.NewModuleConfig().WithName("guest"))
			require.NoError(t, err)

			callCtx := experimental.WithYieldObserver(
				experimental.WithYielder(ctx),
				experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
					recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
				}),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			yieldErr := requireYieldError(t, err)

			results, err := yieldErr.Resumer().Resume(
				experimental.WithYieldObserver(
					experimental.WithYielder(ctx),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
				[]uint64{42},
			)
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener before run",
				"listener before async_work",
				"yield yielded #1",
				"yield resumed #1",
				"listener after async_work",
				"listener after run",
			})
		})
	}
}

func TestFunctionListener_FuelObserverOrdering(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, newFunctionListenerFactory(&orderedFunctionListener{recorder: recorder}))

			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg.WithFuel(3))
			defer rt.Close(ctx)

			mod := instantiateListenerGuestModule(t, instantiateCtx, rt, fuelLoopModuleBinary())

			_, err := mod.ExportedFunction("run").Call(experimental.WithFuelObserver(
				ctx,
				experimental.FuelObserverFunc(func(_ context.Context, observation experimental.FuelObservation) {
					recorder.add("fuel %s", observation.Event)
				}),
			))
			require.ErrorIs(t, err, wasmruntime.ErrRuntimeFuelExhausted)

			assertOrderedSubsequence(t, recorder.snapshot(), []string{
				"fuel budgeted",
				"listener before run",
				"listener abort run (fuel_exhausted)",
				"fuel exhausted",
			})
		})
	}
}
