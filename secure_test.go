package wazero_test

import (
	"context"
	"os"
	"testing"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/internal/testing/require"
	"github.com/tetratelabs/wazero/internal/wasmruntime"
)

func TestSecureMode_OutOfBoundsTrap(t *testing.T) {
	// This test verifies that the shared out-of-bounds fixture traps in both
	// secure and standard mode. Platform-specific hardware-fault validation is
	// covered separately by the support/validation docs.
	ctx := context.Background()

	bin, err := os.ReadFile("testdata/oob_load.wasm")
	require.NoError(t, err)

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
