//go:build unix

package platform

import (
	"runtime/debug"
	"testing"
	"unsafe"
)

//go:noinline
func readByteAt(ptr unsafe.Pointer) byte {
	return *(*byte)(ptr)
}

func TestMmapLinearMemory_Basic(t *testing.T) {
	const commitSize = 65536 // 1 page
	const maxSize = 65536 * 4

	buf, err := MmapLinearMemory(maxSize, commitSize)
	if err != nil {
		t.Fatalf("MmapLinearMemory failed: %v", err)
	}
	defer func() {
		if err := MunmapLinearMemory(buf); err != nil {
			t.Fatalf("MunmapLinearMemory failed: %v", err)
		}
	}()

	// Write and read within the committed region.
	buf[0] = 42
	buf[commitSize-1] = 99
	if buf[0] != 42 {
		t.Fatalf("expected buf[0] == 42, got %d", buf[0])
	}
	if buf[commitSize-1] != 99 {
		t.Fatalf("expected buf[%d] == 99, got %d", commitSize-1, buf[commitSize-1])
	}
}

func TestMmapLinearMemory_GuardPageFault(t *testing.T) {
	const commitSize = 65536
	const maxSize = 65536 * 2

	buf, err := MmapLinearMemory(maxSize, commitSize)
	if err != nil {
		t.Fatalf("MmapLinearMemory failed: %v", err)
	}
	defer MunmapLinearMemory(buf)

	// Access one byte past the committed region via unsafe pointer — should fault.
	// We use unsafe.Pointer because Go slice access may be optimized away by the
	// compiler. In the real wazevo path, compiled machine code accesses memory
	// via raw pointers, so this is the realistic test.
	recovered := false
	func() {
		old := debug.SetPanicOnFault(true)
		defer debug.SetPanicOnFault(old)

		defer func() {
			if r := recover(); r != nil {
				recovered = true
				t.Logf("recovered from guard page fault: %v", r)
			}
		}()

		// This should trigger SIGSEGV / fault panic.
		ptr := unsafe.Pointer(&buf[0])
		_ = readByteAt(unsafe.Add(ptr, commitSize))
	}()

	if !recovered {
		t.Fatal("expected a panic from accessing guard page, but none occurred")
	}
}

func TestMmapLinearMemory_Grow(t *testing.T) {
	const pageSize = 65536
	const maxSize = pageSize * 10

	buf, err := MmapLinearMemory(maxSize, pageSize)
	if err != nil {
		t.Fatalf("MmapLinearMemory failed: %v", err)
	}
	defer MunmapLinearMemory(buf)

	// Write to the initial committed region.
	buf[0] = 1
	buf[pageSize-1] = 2

	// Grow to 3 pages.
	if err := MmapGrowLinearMemory(buf, pageSize, pageSize*3); err != nil {
		t.Fatalf("MmapGrowLinearMemory failed: %v", err)
	}

	// Now we should be able to access the newly committed region.
	buf[pageSize] = 3
	buf[pageSize*3-1] = 4
	if buf[pageSize] != 3 {
		t.Fatalf("expected buf[%d] == 3, got %d", pageSize, buf[pageSize])
	}

	// Original data should still be intact.
	if buf[0] != 1 {
		t.Fatalf("expected buf[0] == 1, got %d", buf[0])
	}
}

func TestMmapLinearMemory_GrowThenFault(t *testing.T) {
	const pageSize = 65536
	const maxSize = pageSize * 4

	buf, err := MmapLinearMemory(maxSize, pageSize)
	if err != nil {
		t.Fatalf("MmapLinearMemory failed: %v", err)
	}
	defer MunmapLinearMemory(buf)

	// Grow to 2 pages.
	if err := MmapGrowLinearMemory(buf, pageSize, pageSize*2); err != nil {
		t.Fatalf("MmapGrowLinearMemory failed: %v", err)
	}

	// Access at byte offset 2*pageSize should still fault since only 2 pages committed.
	recovered := false
	func() {
		old := debug.SetPanicOnFault(true)
		defer debug.SetPanicOnFault(old)

		defer func() {
			if r := recover(); r != nil {
				recovered = true
			}
		}()
		ptr := unsafe.Pointer(&buf[0])
		_ = readByteAt(unsafe.Add(ptr, pageSize*2))
	}()

	if !recovered {
		t.Fatal("expected guard page fault at uncommitted offset after grow")
	}
}

func TestMmapLinearMemory_CommitZero(t *testing.T) {
	const maxSize = 65536

	buf, err := MmapLinearMemory(maxSize, 0)
	if err != nil {
		t.Fatalf("MmapLinearMemory failed: %v", err)
	}
	defer MunmapLinearMemory(buf)

	// Even buf[0] should fault since 0 bytes are committed.
	recovered := false
	func() {
		old := debug.SetPanicOnFault(true)
		defer debug.SetPanicOnFault(old)

		defer func() {
			if r := recover(); r != nil {
				recovered = true
			}
		}()
		ptr := unsafe.Pointer(&buf[0])
		_ = readByteAt(ptr)
	}()

	if !recovered {
		t.Fatal("expected fault with 0 committed bytes")
	}
}

func TestSupportsGuardPages(t *testing.T) {
	if !SupportsGuardPages() {
		t.Fatal("expected SupportsGuardPages to return true on unix")
	}
}
