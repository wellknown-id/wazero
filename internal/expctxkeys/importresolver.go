package expctxkeys

// ImportResolverKey is a context.Context Value key.
// Its associated value should be an experimental.ImportResolver or
// experimental.ImportResolverConfig.
// See issue 2294.
type ImportResolverKey struct{}
