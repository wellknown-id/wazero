package experimental_test

import (
	"context"
	"errors"
	"testing"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/testing/binaryencoding"
	"github.com/tetratelabs/wazero/internal/wasm"
	"github.com/tetratelabs/wazero/internal/wasmruntime"
)

const (
	hostBoundaryImportedCall uint8 = iota
	hostBoundaryStartCall
	hostBoundaryHostPanic
	hostBoundaryPureWasm
	hostBoundaryModeCount
)

func FuzzHostBoundaryPolicyTrap(f *testing.F) {
	f.Add(hostBoundaryImportedCall, true, true)
	f.Add(hostBoundaryImportedCall, false, true)
	f.Add(hostBoundaryStartCall, true, true)
	f.Add(hostBoundaryStartCall, false, false)
	f.Add(hostBoundaryHostPanic, false, true)
	f.Add(hostBoundaryHostPanic, true, true)
	f.Add(hostBoundaryPureWasm, true, true)

	f.Fuzz(func(t *testing.T, mode uint8, deny bool, observe bool) {
		ctx := context.Background()
		rt := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfigInterpreter())
		defer rt.Close(ctx)

		mode %= hostBoundaryModeCount

		var (
			hostCalled   bool
			policyCalled bool
			mod          api.Module
			err          error
		)

		moduleName := "guest"
		observer := &recordingTrapObserver{}
		callCtx := experimental.WithHostCallPolicy(ctx, experimental.HostCallPolicyFunc(
			func(_ context.Context, caller api.Module, hostFunction api.FunctionDefinition) bool {
				policyCalled = true
				if caller == nil {
					t.Fatal("policy caller must not be nil")
				}
				if caller.Name() != moduleName {
					t.Fatalf("policy caller name = %q, want %q", caller.Name(), moduleName)
				}
				if hostFunction == nil {
					t.Fatal("policy host function must not be nil")
				}
				if hostFunction.DebugName() != "env.check" {
					t.Fatalf("policy host function = %q, want env.check", hostFunction.DebugName())
				}
				return !deny
			},
		))
		if observe {
			callCtx = experimental.WithTrapObserver(callCtx, observer)
		}

		switch mode {
		case hostBoundaryImportedCall:
			mod = instantiateBoundaryGuest(t, rt, ctx, false, func() { hostCalled = true })
		case hostBoundaryStartCall:
			moduleName = "guest-start"
			registerBoundaryHost(t, rt, ctx, func() { hostCalled = true })
			_, err = rt.InstantiateWithConfig(callCtx, boundaryStartModuleBinary(), wazero.NewModuleConfig().WithName(moduleName))
		case hostBoundaryHostPanic:
			mod = instantiateBoundaryGuest(t, rt, ctx, false, func() {
				hostCalled = true
				panic("boom")
			})
		case hostBoundaryPureWasm:
			mod, err = rt.InstantiateWithConfig(callCtx, boundaryPureWasmBinary(), wazero.NewModuleConfig().WithName(moduleName))
		default:
			t.Fatalf("unexpected mode %d", mode)
		}

		if err == nil && mod != nil {
			_, err = mod.ExportedFunction("run").Call(callCtx)
		}

		switch mode {
		case hostBoundaryImportedCall:
			assertHostBoundaryPolicyResult(t, err, hostCalled, policyCalled, observer, observe, deny, moduleName)
		case hostBoundaryStartCall:
			assertHostBoundaryPolicyResult(t, err, hostCalled, policyCalled, observer, observe, deny, moduleName)
		case hostBoundaryHostPanic:
			if !policyCalled {
				t.Fatal("policy should be consulted before imported host calls")
			}
			if deny {
				assertPolicyDeniedObservation(t, err, hostCalled, observer, observe, moduleName)
				return
			}
			if err == nil {
				t.Fatal("expected host panic error")
			}
			if !hostCalled {
				t.Fatal("host panic case should enter the host function")
			}
			if errors.Is(err, wasmruntime.ErrRuntimePolicyDenied) {
				t.Fatalf("host panic case returned policy denied: %v", err)
			}
			if observer.count() != 0 {
				t.Fatalf("host panic case should not notify trap observers, got %d observations", observer.count())
			}
		case hostBoundaryPureWasm:
			if err != nil {
				t.Fatalf("pure wasm case returned error: %v", err)
			}
			if policyCalled {
				t.Fatal("policy should not be consulted when there are no imported host calls")
			}
			if hostCalled {
				t.Fatal("pure wasm case should not call any host function")
			}
			if observer.count() != 0 {
				t.Fatalf("pure wasm case should not notify trap observers, got %d observations", observer.count())
			}
		}
	})
}

func instantiateBoundaryGuest(t *testing.T, rt wazero.Runtime, ctx context.Context, start bool, host func()) api.Module {
	t.Helper()
	registerBoundaryHost(t, rt, ctx, host)

	mod, err := rt.InstantiateWithConfig(ctx, boundaryImportedCallModuleBinary(start), wazero.NewModuleConfig().WithName("guest"))
	if err != nil {
		t.Fatalf("instantiate guest: %v", err)
	}
	return mod
}

func registerBoundaryHost(t *testing.T, rt wazero.Runtime, ctx context.Context, host func()) {
	t.Helper()
	_, err := rt.NewHostModuleBuilder("env").
		NewFunctionBuilder().
		WithFunc(host).
		Export("check").
		Instantiate(ctx)
	if err != nil {
		t.Fatalf("instantiate host: %v", err)
	}
}

func assertHostBoundaryPolicyResult(t *testing.T, err error, hostCalled bool, policyCalled bool, observer *recordingTrapObserver, observe bool, deny bool, moduleName string) {
	t.Helper()

	if !policyCalled {
		t.Fatal("policy should be consulted before imported host calls")
	}
	if deny {
		assertPolicyDeniedObservation(t, err, hostCalled, observer, observe, moduleName)
		return
	}
	if err != nil {
		t.Fatalf("allowed host call returned error: %v", err)
	}
	if !hostCalled {
		t.Fatal("allowed host call should reach the host function")
	}
	if observer.count() != 0 {
		t.Fatalf("allowed host call should not notify trap observers, got %d observations", observer.count())
	}
}

func assertPolicyDeniedObservation(t *testing.T, err error, hostCalled bool, observer *recordingTrapObserver, observe bool, moduleName string) {
	t.Helper()

	if err == nil {
		t.Fatal("expected policy denied error")
	}
	if hostCalled {
		t.Fatal("denied host call should not reach the host function")
	}
	if !errors.Is(err, wasmruntime.ErrRuntimePolicyDenied) {
		t.Fatalf("expected policy denied error, got %v", err)
	}
	if !observe {
		if observer.count() != 0 {
			t.Fatalf("unexpected trap observations without observer registration: %d", observer.count())
		}
		return
	}

	observation := observer.single(t)
	if observation.Cause != experimental.TrapCausePolicyDenied {
		t.Fatalf("observation cause = %q, want %q", observation.Cause, experimental.TrapCausePolicyDenied)
	}
	if !errors.Is(observation.Err, wasmruntime.ErrRuntimePolicyDenied) {
		t.Fatalf("observation error = %v, want policy denied", observation.Err)
	}
	if observation.Module == nil {
		t.Fatal("observation module must not be nil")
	}
	if observation.Module.Name() != moduleName {
		t.Fatalf("observation module = %q, want %q", observation.Module.Name(), moduleName)
	}
}

func boundaryImportedCallModuleBinary(start bool) []byte {
	mod := &wasm.Module{
		TypeSection:   []wasm.FunctionType{{}},
		ImportSection: []wasm.Import{{Module: "env", Name: "check", Type: wasm.ExternTypeFunc, DescFunc: 0}},
	}
	if start {
		startIndex := wasm.Index(0)
		mod.StartSection = &startIndex
		return binaryencoding.EncodeModule(mod)
	}
	mod.FunctionSection = []wasm.Index{0}
	mod.CodeSection = []wasm.Code{{Body: []byte{wasm.OpcodeCall, 0, wasm.OpcodeEnd}}}
	mod.ExportSection = []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 1}}
	return binaryencoding.EncodeModule(mod)
}

func boundaryStartModuleBinary() []byte {
	return boundaryImportedCallModuleBinary(true)
}

func boundaryPureWasmBinary() []byte {
	return binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		FunctionSection: []wasm.Index{0},
		CodeSection:     []wasm.Code{{Body: []byte{wasm.OpcodeEnd}}},
		ExportSection:   []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 0}},
	})
}
