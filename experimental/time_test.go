package experimental

import (
	"context"
	"testing"
)

type stubTimeProvider struct{}

func (*stubTimeProvider) Walltime() (int64, int32) { return 1, 2 }
func (*stubTimeProvider) Nanotime() int64          { return 3 }
func (*stubTimeProvider) Nanosleep(int64)          {}

func TestWithTimeProvider_NilDoesNothing(t *testing.T) {
	ctx := context.Background()
	result := WithTimeProvider(ctx, nil)
	if result != ctx {
		t.Fatal("WithTimeProvider(ctx, nil) should return the same context")
	}
}

func TestGetTimeProvider_NotSet(t *testing.T) {
	if got := GetTimeProvider(context.Background()); got != nil {
		t.Fatal("GetTimeProvider on empty context should return nil")
	}
}

func TestGetTimeProvider_NilContext(t *testing.T) {
	if got := GetTimeProvider(nil); got != nil {
		t.Fatal("GetTimeProvider(nil) should return nil")
	}
}

func TestWithTimeProvider_TypedNilDoesNothing(t *testing.T) {
	ctx := context.Background()

	var ptrProvider *stubTimeProvider
	if result := WithTimeProvider(ctx, ptrProvider); result != ctx {
		t.Fatal("WithTimeProvider should ignore typed-nil TimeProvider values")
	}
}

func TestWithTimeProvider_RoundTrip(t *testing.T) {
	provider := &stubTimeProvider{}

	got := GetTimeProvider(WithTimeProvider(context.Background(), provider))
	if got == nil {
		t.Fatal("GetTimeProvider should return non-nil")
	}

	sec, nsec := got.Walltime()
	if sec != 1 || nsec != 2 {
		t.Fatalf("round-tripped provider should preserve Walltime, got (%d, %d)", sec, nsec)
	}
	if got.Nanotime() != 3 {
		t.Fatalf("round-tripped provider should preserve Nanotime, got %d", got.Nanotime())
	}
}
