//go:build linux && amd64

package wazevo

import (
	"sync"
	"sync/atomic"
	"syscall"
	"unsafe"
)

type jitCodeRange struct {
	start uintptr
	end   uintptr
}

const maxJITCodeRanges = 4096

var (
	jitRanges [maxJITCodeRanges]jitCodeRange
	// jitRangeCount is read directly by the assembly signal handler.
	jitRangeCount uint32
	jitRangeMu    sync.Mutex

	savedGoHandler uintptr

	signalHandlerInstalled atomic.Bool
)

type linuxSigaction struct {
	handler  uintptr
	flags    uint64
	restorer uintptr
	mask     uint64
}

// Assembly-defined functions (sighandler_linux_amd64.s).
func jitSigHandlerAddr() uintptr
func faultReturnTrampoline()

func signalHandlerSupported() bool {
	return true
}

// InstallSignalHandler saves Go's SIGSEGV handler and installs ours.
func InstallSignalHandler() {
	if signalHandlerInstalled.Load() {
		return
	}

	jitRangeMu.Lock()
	defer jitRangeMu.Unlock()

	if signalHandlerInstalled.Load() {
		return
	}

	var old linuxSigaction
	_, _, errno := syscall.RawSyscall6(
		syscall.SYS_RT_SIGACTION,
		uintptr(syscall.SIGSEGV),
		0,
		uintptr(unsafe.Pointer(&old)),
		8,
		0,
		0,
	)
	if errno != 0 {
		panic("wazevo: failed to read SIGSEGV handler: " + errno.Error())
	}

	savedGoHandler = old.handler

	act := old
	act.handler = jitSigHandlerAddr()

	_, _, errno = syscall.RawSyscall6(
		syscall.SYS_RT_SIGACTION,
		uintptr(syscall.SIGSEGV),
		uintptr(unsafe.Pointer(&act)),
		0,
		8,
		0,
		0,
	)
	if errno != 0 {
		panic("wazevo: failed to install SIGSEGV handler: " + errno.Error())
	}

	signalHandlerInstalled.Store(true)
}

// RegisterJITCodeRange registers a JIT code region for fault detection.
func RegisterJITCodeRange(start, end uintptr) {
	if start == 0 || end <= start {
		panic("wazevo: invalid JIT code range")
	}

	InstallSignalHandler()

	jitRangeMu.Lock()
	defer jitRangeMu.Unlock()

	n := atomic.LoadUint32(&jitRangeCount)
	for i := uint32(0); i < n; i++ {
		r := jitRanges[i]
		if r.start == start && r.end == end {
			return
		}
	}

	if n >= maxJITCodeRanges {
		panic("wazevo: too many JIT code ranges")
	}

	jitRanges[n] = jitCodeRange{start: start, end: end}
	atomic.StoreUint32(&jitRangeCount, n+1)
}
