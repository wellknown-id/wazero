package wazero_test

import (
	"context"
	"os"
	"runtime"
	"testing"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/platform"
	"github.com/tetratelabs/wazero/internal/testing/binaryencoding"
	"github.com/tetratelabs/wazero/internal/testing/require"
	"github.com/tetratelabs/wazero/internal/wasm"
	"github.com/tetratelabs/wazero/internal/wasmruntime"
)

type countingAllocator struct {
	calls int
}

func (a *countingAllocator) Allocate(cap, max uint64) experimental.LinearMemory {
	a.calls++
	return &countingLinearMemory{buf: make([]byte, 0, max)}
}

type countingLinearMemory struct {
	buf []byte
}

func (m *countingLinearMemory) Reallocate(size uint64) []byte {
	if uint64(cap(m.buf)) < size {
		next := make([]byte, size)
		copy(next, m.buf)
		m.buf = next
	} else {
		m.buf = m.buf[:size]
	}
	return m.buf
}

func (*countingLinearMemory) Free() {}

func TestSecureMode_OutOfBoundsTrap(t *testing.T) {
	// This test verifies that the shared out-of-bounds fixture traps in both
	// secure and standard mode, and that secure mode only surfaces a distinct
	// memory fault on the platforms where the compiler signal path is active.
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
		require.Contains(t, err.Error(), expectedSecureModeMemoryTrap().Error())
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

func expectedSecureModeMemoryTrap() error {
	if platform.CompilerSupported() && runtime.GOOS == "linux" && (runtime.GOARCH == "amd64" || runtime.GOARCH == "arm64") {
		return wasmruntime.ErrRuntimeMemoryFault
	}
	return wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess
}

func TestSecureMode_PreservesCustomMemoryAllocator(t *testing.T) {
	ctx := context.Background()
	allocator := &countingAllocator{}
	ctx = experimental.WithMemoryAllocator(ctx, allocator)

	bin := binaryencoding.EncodeModule(&wasm.Module{
		MemorySection: &wasm.Memory{Min: 1, Cap: 1, Max: 1, IsMaxEncoded: true},
	})

	r := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfig().WithSecureMode(true))
	defer r.Close(ctx)

	_, err := r.Instantiate(ctx, bin)
	require.NoError(t, err)
	require.Equal(t, 1, allocator.calls)
}
