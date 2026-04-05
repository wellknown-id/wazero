//go:build !linux || (!amd64 && !arm64)

package wazevo

func signalHandlerSupported() bool {
	return false
}

func InstallSignalHandler() {}

func RegisterJITCodeRange(start, end uintptr) {
	_, _ = start, end
}
