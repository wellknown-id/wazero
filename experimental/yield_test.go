package experimental

import (
	"context"
	"errors"
	"testing"
)

func TestYieldError_Error(t *testing.T) {
	ye := NewYieldError(nil)
	if got := ye.Error(); got != "wasm execution yielded" {
		t.Fatalf("YieldError.Error() = %q, want %q", got, "wasm execution yielded")
	}
}

func TestYieldError_Is(t *testing.T) {
	ye := NewYieldError(nil)
	if !errors.Is(ye, ErrYielded) {
		t.Fatal("errors.Is(YieldError, ErrYielded) should be true")
	}
}

func TestYieldError_Resumer(t *testing.T) {
	// Resumer should be nil when constructed with nil.
	ye := NewYieldError(nil)
	if ye.Resumer() != nil {
		t.Fatal("expected nil Resumer")
	}
}

func TestWithYielder_RoundTrip(t *testing.T) {
	ctx := context.Background()

	// Before WithYielder, GetYielder should return nil.
	if got := GetYielder(ctx); got != nil {
		t.Fatal("GetYielder on plain context should return nil")
	}

	// After WithYielder, GetYielder still returns nil because the engine
	// hasn't set the YielderKey yet — WithYielder only sets the enablement key.
	ctx = WithYielder(ctx)
	if got := GetYielder(ctx); got != nil {
		t.Fatal("GetYielder after WithYielder should return nil (yielder set by engine)")
	}
}

func TestGetYielder_NotSet(t *testing.T) {
	ctx := context.Background()
	if got := GetYielder(ctx); got != nil {
		t.Fatal("GetYielder on empty context should return nil")
	}
}
