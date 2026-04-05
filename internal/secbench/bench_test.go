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

// BenchmarkFuelOverhead measures the performance impact of fuel metering on
// compute-bound workloads. By comparing execution with and without fuel enabled,
// we can quantify the overhead of the inline fuel checks.
func BenchmarkFuelOverhead(b *testing.B) {
	facWasm := loadTestWasm(b, "fac.wasm")

	for _, tc := range []struct {
		name string
		fuel int64
	}{
		{"no_fuel", 0},                // No fuel metering — baseline
		{"fuel_1M", 1_000_000},        // Generous budget — won't exhaust
		{"fuel_100M", 100_000_000},    // Very generous
	} {
		b.Run(tc.name, func(b *testing.B) {
			ctx := context.Background()
			config := wazero.NewRuntimeConfig().WithFuel(tc.fuel)
			r := wazero.NewRuntimeWithConfig(ctx, config)
			defer r.Close(ctx)

			mod, err := r.InstantiateWithConfig(ctx, facWasm,
				wazero.NewModuleConfig().WithStartFunctions())
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

// BenchmarkFuelOverheadWithController measures fuel overhead when using
// a FuelController from the experimental package.
func BenchmarkFuelOverheadWithController(b *testing.B) {
	facWasm := loadTestWasm(b, "fac.wasm")

	for _, tc := range []struct {
		name   string
		budget int64
	}{
		{"simple_1M", 1_000_000},
		{"aggregating_1M", 1_000_000},
	} {
		b.Run(tc.name, func(b *testing.B) {
			ctx := context.Background()
			// Enable fuel in the runtime config (any non-zero value enables compilation).
			config := wazero.NewRuntimeConfig().WithFuel(1)
			r := wazero.NewRuntimeWithConfig(ctx, config)
			defer r.Close(ctx)

			mod, err := r.InstantiateWithConfig(ctx, facWasm,
				wazero.NewModuleConfig().WithStartFunctions())
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
				var fc experimental.FuelController
				if tc.name == "aggregating_1M" {
					parent := experimental.NewSimpleFuelController(10_000_000)
					fc = experimental.NewAggregatingFuelController(parent, tc.budget)
				} else {
					fc = experimental.NewSimpleFuelController(tc.budget)
				}
				callCtx := experimental.WithFuelController(ctx, fc)
				results, err := fac.Call(callCtx, 20)
				if err != nil {
					b.Fatal(err)
				}
				_ = results
			}
		})
	}
}

// compile-time interface checks
var _ experimental.MemoryAllocator = secmem.GuardPageAllocator{}
