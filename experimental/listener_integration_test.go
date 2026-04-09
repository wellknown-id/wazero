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
	"github.com/tetratelabs/wazero/internal/testing/binaryencoding"
	"github.com/tetratelabs/wazero/internal/testing/require"
	"github.com/tetratelabs/wazero/internal/wasm"
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

type orderedRecordingFuelObserver struct {
	recorder     *orderedEventRecorder
	observations *recordingFuelObserver
}

func (o *orderedRecordingFuelObserver) ObserveFuel(ctx context.Context, observation experimental.FuelObservation) {
	o.observations.ObserveFuel(ctx, observation)
	o.recorder.add("fuel %s", observation.Event)
}

type namedOrderedRecordingFuelObserver struct {
	name         string
	recorder     *orderedEventRecorder
	observations *recordingFuelObserver
}

func (o *namedOrderedRecordingFuelObserver) ObserveFuel(ctx context.Context, observation experimental.FuelObservation) {
	o.observations.ObserveFuel(ctx, observation)
	o.recorder.add("%s %s", o.name, observation.Event)
}

type namedOrderedRecordingFunctionListener struct {
	name     string
	recorder *orderedEventRecorder
	events   *recordingFunctionListener
}

func (l *namedOrderedRecordingFunctionListener) Before(ctx context.Context, mod api.Module, def api.FunctionDefinition, params []uint64, stack experimental.StackIterator) {
	l.events.Before(ctx, mod, def, params, stack)
	l.recorder.add("%s before %s", l.name, listenerFunctionName(def))
}

func (l *namedOrderedRecordingFunctionListener) After(ctx context.Context, mod api.Module, def api.FunctionDefinition, results []uint64) {
	l.events.After(ctx, mod, def, results)
	l.recorder.add("%s after %s", l.name, listenerFunctionName(def))
}

func (l *namedOrderedRecordingFunctionListener) Abort(ctx context.Context, mod api.Module, def api.FunctionDefinition, err error) {
	l.events.Abort(ctx, mod, def, err)
	cause, ok := experimental.TrapCauseOf(err)
	if ok {
		l.recorder.add("%s abort %s (%s)", l.name, listenerFunctionName(def), cause)
		return
	}
	l.recorder.add("%s abort %s", l.name, listenerFunctionName(def))
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

func resumeArithmeticTrapModuleBinary(cause experimental.TrapCause) []byte {
	innerBody := []byte{wasm.OpcodeCall, 0}
	switch cause {
	case experimental.TrapCauseIntegerDivideByZero:
		innerBody = append(innerBody,
			wasm.OpcodeI32Const, 0x00,
			wasm.OpcodeI32DivS,
		)
	case experimental.TrapCauseIntegerOverflow:
		innerBody = append(innerBody,
			wasm.OpcodeDrop,
			wasm.OpcodeI32Const, 0x80, 0x80, 0x80, 0x80, 0x78,
			wasm.OpcodeI32Const, 0x7f,
			wasm.OpcodeI32DivS,
		)
	case experimental.TrapCauseInvalidConversionToInteger:
		innerBody = append(innerBody,
			wasm.OpcodeDrop,
			wasm.OpcodeF32Const, 0x00, 0x00, 0xc0, 0x7f,
			wasm.OpcodeI32TruncF32S,
		)
	default:
		panic(fmt.Sprintf("unsupported resume arithmetic trap cause %q", cause))
	}
	innerBody = append(innerBody, wasm.OpcodeEnd)

	return binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{Results: []api.ValueType{api.ValueTypeI32}}},
		ImportSection:   []wasm.Import{{Module: "example", Name: "async_work", Type: wasm.ExternTypeFunc, DescFunc: 0}},
		FunctionSection: []wasm.Index{0, 0},
		CodeSection: []wasm.Code{
			{Body: innerBody},
			{Body: []byte{wasm.OpcodeCall, 1, wasm.OpcodeEnd}},
		},
		ExportSection: []wasm.Export{
			{Type: api.ExternTypeFunc, Name: "resume_trap", Index: 1},
			{Type: api.ExternTypeFunc, Name: "run", Index: 2},
		},
	})
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

func TestFunctionListener_MultiFunctionListenerFactoryWithMultiTrapObserverAbortOrdering(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			trapLeft := &recordingTrapObserver{}
			trapRight := &recordingTrapObserver{}
			callCtx := experimental.WithTrapObserver(
				experimental.WithYieldPolicy(
					experimental.WithYielder(ctx),
					experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
						require.NotNil(t, caller)
						require.Equal(t, "example.async_work", hostFunction.DebugName())
						return false
					}),
				),
				experimental.MultiTrapObserver(
					experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
						trapLeft.ObserveTrap(ctx, observation)
						recorder.add("trap left %s", observation.Cause)
					}),
					experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
						trapRight.ObserveTrap(ctx, observation)
						recorder.add("trap right %s", observation.Cause)
					}),
				),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "abort", function: "async_work", errIs: wasmruntime.ErrRuntimePolicyDenied},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimePolicyDenied},
			}

			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertAbortTrapCauseClassification(t, events, experimental.TrapCausePolicyDenied, true)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			requireEquivalentTrapObservation(t, trapObservationSnapshot{
				Cause:        experimental.TrapCausePolicyDenied,
				ModuleName:   "guest",
				PolicyDenied: true,
			}, wasmruntime.ErrRuntimePolicyDenied, trapLeft, trapRight)

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run",
				"listener right before run",
				"listener left before async_work",
				"listener right before async_work",
				"listener left abort async_work (policy_denied)",
				"listener right abort async_work (policy_denied)",
				"listener left abort run (policy_denied)",
				"listener right abort run (policy_denied)",
				"trap left policy_denied",
				"trap right policy_denied",
			})
		})
	}
}

func TestFunctionListener_MultiFunctionListenerFactoryWithMultiTrapObserverAbortTrapPaths(t *testing.T) {
	testCases := []struct {
		name                   string
		cfg                    func() wazero.RuntimeConfig
		supported              func() bool
		moduleBinary           func(t *testing.T) []byte
		exportName             string
		setupHost              func(t *testing.T, ctx context.Context, rt wazero.Runtime)
		wantErr                error
		wantErrContains        string
		wantCause              experimental.TrapCause
		wantTrapClassification bool
		wantTrapCount          int
		wantEvents             []expectedListenerEvent
	}{
		{
			name:                   "interpreter/integer-divide-by-zero",
			cfg:                    wazero.NewRuntimeConfigInterpreter,
			supported:              func() bool { return true },
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeIntegerDivideByZeroBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeIntegerDivideByZero,
			wantCause:              experimental.TrapCauseIntegerDivideByZero,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeIntegerDivideByZero},
			},
		},
		{
			name:                   "compiler/integer-divide-by-zero",
			cfg:                    wazero.NewRuntimeConfigCompiler,
			supported:              platform.CompilerSupported,
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeIntegerDivideByZeroBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeIntegerDivideByZero,
			wantCause:              experimental.TrapCauseIntegerDivideByZero,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeIntegerDivideByZero},
			},
		},
		{
			name:                   "interpreter/integer-overflow",
			cfg:                    wazero.NewRuntimeConfigInterpreter,
			supported:              func() bool { return true },
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeIntegerOverflowBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeIntegerOverflow,
			wantCause:              experimental.TrapCauseIntegerOverflow,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeIntegerOverflow},
			},
		},
		{
			name:                   "compiler/integer-overflow",
			cfg:                    wazero.NewRuntimeConfigCompiler,
			supported:              platform.CompilerSupported,
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeIntegerOverflowBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeIntegerOverflow,
			wantCause:              experimental.TrapCauseIntegerOverflow,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeIntegerOverflow},
			},
		},
		{
			name:                   "interpreter/invalid-conversion-to-integer",
			cfg:                    wazero.NewRuntimeConfigInterpreter,
			supported:              func() bool { return true },
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeInvalidConversionToIntegerBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeInvalidConversionToInteger,
			wantCause:              experimental.TrapCauseInvalidConversionToInteger,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeInvalidConversionToInteger},
			},
		},
		{
			name:                   "compiler/invalid-conversion-to-integer",
			cfg:                    wazero.NewRuntimeConfigCompiler,
			supported:              platform.CompilerSupported,
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeInvalidConversionToIntegerBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeInvalidConversionToInteger,
			wantCause:              experimental.TrapCauseInvalidConversionToInteger,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeInvalidConversionToInteger},
			},
		},
		{
			name:                   "interpreter/out-of-bounds-memory-access",
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
			name:                   "compiler/out-of-bounds-memory-access",
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
			name:         "interpreter/host-panic",
			cfg:          wazero.NewRuntimeConfigInterpreter,
			supported:    func() bool { return true },
			moduleBinary: func(t *testing.T) []byte { return trapObserverTestModuleBinary() },
			exportName:   "run",
			setupHost: func(t *testing.T, ctx context.Context, rt wazero.Runtime) {
				instantiateListenerHostModule(t, ctx, rt, func() { panic("boom") })
			},
			wantErrContains:        "boom",
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
			wantErrContains:        "boom",
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
				t.Skip("engine is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

			rt := wazero.NewRuntimeWithConfig(ctx, tc.cfg())
			defer rt.Close(ctx)

			if tc.setupHost != nil {
				tc.setupHost(t, instantiateCtx, rt)
			}

			mod, err := rt.InstantiateWithConfig(instantiateCtx, tc.moduleBinary(t), wazero.NewModuleConfig().WithName("guest"))
			require.NoError(t, err)

			trapLeft := &recordingTrapObserver{}
			trapRight := &recordingTrapObserver{}
			callCtx := experimental.WithTrapObserver(
				ctx,
				experimental.MultiTrapObserver(
					experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
						trapLeft.ObserveTrap(ctx, observation)
						recorder.add("trap left %s", observation.Cause)
					}),
					experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
						trapRight.ObserveTrap(ctx, observation)
						recorder.add("trap right %s", observation.Cause)
					}),
				),
			)

			_, err = mod.ExportedFunction(tc.exportName).Call(callCtx)
			if tc.wantErr != nil {
				require.ErrorIs(t, err, tc.wantErr)
			} else {
				require.Error(t, err)
				require.Contains(t, err.Error(), tc.wantErrContains)
			}

			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, tc.wantEvents)
				assertAbortTrapCauseClassification(t, events, tc.wantCause, tc.wantTrapClassification)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			if tc.wantTrapCount > 0 {
				requireEquivalentTrapObservation(t, trapObservationSnapshot{
					Cause:      tc.wantCause,
					ModuleName: "guest",
				}, tc.wantErr, trapLeft, trapRight)
				assertOrderedEvents(t, recorder.snapshot(), []string{
					fmt.Sprintf("listener left before %s", tc.wantEvents[0].function),
					fmt.Sprintf("listener right before %s", tc.wantEvents[0].function),
					fmt.Sprintf("listener left abort %s (%s)", tc.wantEvents[len(tc.wantEvents)-1].function, tc.wantCause),
					fmt.Sprintf("listener right abort %s (%s)", tc.wantEvents[len(tc.wantEvents)-1].function, tc.wantCause),
					fmt.Sprintf("trap left %s", tc.wantCause),
					fmt.Sprintf("trap right %s", tc.wantCause),
				})
				if len(tc.wantEvents) > 2 {
					assertOrderedEvents(t, recorder.snapshot(), []string{
						fmt.Sprintf("listener left before %s", tc.wantEvents[0].function),
						fmt.Sprintf("listener right before %s", tc.wantEvents[0].function),
						fmt.Sprintf("listener left before %s", tc.wantEvents[1].function),
						fmt.Sprintf("listener right before %s", tc.wantEvents[1].function),
						fmt.Sprintf("listener left abort %s (%s)", tc.wantEvents[len(tc.wantEvents)-2].function, tc.wantCause),
						fmt.Sprintf("listener right abort %s (%s)", tc.wantEvents[len(tc.wantEvents)-2].function, tc.wantCause),
						fmt.Sprintf("listener left abort %s (%s)", tc.wantEvents[len(tc.wantEvents)-1].function, tc.wantCause),
						fmt.Sprintf("listener right abort %s (%s)", tc.wantEvents[len(tc.wantEvents)-1].function, tc.wantCause),
						fmt.Sprintf("trap left %s", tc.wantCause),
						fmt.Sprintf("trap right %s", tc.wantCause),
					})
				}
			} else {
				require.Zero(t, trapLeft.count())
				require.Zero(t, trapRight.count())
				assertOrderedEvents(t, recorder.snapshot(), []string{
					"listener left before run",
					"listener right before run",
					"listener left before check",
					"listener right before check",
					"listener left abort check",
					"listener right abort check",
					"listener left abort run",
					"listener right abort run",
				})
			}
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
			name:                   "interpreter/integer-divide-by-zero",
			cfg:                    wazero.NewRuntimeConfigInterpreter,
			supported:              func() bool { return true },
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeIntegerDivideByZeroBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeIntegerDivideByZero,
			wantCause:              experimental.TrapCauseIntegerDivideByZero,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeIntegerDivideByZero},
			},
		},
		{
			name:                   "compiler/integer-divide-by-zero",
			cfg:                    wazero.NewRuntimeConfigCompiler,
			supported:              platform.CompilerSupported,
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeIntegerDivideByZeroBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeIntegerDivideByZero,
			wantCause:              experimental.TrapCauseIntegerDivideByZero,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeIntegerDivideByZero},
			},
		},
		{
			name:                   "interpreter/integer-overflow",
			cfg:                    wazero.NewRuntimeConfigInterpreter,
			supported:              func() bool { return true },
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeIntegerOverflowBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeIntegerOverflow,
			wantCause:              experimental.TrapCauseIntegerOverflow,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeIntegerOverflow},
			},
		},
		{
			name:                   "compiler/integer-overflow",
			cfg:                    wazero.NewRuntimeConfigCompiler,
			supported:              platform.CompilerSupported,
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeIntegerOverflowBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeIntegerOverflow,
			wantCause:              experimental.TrapCauseIntegerOverflow,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeIntegerOverflow},
			},
		},
		{
			name:                   "interpreter/invalid-conversion-to-integer",
			cfg:                    wazero.NewRuntimeConfigInterpreter,
			supported:              func() bool { return true },
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeInvalidConversionToIntegerBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeInvalidConversionToInteger,
			wantCause:              experimental.TrapCauseInvalidConversionToInteger,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeInvalidConversionToInteger},
			},
		},
		{
			name:                   "compiler/invalid-conversion-to-integer",
			cfg:                    wazero.NewRuntimeConfigCompiler,
			supported:              platform.CompilerSupported,
			moduleBinary:           func(t *testing.T) []byte { return trapRuntimeInvalidConversionToIntegerBinary() },
			exportName:             "run",
			wantErr:                wasmruntime.ErrRuntimeInvalidConversionToInteger,
			wantCause:              experimental.TrapCauseInvalidConversionToInteger,
			wantTrapClassification: true,
			wantTrapCount:          1,
			wantEvents: []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimeInvalidConversionToInteger},
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
			name:         "interpreter/integer-divide-by-zero",
			cfg:          wazero.NewRuntimeConfigInterpreter,
			supported:    func() bool { return true },
			moduleBinary: func(t *testing.T) []byte { return trapRuntimeIntegerDivideByZeroBinary() },
			exportName:   "run",
			wantErr:      wasmruntime.ErrRuntimeIntegerDivideByZero,
			wantCause:    experimental.TrapCauseIntegerDivideByZero,
			wantSubsequence: []string{
				"listener before run",
				"listener abort run (integer_divide_by_zero)",
				"trap integer_divide_by_zero",
			},
		},
		{
			name:         "interpreter/integer-overflow",
			cfg:          wazero.NewRuntimeConfigInterpreter,
			supported:    func() bool { return true },
			moduleBinary: func(t *testing.T) []byte { return trapRuntimeIntegerOverflowBinary() },
			exportName:   "run",
			wantErr:      wasmruntime.ErrRuntimeIntegerOverflow,
			wantCause:    experimental.TrapCauseIntegerOverflow,
			wantSubsequence: []string{
				"listener before run",
				"listener abort run (integer_overflow)",
				"trap integer_overflow",
			},
		},
		{
			name:         "interpreter/invalid-conversion-to-integer",
			cfg:          wazero.NewRuntimeConfigInterpreter,
			supported:    func() bool { return true },
			moduleBinary: func(t *testing.T) []byte { return trapRuntimeInvalidConversionToIntegerBinary() },
			exportName:   "run",
			wantErr:      wasmruntime.ErrRuntimeInvalidConversionToInteger,
			wantCause:    experimental.TrapCauseInvalidConversionToInteger,
			wantSubsequence: []string{
				"listener before run",
				"listener abort run (invalid_conversion_to_integer)",
				"trap invalid_conversion_to_integer",
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
			name:         "compiler/integer-divide-by-zero",
			cfg:          wazero.NewRuntimeConfigCompiler,
			supported:    platform.CompilerSupported,
			moduleBinary: func(t *testing.T) []byte { return trapRuntimeIntegerDivideByZeroBinary() },
			exportName:   "run",
			wantErr:      wasmruntime.ErrRuntimeIntegerDivideByZero,
			wantCause:    experimental.TrapCauseIntegerDivideByZero,
			wantSubsequence: []string{
				"listener before run",
				"listener abort run (integer_divide_by_zero)",
				"trap integer_divide_by_zero",
			},
		},
		{
			name:         "compiler/integer-overflow",
			cfg:          wazero.NewRuntimeConfigCompiler,
			supported:    platform.CompilerSupported,
			moduleBinary: func(t *testing.T) []byte { return trapRuntimeIntegerOverflowBinary() },
			exportName:   "run",
			wantErr:      wasmruntime.ErrRuntimeIntegerOverflow,
			wantCause:    experimental.TrapCauseIntegerOverflow,
			wantSubsequence: []string{
				"listener before run",
				"listener abort run (integer_overflow)",
				"trap integer_overflow",
			},
		},
		{
			name:         "compiler/invalid-conversion-to-integer",
			cfg:          wazero.NewRuntimeConfigCompiler,
			supported:    platform.CompilerSupported,
			moduleBinary: func(t *testing.T) []byte { return trapRuntimeInvalidConversionToIntegerBinary() },
			exportName:   "run",
			wantErr:      wasmruntime.ErrRuntimeInvalidConversionToInteger,
			wantCause:    experimental.TrapCauseInvalidConversionToInteger,
			wantSubsequence: []string{
				"listener before run",
				"listener abort run (invalid_conversion_to_integer)",
				"trap invalid_conversion_to_integer",
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

func TestFunctionListener_MultiHostCallPolicyObserverOrdering(t *testing.T) {
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

			hostCalled := false
			instantiateListenerHostModule(t, instantiateCtx, rt, func() { hostCalled = true })
			mod := instantiateListenerGuestModule(t, instantiateCtx, rt, trapObserverTestModuleBinary())

			policyLeft := &recordingHostCallPolicyObserver{}
			policyRight := &recordingHostCallPolicyObserver{}
			trapObserver := &recordingTrapObserver{}
			callCtx := experimental.WithTrapObserver(
				experimental.WithHostCallPolicyObserver(
					experimental.WithHostCallPolicy(ctx, experimental.HostCallPolicyFunc(
						func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, "guest", caller.Name())
							require.Equal(t, "env.check", hostFunction.DebugName())
							return false
						},
					)),
					experimental.MultiHostCallPolicyObserver(
						experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
							policyLeft.ObserveHostCallPolicy(ctx, observation)
							recorder.add("policy left %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
						experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
							policyRight.ObserveHostCallPolicy(ctx, observation)
							recorder.add("policy right %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
					),
				),
				experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
					trapObserver.ObserveTrap(ctx, observation)
					recorder.add("trap %s", observation.Cause)
				}),
			)

			_, err := mod.ExportedFunction("run").Call(callCtx)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)
			require.False(t, hostCalled)

			events := listenerEvents.snapshot()
			assertListenerEvents(t, events, []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimePolicyDenied},
			})
			assertAbortTrapCauseClassification(t, events, experimental.TrapCausePolicyDenied, true)
			assertBeforeCompletionPairing(t, events)
			assertBeforeCompletionStackPairing(t, events)

			leftObservations := policyLeft.snapshot()
			rightObservations := policyRight.snapshot()
			require.Equal(t, 1, len(leftObservations))
			require.Equal(t, 1, len(rightObservations))
			require.Equal(t, leftObservations[0].Event, rightObservations[0].Event)
			require.Equal(t, leftObservations[0].Module.Name(), rightObservations[0].Module.Name())
			require.Equal(t, leftObservations[0].HostFunction.DebugName(), rightObservations[0].HostFunction.DebugName())
			require.Equal(t, experimental.HostCallPolicyEventDenied, leftObservations[0].Event)
			require.Equal(t, "guest", leftObservations[0].Module.Name())
			require.Equal(t, "env.check", leftObservations[0].HostFunction.DebugName())

			observation := trapObserver.single(t)
			require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
			require.ErrorIs(t, observation.Err, wasmruntime.ErrRuntimePolicyDenied)
			require.Equal(t, mod.Name(), observation.Module.Name())

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener before run",
				"policy left denied env.check",
				"policy right denied env.check",
				"listener abort run (policy_denied)",
				"trap policy_denied",
			})
		})
	}
}

func TestFunctionListener_MultiHostCallPolicyObserverOrderingWithMultiFunctionListener(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			hostCalled := false
			instantiateListenerHostModule(t, instantiateCtx, rt, func() { hostCalled = true })
			mod := instantiateListenerGuestModule(t, instantiateCtx, rt, trapObserverTestModuleBinary())

			policyLeft := &recordingHostCallPolicyObserver{}
			policyRight := &recordingHostCallPolicyObserver{}
			trapObserver := &recordingTrapObserver{}
			callCtx := experimental.WithTrapObserver(
				experimental.WithHostCallPolicyObserver(
					experimental.WithHostCallPolicy(ctx, experimental.HostCallPolicyFunc(
						func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, "guest", caller.Name())
							require.Equal(t, "env.check", hostFunction.DebugName())
							return false
						},
					)),
					experimental.MultiHostCallPolicyObserver(
						experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
							policyLeft.ObserveHostCallPolicy(ctx, observation)
							recorder.add("policy left %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
						experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
							policyRight.ObserveHostCallPolicy(ctx, observation)
							recorder.add("policy right %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
					),
				),
				experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
					trapObserver.ObserveTrap(ctx, observation)
					recorder.add("trap %s", observation.Cause)
				}),
			)

			_, err := mod.ExportedFunction("run").Call(callCtx)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)
			require.False(t, hostCalled)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimePolicyDenied},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertAbortTrapCauseClassification(t, events, experimental.TrapCausePolicyDenied, true)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			for _, observations := range [][]experimental.HostCallPolicyObservation{policyLeft.snapshot(), policyRight.snapshot()} {
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.HostCallPolicyEventDenied, observations[0].Event)
				require.Equal(t, "guest", observations[0].Module.Name())
				require.Equal(t, "env.check", observations[0].HostFunction.DebugName())
			}

			observation := trapObserver.single(t)
			require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
			require.ErrorIs(t, observation.Err, wasmruntime.ErrRuntimePolicyDenied)
			require.Equal(t, mod.Name(), observation.Module.Name())

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run",
				"listener right before run",
				"policy left denied env.check",
				"policy right denied env.check",
				"listener left abort run (policy_denied)",
				"listener right abort run (policy_denied)",
				"trap policy_denied",
			})
		})
	}
}

func TestFunctionListener_MultiHostCallPolicy_ComposesAllowAndDenyFlows(t *testing.T) {
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

			hostCalls := 0
			instantiateListenerHostModule(t, instantiateCtx, rt, func() { hostCalls++ })
			mod := instantiateListenerGuestModule(t, instantiateCtx, rt, trapObserverTestModuleBinary())

			_, err := mod.ExportedFunction("run").Call(
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
					experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
						trapObserver.ObserveTrap(ctx, observation)
						recorder.add("trap %s", observation.Cause)
					}),
				),
			)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)
			require.Equal(t, 1, hostCalls)

			events := listenerEvents.snapshot()
			assertListenerEvents(t, events, []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "check"},
				{phase: "after", function: "check"},
				{phase: "after", function: "run"},
				{phase: "before", function: "run"},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimePolicyDenied},
			})
			assertAbortTrapCauseClassification(t, events, experimental.TrapCausePolicyDenied, true)

			observation := trapObserver.single(t)
			require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
			require.ErrorIs(t, observation.Err, wasmruntime.ErrRuntimePolicyDenied)
			require.Equal(t, mod.Name(), observation.Module.Name())

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener before run",
				"listener before check",
				"listener after check",
				"listener after run",
				"listener before run",
				"listener abort run (policy_denied)",
				"trap policy_denied",
			})
		})
	}
}

func TestFunctionListener_MultiYieldPolicy_ComposesAllowAndDenyFlows(t *testing.T) {
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

			allowConsulted := 0
			_, err = mod.ExportedFunction("run").Call(
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
					experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
						trapObserver.ObserveTrap(ctx, observation)
						recorder.add("trap %s", observation.Cause)
					}),
				),
			)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)
			require.Equal(t, 2, denyConsulted)

			events := listenerEvents.snapshot()
			assertListenerEvents(t, events, []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run"},
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "abort", function: "async_work", errIs: wasmruntime.ErrRuntimePolicyDenied},
				{phase: "abort", function: "run", errIs: wasmruntime.ErrRuntimePolicyDenied},
			})
			assertAbortTrapCauseClassification(t, events, experimental.TrapCausePolicyDenied, true)

			observation := trapObserver.single(t)
			require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
			require.ErrorIs(t, observation.Err, wasmruntime.ErrRuntimePolicyDenied)
			require.Equal(t, mod.Name(), observation.Module.Name())

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener before run",
				"listener before async_work",
				"listener after async_work",
				"listener after run",
				"listener before run",
				"listener before async_work",
				"listener abort async_work (policy_denied)",
				"listener abort run (policy_denied)",
				"trap policy_denied",
			})
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

func TestFunctionListener_ResumeArithmeticTrapObserverOrdering(t *testing.T) {
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

					mod, err := rt.InstantiateWithConfig(instantiateCtx, resumeArithmeticTrapModuleBinary(tc.wantCause), wazero.NewModuleConfig().WithName("guest"))
					require.NoError(t, err)

					_, err = mod.ExportedFunction("run").Call(experimental.WithYielder(ctx))
					yieldErr := requireYieldError(t, err)

					assertListenerEvents(t, listenerEvents.snapshot(), []expectedListenerEvent{
						{phase: "before", function: "run"},
						{phase: "before", function: "resume_trap"},
						{phase: "before", function: "async_work"},
					})
					assertOrderedEvents(t, recorder.snapshot(), []string{
						"listener before run",
						"listener before resume_trap",
						"listener before async_work",
					})

					trapObserver := &recordingTrapObserver{}
					_, err = yieldErr.Resumer().Resume(
						experimental.WithTrapObserver(
							experimental.WithYielder(ctx),
							experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
								trapObserver.ObserveTrap(ctx, observation)
								recorder.add("trap %s", observation.Cause)
							}),
						),
						[]uint64{1},
					)
					require.ErrorIs(t, err, tc.wantErr)

					cause, ok := experimental.TrapCauseOf(err)
					require.True(t, ok)
					require.Equal(t, tc.wantCause, cause)

					events := listenerEvents.snapshot()
					assertListenerEvents(t, events, []expectedListenerEvent{
						{phase: "before", function: "run"},
						{phase: "before", function: "resume_trap"},
						{phase: "before", function: "async_work"},
						{phase: "after", function: "async_work"},
						{phase: "abort", function: "resume_trap", errIs: tc.wantErr},
						{phase: "abort", function: "run", errIs: tc.wantErr},
					})
					assertAbortTrapCauseClassification(t, events, tc.wantCause, true)
					assertBeforeCompletionPairing(t, events)
					assertBeforeCompletionStackPairing(t, events)

					observation := trapObserver.single(t)
					require.Equal(t, tc.wantCause, observation.Cause)
					require.ErrorIs(t, observation.Err, tc.wantErr)
					require.Equal(t, mod.Name(), observation.Module.Name())

					assertOrderedEvents(t, recorder.snapshot(), []string{
						"listener before run",
						"listener before resume_trap",
						"listener before async_work",
						"listener after async_work",
						fmt.Sprintf("listener abort resume_trap (%s)", tc.wantCause),
						fmt.Sprintf("listener abort run (%s)", tc.wantCause),
						fmt.Sprintf("trap %s", tc.wantCause),
					})
				})
			}
		})
	}
}

func TestFunctionListener_ResumeMultiFunctionListenerArithmeticTrapObserverOrdering(t *testing.T) {
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
					recorder := &orderedEventRecorder{}
					leftEvents := &recordingFunctionListener{}
					rightEvents := &recordingFunctionListener{}
					instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
						newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
							name:     "listener left",
							recorder: recorder,
							events:   leftEvents,
						}),
						newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
							name:     "listener right",
							recorder: recorder,
							events:   rightEvents,
						}),
					))

					rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
					defer rt.Close(ctx)

					_, err := rt.NewHostModuleBuilder("example").
						NewFunctionBuilder().
						WithGoModuleFunction(&yieldingHostFunc{t: t}, nil, []api.ValueType{api.ValueTypeI32}).
						Export("async_work").
						Instantiate(instantiateCtx)
					require.NoError(t, err)

					mod, err := rt.InstantiateWithConfig(instantiateCtx, resumeArithmeticTrapModuleBinary(tc.wantCause), wazero.NewModuleConfig().WithName("guest"))
					require.NoError(t, err)

					_, err = mod.ExportedFunction("run").Call(experimental.WithYielder(ctx))
					yieldErr := requireYieldError(t, err)

					wantInitialEvents := []expectedListenerEvent{
						{phase: "before", function: "run"},
						{phase: "before", function: "resume_trap"},
						{phase: "before", function: "async_work"},
					}
					for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
						assertListenerEvents(t, events, wantInitialEvents)
					}

					trapLeft := &recordingTrapObserver{}
					trapRight := &recordingTrapObserver{}
					_, err = yieldErr.Resumer().Resume(
						experimental.WithTrapObserver(
							experimental.WithYielder(ctx),
							experimental.MultiTrapObserver(
								experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
									trapLeft.ObserveTrap(ctx, observation)
									recorder.add("trap left %s", observation.Cause)
								}),
								experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
									trapRight.ObserveTrap(ctx, observation)
									recorder.add("trap right %s", observation.Cause)
								}),
							),
						),
						[]uint64{1},
					)
					require.ErrorIs(t, err, tc.wantErr)

					cause, ok := experimental.TrapCauseOf(err)
					require.True(t, ok)
					require.Equal(t, tc.wantCause, cause)

					wantEvents := []expectedListenerEvent{
						{phase: "before", function: "run"},
						{phase: "before", function: "resume_trap"},
						{phase: "before", function: "async_work"},
						{phase: "after", function: "async_work"},
						{phase: "abort", function: "resume_trap", errIs: tc.wantErr},
						{phase: "abort", function: "run", errIs: tc.wantErr},
					}
					for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
						assertListenerEvents(t, events, wantEvents)
						assertAbortTrapCauseClassification(t, events, tc.wantCause, true)
						assertBeforeCompletionPairing(t, events)
						assertBeforeCompletionStackPairing(t, events)
					}

					requireEquivalentTrapObservation(t, trapObservationSnapshot{
						Cause:      tc.wantCause,
						ModuleName: mod.Name(),
					}, tc.wantErr, trapLeft, trapRight)

					assertOrderedEvents(t, recorder.snapshot(), []string{
						"listener left before run",
						"listener right before run",
						"listener left before resume_trap",
						"listener right before resume_trap",
						"listener left before async_work",
						"listener right before async_work",
						"listener left after async_work",
						"listener right after async_work",
						fmt.Sprintf("listener left abort resume_trap (%s)", tc.wantCause),
						fmt.Sprintf("listener right abort resume_trap (%s)", tc.wantCause),
						fmt.Sprintf("listener left abort run (%s)", tc.wantCause),
						fmt.Sprintf("listener right abort run (%s)", tc.wantCause),
						fmt.Sprintf("trap left %s", tc.wantCause),
						fmt.Sprintf("trap right %s", tc.wantCause),
					})
				})
			}
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

			callCtx := experimental.WithYieldObserver(
				experimental.WithYielder(ctx),
				experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
					recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
				}),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			yieldErr := requireYieldError(t, err)
			assertListenerEvents(t, listenerEvents.snapshot(), []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
			})

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

			events := listenerEvents.snapshot()
			assertListenerEvents(t, events, []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run"},
			})
			assertBeforeCompletionPairing(t, events)
			assertBeforeCompletionStackPairing(t, events)

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

func TestFunctionListener_YieldObserverOrderingWithMultiFunctionListener(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			yieldLeft := &recordingYieldObserver{}
			yieldRight := &recordingYieldObserver{}
			callCtx := experimental.WithYieldObserver(
				experimental.WithYielder(ctx),
				experimental.MultiYieldObserver(
					experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
						yieldLeft.ObserveYield(ctx, observation)
						recorder.add("yield left %s #%d", observation.Event, observation.YieldCount)
					}),
					experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
						yieldRight.ObserveYield(ctx, observation)
						recorder.add("yield right %s #%d", observation.Event, observation.YieldCount)
					}),
				),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			yieldErr := requireYieldError(t, err)

			wantInitialEvents := []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantInitialEvents)
			}

			results, err := yieldErr.Resumer().Resume(
				experimental.WithYieldObserver(
					experimental.WithYielder(ctx),
					experimental.MultiYieldObserver(
						experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
							yieldLeft.ObserveYield(ctx, observation)
							recorder.add("yield left %s #%d", observation.Event, observation.YieldCount)
						}),
						experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
							yieldRight.ObserveYield(ctx, observation)
							recorder.add("yield right %s #%d", observation.Event, observation.YieldCount)
						}),
					),
				),
				[]uint64{42},
			)
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			wantYield := []yieldObservationSnapshot{
				{Event: experimental.YieldEventYielded, YieldCount: 1, ExpectedHostResults: 1},
				{Event: experimental.YieldEventResumed, YieldCount: 1, ExpectedHostResults: 1},
			}
			require.Equal(t, wantYield, snapshotYieldObservations(yieldLeft.snapshot()))
			require.Equal(t, wantYield, snapshotYieldObservations(yieldRight.snapshot()))

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run",
				"listener right before run",
				"listener left before async_work",
				"listener right before async_work",
				"yield left yielded #1",
				"yield right yielded #1",
				"yield left resumed #1",
				"yield right resumed #1",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run",
				"listener right after run",
			})
		})
	}
}

func TestFunctionListener_MultiYieldObserver_ReyieldUsesResumedObserver(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			initialLeft := &recordingYieldObserver{}
			initialRight := &recordingYieldObserver{}
			initialProvider := newObservingTimeProvider()
			_, err = mod.ExportedFunction("run_twice").Call(
				experimental.WithYieldObserver(
					experimental.WithTimeProvider(experimental.WithYielder(ctx), initialProvider),
					experimental.MultiYieldObserver(
						experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
							initialLeft.ObserveYield(ctx, observation)
							recorder.add("yield initial left %s #%d", observation.Event, observation.YieldCount)
						}),
						experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
							initialRight.ObserveYield(ctx, observation)
							recorder.add("yield initial right %s #%d", observation.Event, observation.YieldCount)
						}),
					),
				),
			)
			firstYield := requireYieldError(t, err)

			resumeLeft := &recordingYieldObserver{}
			resumeRight := &recordingYieldObserver{}
			resumeProvider := newObservingTimeProvider()
			_, err = firstYield.Resumer().Resume(
				experimental.WithYieldObserver(
					experimental.WithTimeProvider(experimental.WithYielder(ctx), resumeProvider),
					experimental.MultiYieldObserver(
						experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
							resumeLeft.ObserveYield(ctx, observation)
							recorder.add("yield resumed left %s #%d", observation.Event, observation.YieldCount)
						}),
						experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
							resumeRight.ObserveYield(ctx, observation)
							recorder.add("yield resumed right %s #%d", observation.Event, observation.YieldCount)
						}),
					),
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

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "before", function: "async_work"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
			}

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run_twice",
				"listener right before run_twice",
				"listener left before async_work",
				"listener right before async_work",
				"yield initial left yielded #1",
				"yield initial right yielded #1",
				"yield resumed left resumed #1",
				"yield resumed right resumed #1",
				"listener left after async_work",
				"listener right after async_work",
				"listener left before async_work",
				"listener right before async_work",
				"yield resumed left yielded #2",
				"yield resumed right yielded #2",
				"yield resumed left cancelled #2",
				"yield resumed right cancelled #2",
			})
		})
	}
}

func TestFunctionListener_YieldPolicyObserverAcrossResume(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			listenerEvents := &recordingFunctionListener{}
			initialObserver := &recordingYieldPolicyObserver{}
			resumeObserver := &recordingYieldPolicyObserver{}
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

			_, err = mod.ExportedFunction("run").Call(
				experimental.WithYieldObserver(
					experimental.WithYieldPolicyObserver(
						experimental.WithYieldPolicy(
							experimental.WithYielder(ctx),
							experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
								require.NotNil(t, caller)
								require.Equal(t, mod.Name(), caller.Name())
								require.Equal(t, "example.async_work", hostFunction.DebugName())
								return true
							}),
						),
						experimental.YieldPolicyObserverFunc(func(ctx context.Context, observation experimental.YieldPolicyObservation) {
							initialObserver.ObserveYieldPolicy(ctx, observation)
							recorder.add("yield policy %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
					),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
			)
			yieldErr := requireYieldError(t, err)

			assertListenerEvents(t, listenerEvents.snapshot(), []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
			})

			results, err := yieldErr.Resumer().Resume(
				experimental.WithYieldObserver(
					experimental.WithYieldPolicyObserver(
						experimental.WithYielder(ctx),
						experimental.YieldPolicyObserverFunc(func(ctx context.Context, observation experimental.YieldPolicyObservation) {
							resumeObserver.ObserveYieldPolicy(ctx, observation)
							recorder.add("resume yield policy %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
					),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
				[]uint64{42},
			)
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			events := listenerEvents.snapshot()
			assertListenerEvents(t, events, []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run"},
			})
			assertBeforeCompletionPairing(t, events)
			assertBeforeCompletionStackPairing(t, events)

			observations := initialObserver.snapshot()
			require.Equal(t, 1, len(observations))
			require.Equal(t, experimental.YieldPolicyEventAllowed, observations[0].Event)
			require.Equal(t, mod.Name(), observations[0].Module.Name())
			require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
			require.Zero(t, len(resumeObserver.snapshot()))

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener before run",
				"listener before async_work",
				"yield policy allowed example.async_work",
				"yield yielded #1",
				"yield resumed #1",
				"listener after async_work",
				"listener after run",
			})
		})
	}
}

func TestFunctionListener_ResumeYieldPolicyObserverDeniedOrdering(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			listenerEvents := &recordingFunctionListener{}
			observer := &recordingYieldPolicyObserver{}
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

			_, err = yieldErr.Resumer().Resume(
				experimental.WithYieldPolicyObserver(
					experimental.WithYieldPolicy(
						experimental.WithYielder(ctx),
						experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.NotNil(t, caller)
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return false
						}),
					),
					experimental.YieldPolicyObserverFunc(func(ctx context.Context, observation experimental.YieldPolicyObservation) {
						observer.ObserveYieldPolicy(ctx, observation)
						recorder.add("yield policy %s %s", observation.Event, observation.HostFunction.DebugName())
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

			observations := observer.snapshot()
			require.Equal(t, 1, len(observations))
			require.Equal(t, experimental.YieldPolicyEventDenied, observations[0].Event)
			require.Equal(t, mod.Name(), observations[0].Module.Name())
			require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener before run_twice",
				"listener before async_work",
				"listener after async_work",
				"listener before async_work",
				"yield policy denied example.async_work",
				"listener abort async_work (policy_denied)",
				"listener abort run_twice (policy_denied)",
			})
		})
	}
}

func TestFunctionListener_MultiYieldObserverWithMultiYieldPolicyObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			yieldLeft := &recordingYieldObserver{}
			yieldRight := &recordingYieldObserver{}
			policyLeft := &recordingYieldPolicyObserver{}
			policyRight := &recordingYieldPolicyObserver{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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
				experimental.WithYieldPolicyObserver(
					experimental.WithYieldPolicy(
						experimental.WithYielder(ctx),
						experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return true
						}),
					),
					experimental.MultiYieldPolicyObserver(
						experimental.YieldPolicyObserverFunc(func(ctx context.Context, observation experimental.YieldPolicyObservation) {
							policyLeft.ObserveYieldPolicy(ctx, observation)
							recorder.add("yield policy left %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
						experimental.YieldPolicyObserverFunc(func(ctx context.Context, observation experimental.YieldPolicyObservation) {
							policyRight.ObserveYieldPolicy(ctx, observation)
							recorder.add("yield policy right %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
					),
				),
				experimental.MultiYieldObserver(
					experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
						yieldLeft.ObserveYield(ctx, observation)
						recorder.add("yield left %s #%d", observation.Event, observation.YieldCount)
					}),
					experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
						yieldRight.ObserveYield(ctx, observation)
						recorder.add("yield right %s #%d", observation.Event, observation.YieldCount)
					}),
				),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			yieldErr := requireYieldError(t, err)

			results, err := yieldErr.Resumer().Resume(callCtx, []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			wantYield := []yieldObservationSnapshot{
				{Event: experimental.YieldEventYielded, YieldCount: 1, ExpectedHostResults: 1},
				{Event: experimental.YieldEventResumed, YieldCount: 1, ExpectedHostResults: 1},
			}
			require.Equal(t, wantYield, snapshotYieldObservations(yieldLeft.snapshot()))
			require.Equal(t, wantYield, snapshotYieldObservations(yieldRight.snapshot()))

			for _, observations := range [][]experimental.YieldPolicyObservation{policyLeft.snapshot(), policyRight.snapshot()} {
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.YieldPolicyEventAllowed, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
			}

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run",
				"listener right before run",
				"listener left before async_work",
				"listener right before async_work",
				"yield policy left allowed example.async_work",
				"yield policy right allowed example.async_work",
				"yield left yielded #1",
				"yield right yielded #1",
				"yield left resumed #1",
				"yield right resumed #1",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run",
				"listener right after run",
			})
		})
	}
}

func TestFunctionListener_MultiYieldObserverWithMultiHostCallPolicyObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			yieldLeft := &recordingYieldObserver{}
			yieldRight := &recordingYieldObserver{}
			policyLeft := &recordingHostCallPolicyObserver{}
			policyRight := &recordingHostCallPolicyObserver{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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
				experimental.WithHostCallPolicyObserver(
					experimental.WithHostCallPolicy(
						experimental.WithYielder(ctx),
						experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return true
						}),
					),
					experimental.MultiHostCallPolicyObserver(
						experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
							policyLeft.ObserveHostCallPolicy(ctx, observation)
							recorder.add("policy left %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
						experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
							policyRight.ObserveHostCallPolicy(ctx, observation)
							recorder.add("policy right %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
					),
				),
				experimental.MultiYieldObserver(
					experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
						yieldLeft.ObserveYield(ctx, observation)
						recorder.add("yield left %s #%d", observation.Event, observation.YieldCount)
					}),
					experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
						yieldRight.ObserveYield(ctx, observation)
						recorder.add("yield right %s #%d", observation.Event, observation.YieldCount)
					}),
				),
			)

			_, err = mod.ExportedFunction("run_twice").Call(callCtx)
			firstResumer := requireYieldError(t, err).Resumer()

			_, err = firstResumer.Resume(callCtx, []uint64{40})
			secondResumer := requireYieldError(t, err).Resumer()

			results, err := secondResumer.Resume(callCtx, []uint64{2})
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run_twice"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			wantYield := []yieldObservationSnapshot{
				{Event: experimental.YieldEventYielded, YieldCount: 1, ExpectedHostResults: 1},
				{Event: experimental.YieldEventResumed, YieldCount: 1, ExpectedHostResults: 1},
				{Event: experimental.YieldEventYielded, YieldCount: 2, ExpectedHostResults: 1},
				{Event: experimental.YieldEventResumed, YieldCount: 2, ExpectedHostResults: 1},
			}
			require.Equal(t, wantYield, snapshotYieldObservations(yieldLeft.snapshot()))
			require.Equal(t, wantYield, snapshotYieldObservations(yieldRight.snapshot()))

			for _, observations := range [][]experimental.HostCallPolicyObservation{policyLeft.snapshot(), policyRight.snapshot()} {
				require.Equal(t, 2, len(observations))
				require.Equal(t, experimental.HostCallPolicyEventAllowed, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
				require.Equal(t, experimental.HostCallPolicyEventAllowed, observations[1].Event)
				require.Equal(t, mod.Name(), observations[1].Module.Name())
				require.Equal(t, "example.async_work", observations[1].HostFunction.DebugName())
			}

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run_twice",
				"listener right before run_twice",
				"policy left allowed example.async_work",
				"policy right allowed example.async_work",
				"listener left before async_work",
				"listener right before async_work",
				"yield left yielded #1",
				"yield right yielded #1",
				"yield left resumed #1",
				"yield right resumed #1",
				"listener left after async_work",
				"listener right after async_work",
				"policy left allowed example.async_work",
				"policy right allowed example.async_work",
				"listener left before async_work",
				"listener right before async_work",
				"yield left yielded #2",
				"yield right yielded #2",
				"yield left resumed #2",
				"yield right resumed #2",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run_twice",
				"listener right after run_twice",
			})
		})
	}
}

func TestFunctionListener_MultiYieldObserverWithMultiTrapObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			yieldLeft := &recordingYieldObserver{}
			yieldRight := &recordingYieldObserver{}
			trapLeft := &recordingTrapObserver{}
			trapRight := &recordingTrapObserver{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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
				experimental.WithTrapObserver(
					experimental.WithYielder(ctx),
					experimental.MultiTrapObserver(trapLeft, trapRight),
				),
				experimental.MultiYieldObserver(
					experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
						yieldLeft.ObserveYield(ctx, observation)
						recorder.add("yield left %s #%d", observation.Event, observation.YieldCount)
					}),
					experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
						yieldRight.ObserveYield(ctx, observation)
						recorder.add("yield right %s #%d", observation.Event, observation.YieldCount)
					}),
				),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			yieldErr := requireYieldError(t, err)

			results, err := yieldErr.Resumer().Resume(callCtx, []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			wantYield := []yieldObservationSnapshot{
				{Event: experimental.YieldEventYielded, YieldCount: 1, ExpectedHostResults: 1},
				{Event: experimental.YieldEventResumed, YieldCount: 1, ExpectedHostResults: 1},
			}
			require.Equal(t, wantYield, snapshotYieldObservations(yieldLeft.snapshot()))
			require.Equal(t, wantYield, snapshotYieldObservations(yieldRight.snapshot()))
			require.Zero(t, trapLeft.count())
			require.Zero(t, trapRight.count())

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run",
				"listener right before run",
				"listener left before async_work",
				"listener right before async_work",
				"yield left yielded #1",
				"yield right yielded #1",
				"yield left resumed #1",
				"yield right resumed #1",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run",
				"listener right after run",
			})
		})
	}
}

func TestFunctionListener_MultiYieldPolicyObserverYieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			observerLeft := &recordingYieldPolicyObserver{}
			observerRight := &recordingYieldPolicyObserver{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			callCtx := experimental.WithYieldPolicyObserver(
				experimental.WithYieldPolicy(
					experimental.WithYielder(ctx),
					experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
						require.Equal(t, mod.Name(), caller.Name())
						require.Equal(t, "example.async_work", hostFunction.DebugName())
						return true
					}),
				),
				experimental.MultiYieldPolicyObserver(
					experimental.YieldPolicyObserverFunc(func(ctx context.Context, observation experimental.YieldPolicyObservation) {
						observerLeft.ObserveYieldPolicy(ctx, observation)
						recorder.add("yield policy left %s %s", observation.Event, observation.HostFunction.DebugName())
					}),
					experimental.YieldPolicyObserverFunc(func(ctx context.Context, observation experimental.YieldPolicyObservation) {
						observerRight.ObserveYieldPolicy(ctx, observation)
						recorder.add("yield policy right %s %s", observation.Event, observation.HostFunction.DebugName())
					}),
				),
			)

			_, err = mod.ExportedFunction("run_twice").Call(callCtx)
			firstResumer := requireYieldError(t, err).Resumer()

			_, err = firstResumer.Resume(callCtx, []uint64{40})
			secondResumer := requireYieldError(t, err).Resumer()

			results, err := secondResumer.Resume(callCtx, []uint64{2})
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run_twice"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			for _, observations := range [][]experimental.YieldPolicyObservation{observerLeft.snapshot(), observerRight.snapshot()} {
				require.Equal(t, 2, len(observations))
				require.Equal(t, experimental.YieldPolicyEventAllowed, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
				require.Equal(t, experimental.YieldPolicyEventAllowed, observations[1].Event)
				require.Equal(t, mod.Name(), observations[1].Module.Name())
				require.Equal(t, "example.async_work", observations[1].HostFunction.DebugName())
			}

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run_twice",
				"listener right before run_twice",
				"listener left before async_work",
				"listener right before async_work",
				"yield policy left allowed example.async_work",
				"yield policy right allowed example.async_work",
				"listener left after async_work",
				"listener right after async_work",
				"listener left before async_work",
				"listener right before async_work",
				"yield policy left allowed example.async_work",
				"yield policy right allowed example.async_work",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run_twice",
				"listener right after run_twice",
			})
		})
	}
}

func TestFunctionListener_ResumeMultiYieldPolicyObserverDeniedOrdering(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			observerLeft := &recordingYieldPolicyObserver{}
			observerRight := &recordingYieldPolicyObserver{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			_, err = yieldErr.Resumer().Resume(
				experimental.WithYieldPolicyObserver(
					experimental.WithYieldPolicy(
						experimental.WithYielder(ctx),
						experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.NotNil(t, caller)
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return false
						}),
					),
					experimental.MultiYieldPolicyObserver(
						experimental.YieldPolicyObserverFunc(func(ctx context.Context, observation experimental.YieldPolicyObservation) {
							observerLeft.ObserveYieldPolicy(ctx, observation)
							recorder.add("yield policy left %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
						experimental.YieldPolicyObserverFunc(func(ctx context.Context, observation experimental.YieldPolicyObservation) {
							observerRight.ObserveYieldPolicy(ctx, observation)
							recorder.add("yield policy right %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
					),
				),
				[]uint64{1},
			)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)

			cause, ok := experimental.TrapCauseOf(err)
			require.True(t, ok)
			require.Equal(t, experimental.TrapCausePolicyDenied, cause)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "before", function: "async_work"},
				{phase: "abort", function: "async_work", errIs: wasmruntime.ErrRuntimePolicyDenied},
				{phase: "abort", function: "run_twice", errIs: wasmruntime.ErrRuntimePolicyDenied},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertAbortTrapCauseClassification(t, events, experimental.TrapCausePolicyDenied, true)
				assertBeforeCompletionStackPairing(t, events)
			}

			for _, observations := range [][]experimental.YieldPolicyObservation{observerLeft.snapshot(), observerRight.snapshot()} {
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.YieldPolicyEventDenied, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
			}

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run_twice",
				"listener right before run_twice",
				"listener left before async_work",
				"listener right before async_work",
				"listener left after async_work",
				"listener right after async_work",
				"listener left before async_work",
				"listener right before async_work",
				"yield policy left denied example.async_work",
				"yield policy right denied example.async_work",
				"listener left abort async_work (policy_denied)",
				"listener right abort async_work (policy_denied)",
				"listener left abort run_twice (policy_denied)",
				"listener right abort run_twice (policy_denied)",
			})
		})
	}
}

func TestFunctionListener_MultiFunctionListenerFactoryWithMultiTrapObserverYieldResume(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			trapLeft := &recordingTrapObserver{}
			trapRight := &recordingTrapObserver{}
			_, err = yieldErr.Resumer().Resume(
				experimental.WithTrapObserver(
					experimental.WithYieldPolicy(
						experimental.WithYielder(ctx),
						experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.NotNil(t, caller)
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return false
						}),
					),
					experimental.MultiTrapObserver(
						experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
							trapLeft.ObserveTrap(ctx, observation)
							recorder.add("trap left %s", observation.Cause)
						}),
						experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
							trapRight.ObserveTrap(ctx, observation)
							recorder.add("trap right %s", observation.Cause)
						}),
					),
				),
				[]uint64{1},
			)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)

			cause, ok := experimental.TrapCauseOf(err)
			require.True(t, ok)
			require.Equal(t, experimental.TrapCausePolicyDenied, cause)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "before", function: "async_work"},
				{phase: "abort", function: "async_work", errIs: wasmruntime.ErrRuntimePolicyDenied},
				{phase: "abort", function: "run_twice", errIs: wasmruntime.ErrRuntimePolicyDenied},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertAbortTrapCauseClassification(t, events, experimental.TrapCausePolicyDenied, true)
				assertBeforeCompletionStackPairing(t, events)
			}

			requireEquivalentTrapObservation(t, trapObservationSnapshot{
				Cause:        experimental.TrapCausePolicyDenied,
				ModuleName:   mod.Name(),
				PolicyDenied: true,
			}, wasmruntime.ErrRuntimePolicyDenied, trapLeft, trapRight)

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run_twice",
				"listener right before run_twice",
				"listener left before async_work",
				"listener right before async_work",
				"listener left after async_work",
				"listener right after async_work",
				"listener left before async_work",
				"listener right before async_work",
				"listener left abort async_work (policy_denied)",
				"listener right abort async_work (policy_denied)",
				"listener left abort run_twice (policy_denied)",
				"listener right abort run_twice (policy_denied)",
				"trap left policy_denied",
				"trap right policy_denied",
			})
		})
	}
}

func TestFunctionListener_MultiTrapObserver_YieldResumeUsesResumedObserver(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			initialLeft := &recordingTrapObserver{}
			initialRight := &recordingTrapObserver{}
			_, err = mod.ExportedFunction("run_twice").Call(
				experimental.WithTrapObserver(
					experimental.WithYielder(ctx),
					experimental.MultiTrapObserver(initialLeft, initialRight),
				),
			)
			firstYield := requireYieldError(t, err)

			resumeLeft := &recordingTrapObserver{}
			resumeRight := &recordingTrapObserver{}
			_, err = firstYield.Resumer().Resume(
				experimental.WithTrapObserver(
					experimental.WithHostCallPolicy(
						experimental.WithYielder(ctx),
						experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return false
						}),
					),
					experimental.MultiTrapObserver(
						experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
							resumeLeft.ObserveTrap(ctx, observation)
							recorder.add("trap left %s", observation.Cause)
						}),
						experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
							resumeRight.ObserveTrap(ctx, observation)
							recorder.add("trap right %s", observation.Cause)
						}),
					),
				),
				[]uint64{1},
			)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)

			cause, ok := experimental.TrapCauseOf(err)
			require.True(t, ok)
			require.Equal(t, experimental.TrapCausePolicyDenied, cause)

			require.Zero(t, initialLeft.count())
			require.Zero(t, initialRight.count())

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "abort", function: "run_twice", errIs: wasmruntime.ErrRuntimePolicyDenied},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertAbortTrapCauseClassification(t, events, experimental.TrapCausePolicyDenied, true)
				assertBeforeCompletionStackPairing(t, events)
			}

			requireEquivalentTrapObservation(t, trapObservationSnapshot{
				Cause:        experimental.TrapCausePolicyDenied,
				ModuleName:   mod.Name(),
				PolicyDenied: true,
			}, wasmruntime.ErrRuntimePolicyDenied, resumeLeft, resumeRight)

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run_twice",
				"listener right before run_twice",
				"listener left before async_work",
				"listener right before async_work",
				"listener left after async_work",
				"listener right after async_work",
				"listener left abort run_twice (policy_denied)",
				"listener right abort run_twice (policy_denied)",
				"trap left policy_denied",
				"trap right policy_denied",
			})
		})
	}
}

func TestFunctionListener_ResumeMultiHostCallPolicyObserverDeniedOrdering(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			initialLeft := &recordingHostCallPolicyObserver{}
			initialRight := &recordingHostCallPolicyObserver{}
			resumeLeft := &recordingHostCallPolicyObserver{}
			resumeRight := &recordingHostCallPolicyObserver{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			_, err = mod.ExportedFunction("run_twice").Call(
				experimental.WithHostCallPolicyObserver(
					experimental.WithHostCallPolicy(
						experimental.WithYielder(ctx),
						experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return true
						}),
					),
					experimental.MultiHostCallPolicyObserver(
						experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
							initialLeft.ObserveHostCallPolicy(ctx, observation)
							recorder.add("policy left %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
						experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
							initialRight.ObserveHostCallPolicy(ctx, observation)
							recorder.add("policy right %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
					),
				),
			)
			yieldErr := requireYieldError(t, err)

			trapObserver := &recordingTrapObserver{}
			_, err = yieldErr.Resumer().Resume(
				experimental.WithTrapObserver(
					experimental.WithHostCallPolicyObserver(
						experimental.WithHostCallPolicy(
							experimental.WithYielder(ctx),
							experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
								require.Equal(t, mod.Name(), caller.Name())
								require.Equal(t, "example.async_work", hostFunction.DebugName())
								return false
							}),
						),
						experimental.MultiHostCallPolicyObserver(
							experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
								resumeLeft.ObserveHostCallPolicy(ctx, observation)
								recorder.add("resume policy left %s %s", observation.Event, observation.HostFunction.DebugName())
							}),
							experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
								resumeRight.ObserveHostCallPolicy(ctx, observation)
								recorder.add("resume policy right %s %s", observation.Event, observation.HostFunction.DebugName())
							}),
						),
					),
					experimental.TrapObserverFunc(func(ctx context.Context, observation experimental.TrapObservation) {
						trapObserver.ObserveTrap(ctx, observation)
						recorder.add("trap %s", observation.Cause)
					}),
				),
				[]uint64{40},
			)
			require.ErrorIs(t, err, wasmruntime.ErrRuntimePolicyDenied)

			cause, ok := experimental.TrapCauseOf(err)
			require.True(t, ok)
			require.Equal(t, experimental.TrapCausePolicyDenied, cause)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "abort", function: "run_twice", errIs: wasmruntime.ErrRuntimePolicyDenied},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertAbortTrapCauseClassification(t, events, experimental.TrapCausePolicyDenied, true)
				assertBeforeCompletionStackPairing(t, events)
			}

			for _, observations := range [][]experimental.HostCallPolicyObservation{initialLeft.snapshot(), initialRight.snapshot()} {
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.HostCallPolicyEventAllowed, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
			}

			for _, observations := range [][]experimental.HostCallPolicyObservation{resumeLeft.snapshot(), resumeRight.snapshot()} {
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.HostCallPolicyEventDenied, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
			}

			observation := trapObserver.single(t)
			require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
			require.ErrorIs(t, observation.Err, wasmruntime.ErrRuntimePolicyDenied)
			require.Equal(t, mod.Name(), observation.Module.Name())

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run_twice",
				"listener right before run_twice",
				"policy left allowed example.async_work",
				"policy right allowed example.async_work",
				"listener left before async_work",
				"listener right before async_work",
				"listener left after async_work",
				"listener right after async_work",
				"resume policy left denied example.async_work",
				"resume policy right denied example.async_work",
				"listener left abort run_twice (policy_denied)",
				"listener right abort run_twice (policy_denied)",
				"trap policy_denied",
			})
		})
	}
}

func TestFunctionListener_MultiHostCallPolicyObserverYieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			observerLeft := &recordingHostCallPolicyObserver{}
			observerRight := &recordingHostCallPolicyObserver{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			callCtx := experimental.WithHostCallPolicyObserver(
				experimental.WithHostCallPolicy(
					experimental.WithYielder(ctx),
					experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
						require.Equal(t, mod.Name(), caller.Name())
						require.Equal(t, "example.async_work", hostFunction.DebugName())
						return true
					}),
				),
				experimental.MultiHostCallPolicyObserver(
					experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
						observerLeft.ObserveHostCallPolicy(ctx, observation)
						recorder.add("policy left %s %s", observation.Event, observation.HostFunction.DebugName())
					}),
					experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
						observerRight.ObserveHostCallPolicy(ctx, observation)
						recorder.add("policy right %s %s", observation.Event, observation.HostFunction.DebugName())
					}),
				),
			)

			_, err = mod.ExportedFunction("run_twice").Call(callCtx)
			firstResumer := requireYieldError(t, err).Resumer()

			_, err = firstResumer.Resume(callCtx, []uint64{40})
			secondResumer := requireYieldError(t, err).Resumer()

			results, err := secondResumer.Resume(callCtx, []uint64{2})
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run_twice"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			for _, observations := range [][]experimental.HostCallPolicyObservation{observerLeft.snapshot(), observerRight.snapshot()} {
				require.Equal(t, 2, len(observations))
				require.Equal(t, experimental.HostCallPolicyEventAllowed, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
				require.Equal(t, experimental.HostCallPolicyEventAllowed, observations[1].Event)
				require.Equal(t, mod.Name(), observations[1].Module.Name())
				require.Equal(t, "example.async_work", observations[1].HostFunction.DebugName())
			}

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"listener left before run_twice",
				"listener right before run_twice",
				"policy left allowed example.async_work",
				"policy right allowed example.async_work",
				"listener left before async_work",
				"listener right before async_work",
				"listener left after async_work",
				"listener right after async_work",
				"policy left allowed example.async_work",
				"policy right allowed example.async_work",
				"listener left before async_work",
				"listener right before async_work",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run_twice",
				"listener right after run_twice",
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

func TestFunctionListener_FuelObserverAcrossYieldResume(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			listenerEvents := &recordingFunctionListener{}
			host := &fuelAdjustingYieldingHostFunc{t: t, adjustment: 5}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, newFunctionListenerFactory(&orderedRecordingFunctionListener{
				recorder: recorder,
				events:   listenerEvents,
			}))

			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			_, err := rt.NewHostModuleBuilder("example").
				NewFunctionBuilder().
				WithGoModuleFunction(host, nil, []api.ValueType{api.ValueTypeI32}).
				Export("async_work").
				Instantiate(instantiateCtx)
			require.NoError(t, err)

			mod, err := rt.InstantiateWithConfig(instantiateCtx, yieldWasm, wazero.NewModuleConfig().WithName("guest"))
			require.NoError(t, err)

			initialObserver := &orderedRecordingFuelObserver{
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			ctrl := experimental.NewSimpleFuelController(20)

			_, err = mod.ExportedFunction("run_twice").Call(
				experimental.WithYieldObserver(
					experimental.WithFuelObserver(
						experimental.WithFuelController(
							experimental.WithYielder(ctx),
							ctrl,
						),
						initialObserver,
					),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
			)
			firstYield := requireYieldError(t, err)

			assertListenerEvents(t, listenerEvents.snapshot(), []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
			})

			resumeObserver := &orderedRecordingFuelObserver{
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			host.expectedFuelObserver = resumeObserver

			results, err := firstYield.Resumer().Resume(
				experimental.WithYieldObserver(
					experimental.WithFuelObserver(experimental.WithYielder(ctx), resumeObserver),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
				[]uint64{40},
			)
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)
			require.Equal(t, host.beforeAdjustment+int64(5), host.afterAdjustment)

			events := listenerEvents.snapshot()
			assertListenerEvents(t, events, []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run_twice"},
			})
			assertBeforeCompletionPairing(t, events)
			assertBeforeCompletionStackPairing(t, events)

			initial := initialObserver.observations.snapshot()
			require.Equal(t, 1, len(initial))
			require.Equal(t, experimental.FuelEventBudgeted, initial[0].Event)
			require.Equal(t, int64(20), initial[0].Budget)
			require.Equal(t, int64(20), initial[0].Remaining)

			resumed := resumeObserver.observations.snapshot()
			require.Equal(t, 2, len(resumed))
			require.Equal(t, experimental.FuelEventRecharged, resumed[0].Event)
			require.Equal(t, int64(5), resumed[0].Delta)
			require.Equal(t, host.afterAdjustment, resumed[0].Remaining)
			require.Equal(t, experimental.FuelEventConsumed, resumed[1].Event)
			require.Equal(t, int64(25), resumed[1].Budget)
			require.Equal(t, resumed[1].Consumed, ctrl.TotalConsumed())
			require.Equal(t, resumed[1].Budget-resumed[1].Consumed, resumed[1].Remaining)

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"fuel budgeted",
				"listener before run_twice",
				"listener before async_work",
				"yield yielded #1",
				"yield resumed #1",
				"listener after async_work",
				"listener before async_work",
				"fuel recharged",
				"listener after async_work",
				"listener after run_twice",
				"fuel consumed",
			})
		})
	}
}

func TestFunctionListener_MultiFuelObserverOrderingAcrossYieldResume(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			host := &fuelAdjustingYieldingHostFunc{t: t, adjustment: 5}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			_, err := rt.NewHostModuleBuilder("example").
				NewFunctionBuilder().
				WithGoModuleFunction(host, nil, []api.ValueType{api.ValueTypeI32}).
				Export("async_work").
				Instantiate(instantiateCtx)
			require.NoError(t, err)

			mod, err := rt.InstantiateWithConfig(instantiateCtx, yieldWasm, wazero.NewModuleConfig().WithName("guest"))
			require.NoError(t, err)

			initialLeft := &namedOrderedRecordingFuelObserver{
				name:         "fuel left",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			initialRight := &namedOrderedRecordingFuelObserver{
				name:         "fuel right",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			ctrl := experimental.NewSimpleFuelController(20)

			_, err = mod.ExportedFunction("run_twice").Call(
				experimental.WithYieldObserver(
					experimental.WithFuelObserver(
						experimental.WithFuelController(
							experimental.WithYielder(ctx),
							ctrl,
						),
						experimental.MultiFuelObserver(initialLeft, initialRight),
					),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
			)
			firstYield := requireYieldError(t, err)

			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, []expectedListenerEvent{
					{phase: "before", function: "run_twice"},
					{phase: "before", function: "async_work"},
				})
			}

			resumeLeft := &namedOrderedRecordingFuelObserver{
				name:         "fuel left",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			resumeRight := &namedOrderedRecordingFuelObserver{
				name:         "fuel right",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			resumeMulti := &forwardingFuelObserver{inner: experimental.MultiFuelObserver(resumeLeft, resumeRight)}
			host.expectedFuelObserver = resumeMulti

			results, err := firstYield.Resumer().Resume(
				experimental.WithYieldObserver(
					experimental.WithFuelObserver(experimental.WithYielder(ctx), resumeMulti),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
				[]uint64{40},
			)
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)
			require.Equal(t, host.beforeAdjustment+int64(5), host.afterAdjustment)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run_twice"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

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

			for _, observer := range []*namedOrderedRecordingFuelObserver{initialLeft, initialRight} {
				require.Equal(t, wantInitial, snapshotFuelObservations(observer.observations.snapshot()))
			}
			for _, observer := range []*namedOrderedRecordingFuelObserver{resumeLeft, resumeRight} {
				require.Equal(t, wantResumed, snapshotFuelObservations(observer.observations.snapshot()))
			}

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"fuel left budgeted",
				"fuel right budgeted",
				"listener left before run_twice",
				"listener right before run_twice",
				"listener left before async_work",
				"listener right before async_work",
				"yield yielded #1",
				"yield resumed #1",
				"listener left after async_work",
				"listener right after async_work",
				"listener left before async_work",
				"listener right before async_work",
				"fuel left recharged",
				"fuel right recharged",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run_twice",
				"listener right after run_twice",
				"fuel left consumed",
				"fuel right consumed",
			})
		})
	}
}

func TestFunctionListener_MultiFuelObserverYieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			fuelLeft := &namedOrderedRecordingFuelObserver{
				name:         "fuel left",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			fuelRight := &namedOrderedRecordingFuelObserver{
				name:         "fuel right",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			ctrl := experimental.NewSimpleFuelController(20)
			callCtx := experimental.WithYieldObserver(
				experimental.WithFuelObserver(
					experimental.WithFuelController(
						experimental.WithYielder(ctx),
						ctrl,
					),
					experimental.MultiFuelObserver(fuelLeft, fuelRight),
				),
				experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
					recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
				}),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			yieldErr := requireYieldError(t, err)

			results, err := yieldErr.Resumer().Resume(experimental.WithYielder(ctx), []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			wantFuel := []fuelObservationSnapshot{
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
			for _, observer := range []*namedOrderedRecordingFuelObserver{fuelLeft, fuelRight} {
				require.Equal(t, wantFuel, snapshotFuelObservations(observer.observations.snapshot()))
			}

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"fuel left budgeted",
				"fuel right budgeted",
				"listener left before run",
				"listener right before run",
				"listener left before async_work",
				"listener right before async_work",
				"yield yielded #1",
				"yield resumed #1",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run",
				"listener right after run",
				"fuel left consumed",
				"fuel right consumed",
			})
		})
	}
}

func TestFunctionListener_MultiYieldObserverWithMultiFuelObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			yieldLeft := &recordingYieldObserver{}
			yieldRight := &recordingYieldObserver{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			fuelLeft := &namedOrderedRecordingFuelObserver{
				name:         "fuel left",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			fuelRight := &namedOrderedRecordingFuelObserver{
				name:         "fuel right",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			ctrl := experimental.NewSimpleFuelController(20)
			callCtx := experimental.WithYieldObserver(
				experimental.WithFuelObserver(
					experimental.WithFuelController(
						experimental.WithYielder(ctx),
						ctrl,
					),
					experimental.MultiFuelObserver(fuelLeft, fuelRight),
				),
				experimental.MultiYieldObserver(
					experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
						yieldLeft.ObserveYield(ctx, observation)
						recorder.add("yield left %s #%d", observation.Event, observation.YieldCount)
					}),
					experimental.YieldObserverFunc(func(ctx context.Context, observation experimental.YieldObservation) {
						yieldRight.ObserveYield(ctx, observation)
						recorder.add("yield right %s #%d", observation.Event, observation.YieldCount)
					}),
				),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			yieldErr := requireYieldError(t, err)

			results, err := yieldErr.Resumer().Resume(callCtx, []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			wantYield := []yieldObservationSnapshot{
				{Event: experimental.YieldEventYielded, YieldCount: 1, ExpectedHostResults: 1},
				{Event: experimental.YieldEventResumed, YieldCount: 1, ExpectedHostResults: 1},
			}
			require.Equal(t, wantYield, snapshotYieldObservations(yieldLeft.snapshot()))
			require.Equal(t, wantYield, snapshotYieldObservations(yieldRight.snapshot()))

			wantFuel := []fuelObservationSnapshot{
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
			for _, observer := range []*namedOrderedRecordingFuelObserver{fuelLeft, fuelRight} {
				require.Equal(t, wantFuel, snapshotFuelObservations(observer.observations.snapshot()))
			}

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"fuel left budgeted",
				"fuel right budgeted",
				"listener left before run",
				"listener right before run",
				"listener left before async_work",
				"listener right before async_work",
				"yield left yielded #1",
				"yield right yielded #1",
				"yield left resumed #1",
				"yield right resumed #1",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run",
				"listener right after run",
				"fuel left consumed",
				"fuel right consumed",
			})
		})
	}
}

func TestFunctionListener_MultiFuelObserverWithMultiTrapObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			fuelLeft := &namedOrderedRecordingFuelObserver{
				name:         "fuel left",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			fuelRight := &namedOrderedRecordingFuelObserver{
				name:         "fuel right",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			trapLeft := &recordingTrapObserver{}
			trapRight := &recordingTrapObserver{}
			ctrl := experimental.NewSimpleFuelController(20)
			callCtx := experimental.WithYieldObserver(
				experimental.WithTrapObserver(
					experimental.WithFuelObserver(
						experimental.WithFuelController(
							experimental.WithYielder(ctx),
							ctrl,
						),
						experimental.MultiFuelObserver(fuelLeft, fuelRight),
					),
					experimental.MultiTrapObserver(trapLeft, trapRight),
				),
				experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
					recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
				}),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			yieldErr := requireYieldError(t, err)

			results, err := yieldErr.Resumer().Resume(callCtx, []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			wantFuel := []fuelObservationSnapshot{
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
			for _, observer := range []*namedOrderedRecordingFuelObserver{fuelLeft, fuelRight} {
				require.Equal(t, wantFuel, snapshotFuelObservations(observer.observations.snapshot()))
			}
			require.Zero(t, trapLeft.count())
			require.Zero(t, trapRight.count())

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"fuel left budgeted",
				"fuel right budgeted",
				"listener left before run",
				"listener right before run",
				"listener left before async_work",
				"listener right before async_work",
				"yield yielded #1",
				"yield resumed #1",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run",
				"listener right after run",
				"fuel left consumed",
				"fuel right consumed",
			})
		})
	}
}

func TestFunctionListener_MultiFuelObserverWithMultiYieldPolicyObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			fuelLeft := &namedOrderedRecordingFuelObserver{
				name:         "fuel left",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			fuelRight := &namedOrderedRecordingFuelObserver{
				name:         "fuel right",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			policyLeft := &recordingYieldPolicyObserver{}
			policyRight := &recordingYieldPolicyObserver{}
			ctrl := experimental.NewSimpleFuelController(20)
			callCtx := experimental.WithYieldObserver(
				experimental.WithYieldPolicyObserver(
					experimental.WithYieldPolicy(
						experimental.WithFuelObserver(
							experimental.WithFuelController(
								experimental.WithYielder(ctx),
								ctrl,
							),
							experimental.MultiFuelObserver(fuelLeft, fuelRight),
						),
						experimental.YieldPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return true
						}),
					),
					experimental.MultiYieldPolicyObserver(
						experimental.YieldPolicyObserverFunc(func(ctx context.Context, observation experimental.YieldPolicyObservation) {
							policyLeft.ObserveYieldPolicy(ctx, observation)
							recorder.add("yield policy left %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
						experimental.YieldPolicyObserverFunc(func(ctx context.Context, observation experimental.YieldPolicyObservation) {
							policyRight.ObserveYieldPolicy(ctx, observation)
							recorder.add("yield policy right %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
					),
				),
				experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
					recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
				}),
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			yieldErr := requireYieldError(t, err)

			results, err := yieldErr.Resumer().Resume(callCtx, []uint64{42})
			require.NoError(t, err)
			require.Equal(t, []uint64{142}, results)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			wantFuel := []fuelObservationSnapshot{
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
			for _, observer := range []*namedOrderedRecordingFuelObserver{fuelLeft, fuelRight} {
				require.Equal(t, wantFuel, snapshotFuelObservations(observer.observations.snapshot()))
			}

			for _, observations := range [][]experimental.YieldPolicyObservation{policyLeft.snapshot(), policyRight.snapshot()} {
				require.Equal(t, 1, len(observations))
				require.Equal(t, experimental.YieldPolicyEventAllowed, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
			}

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"fuel left budgeted",
				"fuel right budgeted",
				"listener left before run",
				"listener right before run",
				"listener left before async_work",
				"listener right before async_work",
				"yield policy left allowed example.async_work",
				"yield policy right allowed example.async_work",
				"yield yielded #1",
				"yield resumed #1",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run",
				"listener right after run",
				"fuel left consumed",
				"fuel right consumed",
			})
		})
	}
}

func TestFunctionListener_MultiFuelObserverWithMultiHostCallPolicyObserver_YieldResumeLifecycle(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			fuelLeft := &namedOrderedRecordingFuelObserver{
				name:         "fuel left",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			fuelRight := &namedOrderedRecordingFuelObserver{
				name:         "fuel right",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			policyLeft := &recordingHostCallPolicyObserver{}
			policyRight := &recordingHostCallPolicyObserver{}
			ctrl := experimental.NewSimpleFuelController(20)
			callCtx := experimental.WithYieldObserver(
				experimental.WithHostCallPolicyObserver(
					experimental.WithHostCallPolicy(
						experimental.WithFuelObserver(
							experimental.WithFuelController(
								experimental.WithYielder(ctx),
								ctrl,
							),
							experimental.MultiFuelObserver(fuelLeft, fuelRight),
						),
						experimental.HostCallPolicyFunc(func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
							require.Equal(t, mod.Name(), caller.Name())
							require.Equal(t, "example.async_work", hostFunction.DebugName())
							return true
						}),
					),
					experimental.MultiHostCallPolicyObserver(
						experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
							policyLeft.ObserveHostCallPolicy(ctx, observation)
							recorder.add("policy left %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
						experimental.HostCallPolicyObserverFunc(func(ctx context.Context, observation experimental.HostCallPolicyObservation) {
							policyRight.ObserveHostCallPolicy(ctx, observation)
							recorder.add("policy right %s %s", observation.Event, observation.HostFunction.DebugName())
						}),
					),
				),
				experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
					recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
				}),
			)

			_, err = mod.ExportedFunction("run_twice").Call(callCtx)
			firstResumer := requireYieldError(t, err).Resumer()

			_, err = firstResumer.Resume(callCtx, []uint64{40})
			secondResumer := requireYieldError(t, err).Resumer()

			results, err := secondResumer.Resume(callCtx, []uint64{2})
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run_twice"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			wantFuel := []fuelObservationSnapshot{
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
			for _, observer := range []*namedOrderedRecordingFuelObserver{fuelLeft, fuelRight} {
				require.Equal(t, wantFuel, snapshotFuelObservations(observer.observations.snapshot()))
			}

			for _, observations := range [][]experimental.HostCallPolicyObservation{policyLeft.snapshot(), policyRight.snapshot()} {
				require.Equal(t, 2, len(observations))
				require.Equal(t, experimental.HostCallPolicyEventAllowed, observations[0].Event)
				require.Equal(t, mod.Name(), observations[0].Module.Name())
				require.Equal(t, "example.async_work", observations[0].HostFunction.DebugName())
				require.Equal(t, experimental.HostCallPolicyEventAllowed, observations[1].Event)
				require.Equal(t, mod.Name(), observations[1].Module.Name())
				require.Equal(t, "example.async_work", observations[1].HostFunction.DebugName())
			}

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"fuel left budgeted",
				"fuel right budgeted",
				"listener left before run_twice",
				"listener right before run_twice",
				"policy left allowed example.async_work",
				"policy right allowed example.async_work",
				"listener left before async_work",
				"listener right before async_work",
				"yield yielded #1",
				"yield resumed #1",
				"listener left after async_work",
				"listener right after async_work",
				"policy left allowed example.async_work",
				"policy right allowed example.async_work",
				"listener left before async_work",
				"listener right before async_work",
				"yield yielded #2",
				"yield resumed #2",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run_twice",
				"listener right after run_twice",
				"fuel left consumed",
				"fuel right consumed",
			})
		})
	}
}

func TestFunctionListener_MultiFuelObserver_YieldResumeUsesResumedObserver(t *testing.T) {
	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			recorder := &orderedEventRecorder{}
			leftEvents := &recordingFunctionListener{}
			rightEvents := &recordingFunctionListener{}
			instantiateCtx := experimental.WithFunctionListenerFactory(ctx, experimental.MultiFunctionListenerFactory(
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener left",
					recorder: recorder,
					events:   leftEvents,
				}),
				newFunctionListenerFactory(&namedOrderedRecordingFunctionListener{
					name:     "listener right",
					recorder: recorder,
					events:   rightEvents,
				}),
			))

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

			initialLeft := &namedOrderedRecordingFuelObserver{
				name:         "fuel initial left",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			initialRight := &namedOrderedRecordingFuelObserver{
				name:         "fuel initial right",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			ctrl := experimental.NewSimpleFuelController(20)

			_, err = mod.ExportedFunction("run_twice").Call(
				experimental.WithYieldObserver(
					experimental.WithFuelObserver(
						experimental.WithFuelController(
							experimental.WithYielder(ctx),
							ctrl,
						),
						experimental.MultiFuelObserver(initialLeft, initialRight),
					),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
			)
			firstYield := requireYieldError(t, err)

			resumeLeft := &namedOrderedRecordingFuelObserver{
				name:         "fuel resume left",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			resumeRight := &namedOrderedRecordingFuelObserver{
				name:         "fuel resume right",
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}

			_, err = firstYield.Resumer().Resume(
				experimental.WithYieldObserver(
					experimental.WithFuelObserver(
						experimental.WithYielder(ctx),
						experimental.MultiFuelObserver(resumeLeft, resumeRight),
					),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
				[]uint64{40},
			)
			secondYield := requireYieldError(t, err)
			require.Zero(t, len(resumeLeft.observations.snapshot()))
			require.Zero(t, len(resumeRight.observations.snapshot()))

			results, err := secondYield.Resumer().Resume(
				experimental.WithYieldObserver(
					experimental.WithYielder(ctx),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
				[]uint64{2},
			)
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)

			wantEvents := []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run_twice"},
			}
			for _, events := range [][]listenerEvent{leftEvents.snapshot(), rightEvents.snapshot()} {
				assertListenerEvents(t, events, wantEvents)
				assertBeforeCompletionPairing(t, events)
				assertBeforeCompletionStackPairing(t, events)
			}

			wantInitial := []fuelObservationSnapshot{{
				Event:     experimental.FuelEventBudgeted,
				Budget:    20,
				Remaining: 20,
			}}
			for _, observer := range []*namedOrderedRecordingFuelObserver{initialLeft, initialRight} {
				require.Equal(t, wantInitial, snapshotFuelObservations(observer.observations.snapshot()))
			}

			wantResumed := []fuelObservationSnapshot{{
				Event:     experimental.FuelEventConsumed,
				Budget:    20,
				Consumed:  ctrl.TotalConsumed(),
				Remaining: 20 - ctrl.TotalConsumed(),
			}}
			for _, observer := range []*namedOrderedRecordingFuelObserver{resumeLeft, resumeRight} {
				require.Equal(t, wantResumed, snapshotFuelObservations(observer.observations.snapshot()))
			}

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"fuel initial left budgeted",
				"fuel initial right budgeted",
				"listener left before run_twice",
				"listener right before run_twice",
				"listener left before async_work",
				"listener right before async_work",
				"yield yielded #1",
				"yield resumed #1",
				"listener left after async_work",
				"listener right after async_work",
				"listener left before async_work",
				"listener right before async_work",
				"yield yielded #2",
				"yield resumed #2",
				"listener left after async_work",
				"listener right after async_work",
				"listener left after run_twice",
				"listener right after run_twice",
				"fuel resume left consumed",
				"fuel resume right consumed",
			})
		})
	}
}

func TestFunctionListener_FuelObserverAcrossYieldReyield(t *testing.T) {
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

			initialObserver := &orderedRecordingFuelObserver{
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}
			ctrl := experimental.NewSimpleFuelController(20)

			_, err = mod.ExportedFunction("run_twice").Call(
				experimental.WithYieldObserver(
					experimental.WithFuelObserver(
						experimental.WithFuelController(
							experimental.WithYielder(ctx),
							ctrl,
						),
						initialObserver,
					),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
			)
			firstYield := requireYieldError(t, err)

			resumeObserver := &orderedRecordingFuelObserver{
				recorder:     recorder,
				observations: &recordingFuelObserver{},
			}

			_, err = firstYield.Resumer().Resume(
				experimental.WithYieldObserver(
					experimental.WithFuelObserver(experimental.WithYielder(ctx), resumeObserver),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
				[]uint64{40},
			)
			secondYield := requireYieldError(t, err)
			require.Zero(t, len(resumeObserver.observations.snapshot()))

			results, err := secondYield.Resumer().Resume(
				experimental.WithYieldObserver(
					experimental.WithYielder(ctx),
					experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
						recorder.add("yield %s #%d", observation.Event, observation.YieldCount)
					}),
				),
				[]uint64{2},
			)
			require.NoError(t, err)
			require.Equal(t, []uint64{42}, results)

			events := listenerEvents.snapshot()
			assertListenerEvents(t, events, []expectedListenerEvent{
				{phase: "before", function: "run_twice"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "before", function: "async_work"},
				{phase: "after", function: "async_work"},
				{phase: "after", function: "run_twice"},
			})
			assertBeforeCompletionPairing(t, events)
			assertBeforeCompletionStackPairing(t, events)

			initial := initialObserver.observations.snapshot()
			require.Equal(t, 1, len(initial))
			require.Equal(t, experimental.FuelEventBudgeted, initial[0].Event)
			require.Equal(t, int64(20), initial[0].Budget)
			require.Equal(t, int64(20), initial[0].Remaining)

			resumed := resumeObserver.observations.snapshot()
			require.Equal(t, 1, len(resumed))
			require.Equal(t, experimental.FuelEventConsumed, resumed[0].Event)
			require.Equal(t, int64(20), resumed[0].Budget)
			require.Equal(t, resumed[0].Consumed, ctrl.TotalConsumed())
			require.Equal(t, resumed[0].Budget-resumed[0].Consumed, resumed[0].Remaining)

			assertOrderedEvents(t, recorder.snapshot(), []string{
				"fuel budgeted",
				"listener before run_twice",
				"listener before async_work",
				"yield yielded #1",
				"yield resumed #1",
				"listener after async_work",
				"listener before async_work",
				"yield yielded #2",
				"yield resumed #2",
				"listener after async_work",
				"listener after run_twice",
				"fuel consumed",
			})
		})
	}
}
