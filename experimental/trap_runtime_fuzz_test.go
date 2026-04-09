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
	"github.com/tetratelabs/wazero/internal/wasm"
	"github.com/tetratelabs/wazero/internal/wasmruntime"
)

const (
	trapRuntimeModeUnreachable uint8 = iota
	trapRuntimeModeOutOfBoundsMemory
	trapRuntimeModeFuelExhausted
	trapRuntimeModeMemoryFault
)

var (
	trapRuntimeMemoryFaultFixtureOnce  sync.Once
	trapRuntimeMemoryFaultFixtureBytes []byte
	trapRuntimeMemoryFaultFixtureErr   error
)

func FuzzTrapRuntimeClassification(f *testing.F) {
	for _, seed := range supportedTrapRuntimeModes() {
		f.Add(seed, true)
		f.Add(seed, false)
	}

	f.Fuzz(func(t *testing.T, mode uint8, observe bool) {
		cfg, moduleBinary, exportName, moduleName, wantErr, wantCause := trapRuntimeFuzzCase(t, mode)

		ctx := context.Background()
		rt := wazero.NewRuntimeWithConfig(ctx, cfg)
		defer rt.Close(ctx)

		mod, err := rt.InstantiateWithConfig(ctx, moduleBinary, wazero.NewModuleConfig().WithName(moduleName))
		if err != nil {
			t.Fatalf("instantiate module: %v", err)
		}

		observer := &recordingTrapObserver{}
		callCtx := ctx
		if observe {
			callCtx = experimental.WithTrapObserver(callCtx, observer)
		}

		_, err = mod.ExportedFunction(exportName).Call(callCtx)
		if err == nil {
			t.Fatal("expected trap error")
		}
		if !errors.Is(err, wantErr) {
			t.Fatalf("error = %v, want %v", err, wantErr)
		}
		if !observe {
			if observer.count() != 0 {
				t.Fatalf("unexpected trap observations without observer registration: %d", observer.count())
			}
			return
		}

		observation := observer.single(t)
		if observation.Cause != wantCause {
			t.Fatalf("observation cause = %q, want %q", observation.Cause, wantCause)
		}
		if !errors.Is(observation.Err, wantErr) {
			t.Fatalf("observation error = %v, want %v", observation.Err, wantErr)
		}
		if observation.Module == nil {
			t.Fatal("observation module must not be nil")
		}
		if observation.Module.Name() != moduleName {
			t.Fatalf("observation module = %q, want %q", observation.Module.Name(), moduleName)
		}
	})
}

func supportedTrapRuntimeModes() []uint8 {
	modes := []uint8{trapRuntimeModeUnreachable, trapRuntimeModeOutOfBoundsMemory}
	if platform.CompilerSupported() {
		modes = append(modes, trapRuntimeModeFuelExhausted)
	}
	if supportsGuardedMemoryFaultTrap() {
		modes = append(modes, trapRuntimeModeMemoryFault)
	}
	return modes
}

func trapRuntimeFuzzCase(t *testing.T, mode uint8) (cfg wazero.RuntimeConfig, moduleBinary []byte, exportName string, moduleName string, wantErr error, wantCause experimental.TrapCause) {
	t.Helper()

	modes := supportedTrapRuntimeModes()
	selected := modes[int(mode)%len(modes)]

	switch selected {
	case trapRuntimeModeUnreachable:
		return wazero.NewRuntimeConfigInterpreter(), trapRuntimeUnreachableBinary(), "run", "trap-unreachable", wasmruntime.ErrRuntimeUnreachable, experimental.TrapCauseUnreachable
	case trapRuntimeModeOutOfBoundsMemory:
		return wazero.NewRuntimeConfigInterpreter(), trapRuntimeOutOfBoundsLoadBinary(), "run", "trap-oob", wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess, experimental.TrapCauseOutOfBoundsMemoryAccess
	case trapRuntimeModeFuelExhausted:
		return wazero.NewRuntimeConfigCompiler().WithFuel(1), trapObserverFuelLoopBinary(), "run", "trap-fuel", wasmruntime.ErrRuntimeFuelExhausted, experimental.TrapCauseFuelExhausted
	case trapRuntimeModeMemoryFault:
		return wazero.NewRuntimeConfigCompiler().WithSecureMode(true), trapRuntimeMemoryFaultFixture(t), "oob", "trap-memory-fault", wasmruntime.ErrRuntimeMemoryFault, experimental.TrapCauseMemoryFault
	default:
		t.Fatalf("unexpected mode %d", selected)
		return nil, nil, "", "", nil, ""
	}
}

func trapRuntimeUnreachableBinary() []byte {
	return binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		FunctionSection: []wasm.Index{0},
		CodeSection:     []wasm.Code{{Body: []byte{wasm.OpcodeUnreachable, wasm.OpcodeEnd}}},
		ExportSection:   []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 0}},
	})
}

func trapRuntimeOutOfBoundsLoadBinary() []byte {
	return binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{Results: []api.ValueType{api.ValueTypeI32}}},
		FunctionSection: []wasm.Index{0},
		MemorySection:   &wasm.Memory{Min: 1},
		CodeSection: []wasm.Code{{Body: []byte{
			wasm.OpcodeI32Const, 0x00,
			wasm.OpcodeI32Load, 0x02, 0x80, 0x80, 0x04,
			wasm.OpcodeEnd,
		}}},
		ExportSection: []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 0}},
	})
}

func trapRuntimeMemoryFaultFixture(t *testing.T) []byte {
	t.Helper()
	trapRuntimeMemoryFaultFixtureOnce.Do(func() {
		trapRuntimeMemoryFaultFixtureBytes, trapRuntimeMemoryFaultFixtureErr = os.ReadFile("../testdata/oob_load.wasm")
	})
	if trapRuntimeMemoryFaultFixtureErr != nil {
		t.Fatalf("read memory fault fixture: %v", trapRuntimeMemoryFaultFixtureErr)
	}
	return trapRuntimeMemoryFaultFixtureBytes
}

func supportsGuardedMemoryFaultTrap() bool {
	return platform.CompilerSupported() && runtime.GOOS == "linux" && (runtime.GOARCH == "amd64" || runtime.GOARCH == "arm64")
}
