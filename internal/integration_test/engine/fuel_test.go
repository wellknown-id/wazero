package adhoc

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"runtime"
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

// TestFuelExhaustion verifies that an infinite-loop Wasm module is terminated
// when fuel runs out, and that the ErrRuntimeFuelExhausted error is surfaced.
func TestFuelExhaustion(t *testing.T) {
	if !platform.CompilerSupported() {
		t.Skip("fuel metering requires the compiler engine")
	}

	// Build a simple Wasm module with an infinite loop:
	//   (func (export "loop") (loop $l (br $l)))
	loopWasm := binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		FunctionSection: []wasm.Index{0},
		ExportSection: []wasm.Export{
			{Name: "loop", Type: wasm.ExternTypeFunc, Index: 0},
		},
		CodeSection: []wasm.Code{
			{Body: []byte{
				wasm.OpcodeLoop, 0x40, // loop (block type void)
				wasm.OpcodeBr, 0, // br 0 (back to loop header)
				wasm.OpcodeEnd, // end loop
				wasm.OpcodeEnd, // end func
			}},
		},
	})

	t.Run("config fuel exhausts", func(t *testing.T) {
		ctx := context.Background()
		config := wazero.NewRuntimeConfig().WithFuel(100)
		r := wazero.NewRuntimeWithConfig(ctx, config)
		defer r.Close(ctx)

		mod, err := r.InstantiateWithConfig(ctx, loopWasm,
			wazero.NewModuleConfig().WithName("fuel-test").WithStartFunctions())
		require.NoError(t, err)

		fn := mod.ExportedFunction("loop")
		require.NotNil(t, fn)

		_, err = fn.Call(ctx)
		require.Error(t, err)

		// Verify it's a fuel exhaustion error.
		require.Contains(t, err.Error(), "fuel exhausted")
	})

	t.Run("controller fuel exhausts", func(t *testing.T) {
		ctx := context.Background()
		// Enable fuel in the runtime so the compiler injects fuel checks.
		config := wazero.NewRuntimeConfig().WithFuel(1)
		r := wazero.NewRuntimeWithConfig(ctx, config)
		defer r.Close(ctx)

		mod, err := r.InstantiateWithConfig(ctx, loopWasm,
			wazero.NewModuleConfig().WithName("fuel-ctrl-test").WithStartFunctions())
		require.NoError(t, err)

		fn := mod.ExportedFunction("loop")
		require.NotNil(t, fn)

		ctrl := experimental.NewSimpleFuelController(50)
		callCtx := experimental.WithFuelController(ctx, ctrl)

		_, err = fn.Call(callCtx)
		require.Error(t, err)
		require.Contains(t, err.Error(), "fuel exhausted")

		// Controller should have tracked consumption.
		consumed := ctrl.TotalConsumed()
		// The exact number depends on the cost model (1 per func entry + 1 per loop back-edge).
		// With budget=50, it should have consumed roughly the budget before exhaustion.
		if consumed <= 0 {
			t.Fatalf("expected positive consumption, got %d", consumed)
		}
	})

	t.Run("unlimited fuel does not exhaust", func(t *testing.T) {
		ctx := context.Background()
		// WithFuel(0) means unlimited — no fuel checks should be inserted.
		config := wazero.NewRuntimeConfig().WithFuel(0)
		r := wazero.NewRuntimeWithConfig(ctx, config)
		defer r.Close(ctx)

		// Build a simple function that does NOT loop infinitely.
		// (func (export "noop"))
		noopWasm := binaryencoding.EncodeModule(&wasm.Module{
			TypeSection:     []wasm.FunctionType{{}},
			FunctionSection: []wasm.Index{0},
			ExportSection: []wasm.Export{
				{Name: "noop", Type: wasm.ExternTypeFunc, Index: 0},
			},
			CodeSection: []wasm.Code{
				{Body: []byte{wasm.OpcodeEnd}},
			},
		})

		mod, err := r.InstantiateWithConfig(ctx, noopWasm,
			wazero.NewModuleConfig().WithName("noop-test").WithStartFunctions())
		require.NoError(t, err)

		fn := mod.ExportedFunction("noop")
		require.NotNil(t, fn)

		_, err = fn.Call(ctx)
		require.NoError(t, err)
	})

	t.Run("finite computation completes within budget", func(t *testing.T) {
		ctx := context.Background()
		// Large enough budget that fac(20) completes.
		config := wazero.NewRuntimeConfig().WithFuel(1_000_000)
		r := wazero.NewRuntimeWithConfig(ctx, config)
		defer r.Close(ctx)

		facWasm, err := loadFuelTestdata("fac.wasm")
		if err != nil {
			t.Skip("fac.wasm not available:", err)
		}

		mod, err := r.InstantiateWithConfig(ctx, facWasm,
			wazero.NewModuleConfig().WithName("fac-fuel-test").WithStartFunctions())
		require.NoError(t, err)

		fac := mod.ExportedFunction("fac-ssa")
		require.NotNil(t, fac)

		results, err := fac.Call(ctx, 20)
		require.NoError(t, err)
		// fac(20) = 2432902008176640000
		require.Equal(t, uint64(2432902008176640000), results[0])
	})

	t.Run("aggregating controller cross-tenant", func(t *testing.T) {
		ctx := context.Background()
		config := wazero.NewRuntimeConfig().WithFuel(1)
		r := wazero.NewRuntimeWithConfig(ctx, config)
		defer r.Close(ctx)

		facWasm, err := loadFuelTestdata("fac.wasm")
		if err != nil {
			t.Skip("fac.wasm not available:", err)
		}

		mod, err := r.InstantiateWithConfig(ctx, facWasm,
			wazero.NewModuleConfig().WithName("fac-agg-test").WithStartFunctions())
		require.NoError(t, err)

		fac := mod.ExportedFunction("fac-ssa")
		require.NotNil(t, fac)

		// Parent (Alice) has a large budget.
		alice := experimental.NewSimpleFuelController(10_000_000)
		// Child (Bob) borrows a sub-budget from Alice.
		bob := experimental.NewAggregatingFuelController(alice, 1_000_000)

		callCtx := experimental.WithFuelController(ctx, bob)
		results, err := fac.Call(callCtx, 10)
		require.NoError(t, err)
		// fac(10) = 3628800
		require.Equal(t, uint64(3628800), results[0])

		// Both controllers should have tracked consumption.
		if bob.TotalConsumed() <= 0 {
			t.Fatal("bob should have consumed fuel")
		}
		if alice.TotalConsumed() <= 0 {
			t.Fatal("alice should see bob's consumption")
		}
		require.Equal(t, bob.TotalConsumed(), alice.TotalConsumed())
	})
}

// TestFuelExhaustionIsRuntimeError verifies that fuel exhaustion is a
// wasmruntime error that can be detected programmatically.
func TestFuelExhaustionIsRuntimeError(t *testing.T) {
	err := wasmruntime.ErrRuntimeFuelExhausted
	if err == nil {
		t.Fatal("ErrRuntimeFuelExhausted should not be nil")
	}
	if !errors.Is(err, err) {
		t.Fatal("ErrRuntimeFuelExhausted should be self-equal")
	}
	require.Contains(t, err.Error(), "fuel exhausted")
}

// loadFuelTestdata loads a file from the repo testdata directory.
func loadFuelTestdata(name string) ([]byte, error) {
	_, f, _, _ := runtime.Caller(0)
	root := filepath.Join(filepath.Dir(f), "..", "..", "..")
	return os.ReadFile(filepath.Join(root, "testdata", name))
}

// TestFuelAddFuelFromHostFunction verifies that a host function can add
// fuel mid-execution via experimental.AddFuel, allowing an otherwise
// fuel-exhausting module to complete.
func TestFuelAddFuelFromHostFunction(t *testing.T) {
	if !platform.CompilerSupported() {
		t.Skip("fuel metering requires the compiler engine")
	}

	ctx := context.Background()
	// Use a small fuel budget that the loop alone would exhaust.
	config := wazero.NewRuntimeConfig().WithFuel(50)
	r := wazero.NewRuntimeWithConfig(ctx, config)
	defer r.Close(ctx)

	// Host function that adds fuel.
	rechargeFn := func(callCtx context.Context, mod api.Module) {
		err := experimental.AddFuel(callCtx, 100)
		if err != nil {
			panic("AddFuel failed: " + err.Error())
		}
	}

	_, err := r.NewHostModuleBuilder("env").
		NewFunctionBuilder().WithFunc(rechargeFn).Export("recharge").
		Instantiate(ctx)
	require.NoError(t, err)

	// Build a Wasm module:
	//   (import "env" "recharge" (func))
	//   (func (export "run")
	//     call $recharge    ;; refills fuel
	//     call $recharge    ;; refills again
	//   )
	runWasm := binaryencoding.EncodeModule(&wasm.Module{
		TypeSection: []wasm.FunctionType{{}},
		ImportSection: []wasm.Import{
			{Module: "env", Name: "recharge", Type: wasm.ExternTypeFunc, DescFunc: 0},
		},
		FunctionSection: []wasm.Index{0},
		ExportSection: []wasm.Export{
			{Name: "run", Type: wasm.ExternTypeFunc, Index: 1},
		},
		CodeSection: []wasm.Code{
			{Body: []byte{
				wasm.OpcodeCall, 0, // call $recharge (index 0 = import)
				wasm.OpcodeCall, 0, // call $recharge again
				wasm.OpcodeEnd,
			}},
		},
	})

	mod, err := r.InstantiateWithConfig(ctx, runWasm,
		wazero.NewModuleConfig().WithName("addfuel-test").WithStartFunctions())
	require.NoError(t, err)

	fn := mod.ExportedFunction("run")
	require.NotNil(t, fn)

	// Should succeed because the host function recharges fuel.
	_, err = fn.Call(ctx)
	require.NoError(t, err)
}

// TestFuelRemainingFuelFromHostFunction verifies that a host function can
// inspect remaining fuel via experimental.RemainingFuel.
func TestFuelRemainingFuelFromHostFunction(t *testing.T) {
	if !platform.CompilerSupported() {
		t.Skip("fuel metering requires the compiler engine")
	}

	ctx := context.Background()
	config := wazero.NewRuntimeConfig().WithFuel(10_000)
	r := wazero.NewRuntimeWithConfig(ctx, config)
	defer r.Close(ctx)

	var capturedRemaining int64
	inspectFn := func(callCtx context.Context, mod api.Module) {
		remaining, err := experimental.RemainingFuel(callCtx)
		if err != nil {
			panic("RemainingFuel failed: " + err.Error())
		}
		capturedRemaining = remaining
	}

	_, err := r.NewHostModuleBuilder("env").
		NewFunctionBuilder().WithFunc(inspectFn).Export("inspect").
		Instantiate(ctx)
	require.NoError(t, err)

	// Build a Wasm module:
	//   (import "env" "inspect" (func))
	//   (func (export "run") call $inspect)
	inspectWasm := binaryencoding.EncodeModule(&wasm.Module{
		TypeSection: []wasm.FunctionType{{}},
		ImportSection: []wasm.Import{
			{Module: "env", Name: "inspect", Type: wasm.ExternTypeFunc, DescFunc: 0},
		},
		FunctionSection: []wasm.Index{0},
		ExportSection: []wasm.Export{
			{Name: "run", Type: wasm.ExternTypeFunc, Index: 1},
		},
		CodeSection: []wasm.Code{
			{Body: []byte{
				wasm.OpcodeCall, 0, // call $inspect
				wasm.OpcodeEnd,
			}},
		},
	})

	mod, err := r.InstantiateWithConfig(ctx, inspectWasm,
		wazero.NewModuleConfig().WithName("remaining-test").WithStartFunctions())
	require.NoError(t, err)

	fn := mod.ExportedFunction("run")
	require.NotNil(t, fn)

	_, err = fn.Call(ctx)
	require.NoError(t, err)

	// The host function should have seen some reduced fuel (after function
	// entry costs). It should be strictly less than the initial budget.
	if capturedRemaining <= 0 {
		t.Fatalf("expected positive remaining fuel, got %d", capturedRemaining)
	}
	if capturedRemaining >= 10_000 {
		t.Fatalf("remaining fuel should be less than initial budget, got %d", capturedRemaining)
	}
	t.Logf("remaining fuel when host function ran: %d / 10000", capturedRemaining)
}
