package expctxkeys

// EnableYielderKey is a context.Context key to indicate that async yield/resume
// should be enabled. The context.Context passed to an exported function invocation
// should have this key set to a non-nil value, and host functions will be able to
// retrieve the Yielder using YielderKey.
type EnableYielderKey struct{}

// YielderKey is a context.Context key to access a Yielder from a host function.
// It is only present if EnableYielderKey was set in the function invocation context.
type YielderKey struct{}

// YieldObserverKey is a context.Context key for the experimental yield observer.
type YieldObserverKey struct{}
