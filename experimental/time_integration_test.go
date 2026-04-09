package experimental_test

import (
	"context"
	"testing"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/platform"
	"github.com/tetratelabs/wazero/internal/testing/binaryencoding"
	"github.com/tetratelabs/wazero/internal/testing/require"
	"github.com/tetratelabs/wazero/internal/wasm"
)

type fixedTimeProvider struct {
	sec  int64
	nsec int32
	nano int64
}

func (p fixedTimeProvider) Walltime() (int64, int32) { return p.sec, p.nsec }
func (p fixedTimeProvider) Nanotime() int64          { return p.nano }
func (p fixedTimeProvider) Nanosleep(int64)          {}

func TestTimeProvider_RuntimeDefaultOnImportedHostCall(t *testing.T) {
	moduleBinary := binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		ImportSection:   []wasm.Import{{Module: "env", Name: "now", Type: wasm.ExternTypeFunc, DescFunc: 0}},
		FunctionSection: []wasm.Index{0},
		CodeSection:     []wasm.Code{{Body: []byte{wasm.OpcodeCall, 0, wasm.OpcodeEnd}}},
		ExportSection:   []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 1}},
	})

	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg.WithTimeProvider(fixedTimeProvider{nano: 123}))
			defer rt.Close(ctx)

			var got int64
			_, err := rt.NewHostModuleBuilder("env").
				NewFunctionBuilder().
				WithFunc(func(ctx context.Context) {
					provider := experimental.GetTimeProvider(ctx)
					if provider == nil {
						t.Fatal("expected time provider in host call context")
					}
					got = provider.Nanotime()
				}).
				Export("now").
				Instantiate(ctx)
			require.NoError(t, err)

			mod, err := rt.Instantiate(ctx, moduleBinary)
			require.NoError(t, err)

			_, err = mod.ExportedFunction("run").Call(ctx)
			require.NoError(t, err)
			require.Equal(t, int64(123), got)
		})
	}
}

func TestTimeProvider_CallContextOverridesRuntimeDefaultOnImportedHostCall(t *testing.T) {
	moduleBinary := binaryencoding.EncodeModule(&wasm.Module{
		TypeSection:     []wasm.FunctionType{{}},
		ImportSection:   []wasm.Import{{Module: "env", Name: "now", Type: wasm.ExternTypeFunc, DescFunc: 0}},
		FunctionSection: []wasm.Index{0},
		CodeSection:     []wasm.Code{{Body: []byte{wasm.OpcodeCall, 0, wasm.OpcodeEnd}}},
		ExportSection:   []wasm.Export{{Type: api.ExternTypeFunc, Name: "run", Index: 1}},
	})

	for _, ec := range engineConfigs() {
		t.Run(ec.name, func(t *testing.T) {
			if ec.name == "compiler" && !platform.CompilerSupported() {
				t.Skip("compiler is not supported on this host")
			}

			ctx := context.Background()
			rt := wazero.NewRuntimeWithConfig(ctx, ec.cfg.WithTimeProvider(fixedTimeProvider{nano: 123}))
			defer rt.Close(ctx)

			var got int64
			_, err := rt.NewHostModuleBuilder("env").
				NewFunctionBuilder().
				WithFunc(func(ctx context.Context) {
					provider := experimental.GetTimeProvider(ctx)
					if provider == nil {
						t.Fatal("expected time provider in host call context")
					}
					got = provider.Nanotime()
				}).
				Export("now").
				Instantiate(ctx)
			require.NoError(t, err)

			mod, err := rt.Instantiate(ctx, moduleBinary)
			require.NoError(t, err)

			callCtx := experimental.WithTimeProvider(ctx, fixedTimeProvider{nano: 456})
			_, err = mod.ExportedFunction("run").Call(callCtx)
			require.NoError(t, err)
			require.Equal(t, int64(456), got)
		})
	}
}
