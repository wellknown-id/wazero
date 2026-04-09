package experimental_test

import (
	"context"
	"errors"
	"os"
	"runtime"
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

type recordingTrapObserver struct {
	mu           sync.Mutex
	observations []experimental.TrapObservation
}

func (r *recordingTrapObserver) ObserveTrap(_ context.Context, observation experimental.TrapObservation) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.observations = append(r.observations, observation)
}

func (r *recordingTrapObserver) single(t *testing.T) experimental.TrapObservation {
	t.Helper()
	r.mu.Lock()
	defer r.mu.Unlock()
	require.Equal(t, 1, len(r.observations))
	return r.observations[0]
}

func trapObserverTestModuleBinary() []byte {
	return binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		ImportSection:   []wasm.Import{{Module: "env", Name: "check", Type: wasm.ExternTypeFunc, DescFunc: 0}},
		FunctionSection: []wasm.Index{0},
		CodeSection:     []wasm.Code{{Body: []byte{wasm.OpcodeCall, 0, wasm.OpcodeEnd}}},
		ExportSection:   []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 1}},
	})
}

func trapObserverFuelLoopBinary() []byte {
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

func TestTrapObserver_PolicyDenied(t *testing.T) {
	moduleBinary := trapObserverTestModuleBinary()

	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg)
			defer rt.Close(ctx)

			_, err := rt.NewHostModuleBuilder("env").
				NewFunctionBuilder().
				WithFunc(func() {}).
				Export("check").
				Instantiate(ctx)
			require.NoError(t, err)

			mod, err := rt.InstantiateWithConfig(ctx, moduleBinary, wazero.NewModuleConfig().WithName("guest"))
			require.NoError(t, err)

			observer := &recordingTrapObserver{}
			callCtx := experimental.WithTrapObserver(
				experimental.WithHostCallPolicy(ctx, experimental.HostCallPolicyFunc(
					func(context.Context, api.Module, api.FunctionDefinition) bool { return false },
				)),
				observer,
			)

			_, err = mod.ExportedFunction("run").Call(callCtx)
			require.Error(t, err)
			require.True(t, errors.Is(err, wasmruntime.ErrRuntimePolicyDenied), "expected ErrRuntimePolicyDenied, got %v", err)

			observation := observer.single(t)
			require.Equal(t, experimental.TrapCausePolicyDenied, observation.Cause)
			require.True(t, errors.Is(observation.Err, wasmruntime.ErrRuntimePolicyDenied))
			require.Equal(t, "guest", observation.Module.Name())
		})
	}
}

func TestTrapObserver_FuelExhausted(t *testing.T) {
	if !platform.CompilerSupported() {
		t.Skip("compiler is not supported on this host")
	}

	ctx := context.Background()
	rt := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfigCompiler().WithFuel(1))
	defer rt.Close(ctx)

	mod, err := rt.InstantiateWithConfig(ctx, trapObserverFuelLoopBinary(), wazero.NewModuleConfig().WithName("fuel-guest"))
	require.NoError(t, err)

	observer := &recordingTrapObserver{}
	callCtx := experimental.WithTrapObserver(ctx, observer)

	_, err = mod.ExportedFunction("run").Call(callCtx)
	require.Error(t, err)
	require.True(t, errors.Is(err, wasmruntime.ErrRuntimeFuelExhausted), "expected ErrRuntimeFuelExhausted, got %v", err)

	observation := observer.single(t)
	require.Equal(t, experimental.TrapCauseFuelExhausted, observation.Cause)
	require.True(t, errors.Is(observation.Err, wasmruntime.ErrRuntimeFuelExhausted))
	require.Equal(t, "fuel-guest", observation.Module.Name())
}

func TestTrapObserver_MemoryFault(t *testing.T) {
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

	observer := &recordingTrapObserver{}
	callCtx := experimental.WithTrapObserver(ctx, observer)

	_, err = mod.ExportedFunction("oob").Call(callCtx)
	require.Error(t, err)
	require.True(t, errors.Is(err, wasmruntime.ErrRuntimeMemoryFault), "expected ErrRuntimeMemoryFault, got %v", err)

	observation := observer.single(t)
	require.Equal(t, experimental.TrapCauseMemoryFault, observation.Cause)
	require.True(t, errors.Is(observation.Err, wasmruntime.ErrRuntimeMemoryFault))
	require.Equal(t, "secure-guest", observation.Module.Name())
}
