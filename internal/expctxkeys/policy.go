package expctxkeys

// HostCallPolicyKey is a context.Context key for the experimental host-call
// policy hook.
type HostCallPolicyKey struct{}

// HostCallPolicyObserverKey is a context.Context key for the experimental
// host-call policy observer.
type HostCallPolicyObserverKey struct{}

// YieldPolicyKey is a context.Context key for the experimental yield policy
// hook.
type YieldPolicyKey struct{}

// YieldPolicyObserverKey is a context.Context key for the experimental yield
// policy observer.
type YieldPolicyObserverKey struct{}
