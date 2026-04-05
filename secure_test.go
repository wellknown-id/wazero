package wazero_test

import (
	"context"
	"testing"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/internal/testing/binaryencoding"
	"github.com/tetratelabs/wazero/internal/testing/require"
	"github.com/tetratelabs/wazero/internal/wasm"
	"github.com/tetratelabs/wazero/internal/wasmruntime"
)

func TestSecureMode_HardwareFaultToTrap(t *testing.T) {
	// This test verifies that hardware faults (SIGSEGV) are correctly caught
	// and converted into Wasm out-of-bounds traps when secureMode is enabled.
	ctx := context.Background()

	// A simple module that performs an out-of-bounds load.
	// (module
	//   (memory 1)
	//   (func (export "oob") (result i32)
	//     i32.const 100000 ;; 100KB, exceeds 1 page (64KB)
	//     i32.load
	//   )
	// )
	m := &wasm.Module{
		MemorySection: &wasm.Memory{Min: 1, Max: 1},
		TypeSection:   []wasm.FunctionType{{Results: []wasm.ValueType{wasm.ValueTypeI32}}},
		FunctionSection: []wasm.Index{0},
		CodeSection: []wasm.Code{
			{
				Body: []byte{
					wasm.OpcodeI32Const, 0xa0, 0x8d, 0x06, // 100000 in LEB128
					wasm.OpcodeI32Load, 0x02, 0x00,        // align=2, offset=0
					wasm.OpcodeEnd,
				},
			},
		},
		ExportSection: []wasm.Export{{Name: "oob", Type: wasm.ExternTypeFunc, Index: 0}},
	}
	bin := binaryencoding.EncodeModule(m)

	t.Run("secureMode=true", func(t *testing.T) {
		r := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfig().WithSecureMode(true))
		defer r.Close(ctx)

		mod, err := r.Instantiate(ctx, bin)
		require.NoError(t, err)

		_, err = mod.ExportedFunction("oob").Call(ctx)
		require.Error(t, err)
		require.Contains(t, err.Error(), wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess.Error())
	})

	t.Run("secureMode=false", func(t *testing.T) {
		r := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfig().WithSecureMode(false))
		defer r.Close(ctx)

		mod, err := r.Instantiate(ctx, bin)
		require.NoError(t, err)

		_, err = mod.ExportedFunction("oob").Call(ctx)
		require.Error(t, err)
		require.Contains(t, err.Error(), wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess.Error())
	})
}
