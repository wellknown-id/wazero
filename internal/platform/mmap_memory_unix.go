//go:build unix

package platform

import (
	"fmt"
	"unsafe"

	"golang.org/x/sys/unix"
)

const (
	// GuardRegionSize is the size of the guard region placed after the maximum
	// linear memory to catch out-of-bounds accesses via hardware faults.
	// Set to 4 GiB so that any 32-bit offset from within the linear memory base
	// address that exceeds the committed region hits a guard page, matching
	// Wasmtime's strategy. This eliminates the need for software bounds checks
	// on basic load/store instructions.
	GuardRegionSize = 4 << 30 // 4 GiB
)

// SupportsGuardPages reports whether the current platform supports
// mmap-backed guard pages for linear memory isolation.
func SupportsGuardPages() bool {
	return true
}

// MmapLinearMemory reserves a contiguous virtual address range with guard pages
// for use as WebAssembly linear memory.
//
// It reserves (maxBytes + GuardRegionSize) bytes as PROT_NONE (inaccessible),
// then commits the first commitBytes as PROT_READ|PROT_WRITE.
//
// Out-of-bounds access beyond the committed region will trigger SIGSEGV/SIGBUS
// which Go's runtime.SetPanicOnFault converts to a recoverable panic.
//
// The returned slice covers the full reserved region including guard pages.
// Only the first commitBytes are writable.
func MmapLinearMemory(reserveBytes, commitBytes uint64) ([]byte, error) {
	if commitBytes > reserveBytes {
		return nil, fmt.Errorf("commitBytes (%d) exceeds reserveBytes (%d)", commitBytes, reserveBytes)
	}

	totalReserve := reserveBytes + GuardRegionSize

	// Reserve the entire range as inaccessible.
	buf, err := unix.Mmap(-1, 0, int(totalReserve), unix.PROT_NONE, unix.MAP_ANON|unix.MAP_PRIVATE)
	if err != nil {
		return nil, fmt.Errorf("mmap reserve failed: %w", err)
	}

	// Commit the initial region as read/write.
	if commitBytes > 0 {
		if err := unix.Mprotect(buf[:commitBytes], unix.PROT_READ|unix.PROT_WRITE); err != nil {
			_ = unix.Munmap(buf)
			return nil, fmt.Errorf("mprotect commit failed: %w", err)
		}
	}

	return buf, nil
}

// MmapGrowLinearMemory re-protects pages from PROT_NONE to PROT_READ|PROT_WRITE
// to grow the committed region in-place without changing the base address.
//
// oldSize is the current committed size and newSize is the desired size.
// Both must be page-aligned. The buffer must have been created by MmapLinearMemory.
func MmapGrowLinearMemory(buf []byte, oldSize, newSize uint64) error {
	if newSize <= oldSize {
		return nil
	}
	if newSize > uint64(len(buf))-GuardRegionSize {
		return fmt.Errorf("grow to %d exceeds reservation (%d)", newSize, uint64(len(buf))-GuardRegionSize)
	}
	return unix.Mprotect(buf[oldSize:newSize], unix.PROT_READ|unix.PROT_WRITE)
}

// MunmapLinearMemory releases the entire reserved virtual address range.
func MunmapLinearMemory(buf []byte) error {
	return unix.Munmap(buf)
}

// MmapLinearMemoryPtr returns the base pointer and full length or the reserved
// region. This is a convenience for constructing a []byte header without copying.
func MmapLinearMemoryPtr(buf []byte) (*byte, uintptr) {
	return (*byte)(unsafe.Pointer(&buf[0])), uintptr(len(buf))
}
