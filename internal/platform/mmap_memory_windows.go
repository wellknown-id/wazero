package platform

import (
	"fmt"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	// GuardRegionSize is the size of the guard region placed after the maximum
	// linear memory. 4 GiB matches Wasmtime's strategy.
	GuardRegionSize = 4 << 30 // 4 GiB
)

// SupportsGuardPages reports whether guard-page memory isolation is supported.
func SupportsGuardPages() bool {
	return true
}

// MmapLinearMemory reserves a contiguous virtual address range with guard pages
// for WebAssembly linear memory on Windows.
//
// It reserves (maxBytes + GuardRegionSize) bytes via MEM_RESERVE, then commits
// the first commitBytes via MEM_COMMIT with PAGE_READWRITE.
//
// Out-of-bounds access into uncommitted pages triggers EXCEPTION_IN_PAGE_ERROR,
// which Go's runtime.SetPanicOnFault converts to a recoverable panic.
func MmapLinearMemory(reserveBytes, commitBytes uint64) ([]byte, error) {
	if commitBytes > reserveBytes {
		return nil, fmt.Errorf("commitBytes (%d) exceeds reserveBytes (%d)", commitBytes, reserveBytes)
	}

	totalReserve := reserveBytes + GuardRegionSize

	// Reserve the entire range without committing.
	base, err := windows.VirtualAlloc(0, uintptr(totalReserve), windows.MEM_RESERVE, windows.PAGE_NOACCESS)
	if err != nil {
		return nil, fmt.Errorf("VirtualAlloc reserve failed: %w", err)
	}

	// Commit the initial region.
	if commitBytes > 0 {
		_, err := windows.VirtualAlloc(base, uintptr(commitBytes), windows.MEM_COMMIT, windows.PAGE_READWRITE)
		if err != nil {
			_ = windows.VirtualFree(base, 0, windows.MEM_RELEASE)
			return nil, fmt.Errorf("VirtualAlloc commit failed: %w", err)
		}
	}

	return unsafe.Slice((*byte)(unsafe.Pointer(base)), int(totalReserve)), nil
}

// MmapGrowLinearMemory commits additional pages from the reserved region.
func MmapGrowLinearMemory(buf []byte, oldSize, newSize uint64) error {
	if newSize <= oldSize {
		return nil
	}
	maxReservation := uint64(len(buf)) - GuardRegionSize
	if newSize > maxReservation {
		return fmt.Errorf("grow to %d exceeds reservation (%d)", newSize, maxReservation)
	}

	base := uintptr(unsafe.Pointer(&buf[0]))
	_, err := windows.VirtualAlloc(base+uintptr(oldSize), uintptr(newSize-oldSize),
		windows.MEM_COMMIT, windows.PAGE_READWRITE)
	if err != nil {
		return fmt.Errorf("VirtualAlloc grow commit failed: %w", err)
	}
	return nil
}

// MunmapLinearMemory releases the entire reserved virtual address range.
func MunmapLinearMemory(buf []byte) error {
	base := uintptr(unsafe.Pointer(&buf[0]))
	return windows.VirtualFree(base, 0, windows.MEM_RELEASE)
}

// MmapLinearMemoryPtr returns the base pointer and full length.
func MmapLinearMemoryPtr(buf []byte) (*byte, uintptr) {
	return (*byte)(unsafe.Pointer(&buf[0])), uintptr(len(buf))
}
