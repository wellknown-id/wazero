package experimental

import (
	"context"
	"fmt"
	"testing"

	"github.com/tetratelabs/wazero/internal/wasmruntime"
)

type trapObserver struct{}

func (*trapObserver) ObserveTrap(context.Context, TrapObservation) {}

func TestWithTrapObserver_NilDoesNothing(t *testing.T) {
	ctx := context.Background()
	if result := WithTrapObserver(ctx, nil); result != ctx {
		t.Fatal("WithTrapObserver(ctx, nil) should return the same context")
	}
}

func TestWithTrapObserver_TypedNilDoesNothing(t *testing.T) {
	ctx := context.Background()

	var funcObserver TrapObserverFunc
	if result := WithTrapObserver(ctx, funcObserver); result != ctx {
		t.Fatal("WithTrapObserver(ctx, typed-nil func) should return the same context")
	}

	var ptrObserver *trapObserver
	if result := WithTrapObserver(ctx, ptrObserver); result != ctx {
		t.Fatal("WithTrapObserver(ctx, typed-nil pointer) should return the same context")
	}
}

func TestWithTrapObserver_RoundTrip(t *testing.T) {
	ctx := context.Background()
	observer := TrapObserverFunc(func(context.Context, TrapObservation) {})
	ctx = WithTrapObserver(ctx, observer)

	got := GetTrapObserver(ctx)
	if got == nil {
		t.Fatal("GetTrapObserver should return non-nil")
	}
	if _, ok := got.(TrapObserverFunc); !ok {
		t.Fatal("GetTrapObserver should return the registered observer type")
	}
}

func TestTrapCauseOf(t *testing.T) {
	tests := []struct {
		name  string
		err   error
		cause TrapCause
		ok    bool
	}{
		{name: "fuel exhausted", err: wasmruntime.ErrRuntimeFuelExhausted, cause: TrapCauseFuelExhausted, ok: true},
		{name: "policy denied", err: wasmruntime.ErrRuntimePolicyDenied, cause: TrapCausePolicyDenied, ok: true},
		{name: "memory fault", err: wasmruntime.ErrRuntimeMemoryFault, cause: TrapCauseMemoryFault, ok: true},
		{name: "wrapped", err: fmt.Errorf("wrapped: %w", wasmruntime.ErrRuntimeFuelExhausted), cause: TrapCauseFuelExhausted, ok: true},
		{name: "unknown", err: fmt.Errorf("other"), ok: false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cause, ok := TrapCauseOf(tt.err)
			if ok != tt.ok {
				t.Fatalf("TrapCauseOf() ok = %v, want %v", ok, tt.ok)
			}
			if cause != tt.cause {
				t.Fatalf("TrapCauseOf() cause = %q, want %q", cause, tt.cause)
			}
		})
	}
}
