// Package secbench provides benchmark baselines for se-wazero security features.
package secbench

import (
	"context"
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/secmem"
	"github.com/tetratelabs/wazero/internal/wasm"
)

func repoRoot() string {
	_, f, _, _ := runtime.Caller(0)
	// internal/secbench/bench_test.go -> repo root is ../..
	return filepath.Join(filepath.Dir(f), "..", "..")
}

func loadTestWasm(t testing.TB, name string) []byte {
	data, err := os.ReadFile(filepath.Join(repoRoot(), "testdata", name))
	if err != nil {
		t.Fatalf("failed to load test wasm %s: %v", name, err)
	}
	return data
}

// BenchmarkCompileTime measures the cost of compiling a Wasm module.
func BenchmarkCompileTime(b *testing.B) {
	facWasm := loadTestWasm(b, "fac.wasm")

	for _, mode := range []struct {
		name   string
		secure bool
	}{
		{"standard", false},
		{"secure", true},
	} {
		b.Run(mode.name, func(b *testing.B) {
			b.ReportAllocs()
			for i := 0; i < b.N; i++ {
				ctx := context.Background()
				config := wazero.NewRuntimeConfig().WithSecureMode(mode.secure)
				r := wazero.NewRuntimeWithConfig(ctx, config)
				_, err := r.CompileModule(ctx, facWasm)
				if err != nil {
					b.Fatal(err)
				}
				r.Close(ctx)
			}
		})
	}
}

// BenchmarkExecutionBaseline measures execution time for a compute-bound function.
func BenchmarkExecutionBaseline(b *testing.B) {
	facWasm := loadTestWasm(b, "fac.wasm")

	for _, mode := range []struct {
		name   string
		secure bool
	}{
		{"standard", false},
		{"secure", true},
	} {
		b.Run(mode.name, func(b *testing.B) {
			ctx := context.Background()
			config := wazero.NewRuntimeConfig().WithSecureMode(mode.secure)
			r := wazero.NewRuntimeWithConfig(ctx, config)
			defer r.Close(ctx)

			mod, err := r.InstantiateWithConfig(ctx, facWasm,
				wazero.NewModuleConfig().WithStartFunctions()) // Don't call start
			if err != nil {
				b.Fatal(err)
			}

			fac := mod.ExportedFunction("fac-ssa")
			if fac == nil {
				b.Fatal("fac-ssa not exported")
			}

			b.ResetTimer()
			b.ReportAllocs()
			for i := 0; i < b.N; i++ {
				results, err := fac.Call(ctx, 20)
				if err != nil {
					b.Fatal(err)
				}
				_ = results
			}
		})
	}
}

// BenchmarkMemoryAllocate measures the latency of creating a MemoryAllocator-backed
// memory instance with standard Go slices vs. mmap guard pages.
func BenchmarkMemoryAllocate(b *testing.B) {
	const (
		capPages = 1   // Initial capacity: 64 KiB
		maxPages = 256 // Max: 16 MiB
	)
	capBytes := uint64(capPages) * uint64(wasm.MemoryPageSize)
	maxBytes := uint64(maxPages) * uint64(wasm.MemoryPageSize)

	b.Run("go_slice", func(b *testing.B) {
		b.ReportAllocs()
		for i := 0; i < b.N; i++ {
			buf := make([]byte, capBytes, maxBytes)
			_ = buf
		}
	})

	b.Run("guard_page_mmap", func(b *testing.B) {
		alloc := secmem.GuardPageAllocator{}
		b.ReportAllocs()
		for i := 0; i < b.N; i++ {
			lm := alloc.Allocate(capBytes, maxBytes)
			buf := lm.Reallocate(capBytes)
			_ = buf
			lm.Free()
		}
	})
}

// BenchmarkMemoryGrow measures the latency of memory.grow operations.
func BenchmarkMemoryGrow(b *testing.B) {
	memGrowWasm := loadTestWasm(b, "mem_grow.wasm")

	for _, mode := range []struct {
		name   string
		secure bool
	}{
		{"standard", false},
		{"secure", true},
	} {
		b.Run(mode.name, func(b *testing.B) {
			b.ReportAllocs()
			for i := 0; i < b.N; i++ {
				ctx := context.Background()
				config := wazero.NewRuntimeConfig().WithSecureMode(mode.secure)
				r := wazero.NewRuntimeWithConfig(ctx, config)

				// mem_grow.wasm starts by growing memory in a loop until it fails,
				// so it exercises the grow path. It hits unreachable after failing.
				_, err := r.Instantiate(ctx, memGrowWasm)
				// We expect either an exit error or unreachable trap — that's fine.
				_ = err

				r.Close(ctx)
			}
		})
	}
}

// BenchmarkGuardPageAllocatorGrow measures mmap guard-page grow specifically.
func BenchmarkGuardPageAllocatorGrow(b *testing.B) {
	alloc := secmem.GuardPageAllocator{}
	const maxPages = 1024 // 64 MiB max
	maxBytes := uint64(maxPages) * uint64(wasm.MemoryPageSize)

	b.ReportAllocs()
	for i := 0; i < b.N; i++ {
		lm := alloc.Allocate(0, maxBytes)
		// Grow from 0 to 10 pages incrementally.
		for p := uint64(1); p <= 10; p++ {
			buf := lm.Reallocate(p * uint64(wasm.MemoryPageSize))
			if buf == nil {
				b.Fatalf("grow to page %d failed", p)
			}
		}
		lm.Free()
	}
}

// compile-time interface checks
var _ experimental.MemoryAllocator = secmem.GuardPageAllocator{}
