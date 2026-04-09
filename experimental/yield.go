package experimental

import (
	"context"
	"errors"

	"github.com/tetratelabs/wazero/internal/expctxkeys"
)

// Yielder allows host functions to cooperatively suspend Wasm execution.
// Obtained from within a host function via GetYielder(ctx).
//
// When a host function needs to perform asynchronous work (e.g., a network
// call, database query, or channel operation), it can call Yield() to suspend
// the Wasm execution without blocking a Go goroutine. The embedder receives
// a Resumer handle via a *YieldError and can later continue execution from
// any goroutine.
//
// Example (from a host function):
//
//	func myAsyncHostFn(ctx context.Context, mod api.Module, stack []uint64) {
//	    yielder := experimental.GetYielder(ctx)
//	    if yielder == nil {
//	        // fallback: yield not enabled, do synchronous work
//	        result := doWorkSync()
//	        stack[0] = result
//	        return
//	    }
//	    // Yield execution — this function returns, freeing the goroutine.
//	    // The embedder will receive a *YieldError from Call().
//	    yielder.Yield()
//	    // NOTE: code after Yield() is unreachable. The host function's
//	    // return values are provided via Resumer.Resume().
//	}
type Yielder interface {
	// Yield suspends the current Wasm execution and returns a Resumer
	// that can be used to continue execution later.
	//
	// Yield does not return to the caller. It unwinds the host function's
	// Go stack frame via panic, which is recovered by the call engine.
	// The calling host function MUST NOT defer any cleanup that depends
	// on the Wasm module's state after Yield is called.
	//
	// Panics if called outside a host function scope or if yield/resume
	// is not enabled.
	Yield()
}

// Resumer is a handle for resuming a previously yielded Wasm execution.
// It is safe to pass between goroutines.
//
// A Resumer must be either Resumed or Cancelled. Failure to do so will
// leak the captured execution state.
type Resumer interface {
	// Resume continues the suspended execution. hostResults are the
	// return values that the yielding host function would have produced.
	// len(hostResults) must exactly match that host function's result arity.
	//
	// ctx governs the resumed execution, allowing the embedder to set
	// new deadlines, fuel controllers, or other context values.
	// It must not be nil.
	//
	// Returns the final Wasm function results and nil error on success.
	// Returns (nil, *YieldError) if the execution yields again, in which
	// case a new Resumer is available via the returned error.
	//
	// Returns an error if called with a nil context, after Cancel, after the
	// suspended module has been closed, or with the wrong number of hostResults.
	// Panics if called concurrently or more than once.
	Resume(ctx context.Context, hostResults []uint64) ([]uint64, error)

	// Cancel releases the captured execution state without resuming.
	// After Cancel, the Resumer must not be used.
	//
	// Cancel is safe to call multiple times (subsequent calls are no-ops).
	// If Resume has already started, Cancel is also a no-op.
	Cancel()
}

// YieldError is returned by api.Function.Call when the Wasm execution
// cooperatively yields via the async yield protocol. It carries a Resumer
// that can be used to continue execution later, possibly from a different
// goroutine.
//
// The embedder should check for *YieldError using errors.As:
//
//	results, err := fn.Call(ctx, params...)
//	var yieldErr *experimental.YieldError
//	if errors.As(err, &yieldErr) {
//	    // Module yielded. Arrange async work, then resume later.
//	    go func() {
//	        result := doAsyncWork()
//	        results, err := yieldErr.Resumer().Resume(newCtx, []uint64{result})
//	        // handle results/err
//	    }()
//	    return
//	}
type YieldError struct {
	resumer Resumer
}

// Error implements error.
func (e *YieldError) Error() string { return "wasm execution yielded" }

// Resumer returns the Resumer for this yielded execution.
func (e *YieldError) Resumer() Resumer { return e.resumer }

// NewYieldError creates a YieldError with the given Resumer.
// This is intended for use by engine implementations; embedders should
// not call this directly.
func NewYieldError(r Resumer) *YieldError {
	return &YieldError{resumer: r}
}

// ErrYielded is a sentinel error that can be used with errors.Is to check
// whether a Call error is a yield.
var ErrYielded = errors.New("wasm execution yielded")

// Is implements errors.Is so that errors.Is(yieldErr, ErrYielded) returns true.
func (e *YieldError) Is(target error) bool {
	return target == ErrYielded
}

// WithYielder enables async yield/resume for function calls made with the
// returned context. This is analogous to WithSnapshotter.
//
// Passing the returned context to an exported function invocation enables
// yield/resume and allows host functions to retrieve the Yielder using
// GetYielder.
func WithYielder(ctx context.Context) context.Context {
	return context.WithValue(ctx, expctxkeys.EnableYielderKey{}, struct{}{})
}

// GetYielder returns the Yielder from a host function's context, or nil
// if yield/resume is not enabled.
//
// The Yielder is only present during host function execution when the
// function invocation context had WithYielder applied.
func GetYielder(ctx context.Context) Yielder {
	y, _ := ctx.Value(expctxkeys.YielderKey{}).(Yielder)
	return y
}
