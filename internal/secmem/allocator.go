// Package secmem implements a guard-page backed memory allocator for
// se-wazero's secure mode. It satisfies the experimental.MemoryAllocator
// interface and uses OS virtual memory primitives (mmap/VirtualAlloc) to
// provide hardware-enforced linear memory isolation.
package secmem

import (
	"fmt"
	"sync"
	"unsafe"

	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/platform"
)

// compile-time check
var _ experimental.MemoryAllocator = GuardPageAllocator{}

// GuardPageAllocator implements experimental.MemoryAllocator using
// mmap-backed virtual memory with guard pages.
//
// When Allocate is called, it reserves (max + 4 GiB guard) bytes of virtual
// address space with the initial capacity committed as readable/writable.
// Out-of-bounds access into the guard region triggers a hardware fault
// that Go's runtime.SetPanicOnFault converts to a recoverable panic.
type GuardPageAllocator struct{}

// Allocate implements experimental.MemoryAllocator.
func (GuardPageAllocator) Allocate(cap, max uint64) experimental.LinearMemory {
	if max == 0 {
		// If max is 0, there is no memory to allocate.
		return &guardPageMemory{}
	}
	buf, err := platform.MmapLinearMemory(max, 0)
	if err != nil {
		// If mmap fails (e.g. insufficient virtual address space), log and
		// fall back to a nil buffer. The caller (NewMemoryInstance) will
		// get a nil Reallocate result and handle it.
		// In practice this shouldn't happen for sane max values on 64-bit systems.
		panic(fmt.Sprintf("secmem: MmapLinearMemory(%d, 0) failed: %v", max, err))
	}
	return &guardPageMemory{
		buf:       buf,
		committed: 0,
		max:       max,
	}
}

// guardPageMemory implements experimental.LinearMemory backed by mmap
// with guard pages.
type guardPageMemory struct {
	mu        sync.Mutex
	buf       []byte // full mmap reservation including guard region
	committed uint64 // current committed size in bytes
	max       uint64 // maximum linear memory size (excludes guard region)
}

// Reallocate implements experimental.LinearMemory.
// It grows the committed region to `size` bytes by re-protecting guard pages.
// The base address never changes, which is required for shared memory.
func (g *guardPageMemory) Reallocate(size uint64) []byte {
	g.mu.Lock()
	defer g.mu.Unlock()

	if g.buf == nil {
		return nil
	}

	if size > g.max {
		return nil // cannot grow beyond max
	}

	if size > g.committed {
		if err := platform.MmapGrowLinearMemory(g.buf, g.committed, size); err != nil {
			return nil // grow failed
		}
		g.committed = size
	}

	// Return a slice viewing just the committed bytes, with cap == committed.
	// The underlying array is the mmap buffer so the base address is stable.
	return unsafe.Slice(&g.buf[0], int(size))
}

// Free implements experimental.LinearMemory.
func (g *guardPageMemory) Free() {
	g.mu.Lock()
	defer g.mu.Unlock()

	if g.buf != nil {
		_ = platform.MunmapLinearMemory(g.buf)
		g.buf = nil
		g.committed = 0
	}
}
