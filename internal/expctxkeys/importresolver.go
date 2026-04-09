package expctxkeys

// ImportResolverKey is a context.Context Value key.
// Its associated value should be an experimental.ImportResolver or
// experimental.ImportResolverConfig.
// See issue 2294.
type ImportResolverKey struct{}

// ImportResolverObserverKey is a context.Context key for the experimental
// import-resolution observer.
type ImportResolverObserverKey struct{}
