//go:build !(unix || windows)

package platform

import (
	"fmt"
	"runtime"
)

const (
	// GuardRegionSize is defined for interface compatibility but is not
	// used on unsupported platforms.
	GuardRegionSize = 4 << 30
)

var errGuardPagesUnsupported = fmt.Errorf(
	"guard-page memory isolation is not supported on GOOS=%s GOARCH=%s; "+
		"secure mode will use software bounds checks only",
	runtime.GOOS, runtime.GOARCH,
)

// SupportsGuardPages reports that guard pages are not available.
func SupportsGuardPages() bool {
	return false
}

// MmapLinearMemory is not supported on this platform.
func MmapLinearMemory(reserveBytes, commitBytes uint64) ([]byte, error) {
	return nil, errGuardPagesUnsupported
}

// MmapGrowLinearMemory is not supported on this platform.
func MmapGrowLinearMemory(buf []byte, oldSize, newSize uint64) error {
	return errGuardPagesUnsupported
}

// MunmapLinearMemory is not supported on this platform.
func MunmapLinearMemory(buf []byte) error {
	return errGuardPagesUnsupported
}

// MmapLinearMemoryPtr panics on unsupported platforms.
func MmapLinearMemoryPtr(buf []byte) (*byte, uintptr) {
	panic(errGuardPagesUnsupported)
}
