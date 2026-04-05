package experimental

import (
	"context"
	"testing"
)

func TestSimpleFuelController_Budget(t *testing.T) {
	ctrl := NewSimpleFuelController(42)
	if got := ctrl.Budget(); got != 42 {
		t.Fatalf("Budget() = %d, want 42", got)
	}
}

func TestSimpleFuelController_Consumed(t *testing.T) {
	ctrl := NewSimpleFuelController(1000)
	ctrl.Consumed(100)
	ctrl.Consumed(200)
	if got := ctrl.TotalConsumed(); got != 300 {
		t.Fatalf("TotalConsumed() = %d, want 300", got)
	}
}

func TestSimpleFuelController_ConcurrentConsumption(t *testing.T) {
	ctrl := NewSimpleFuelController(1_000_000)
	const goroutines = 10
	const perGoroutine = 1000
	done := make(chan struct{})
	for i := 0; i < goroutines; i++ {
		go func() {
			for j := 0; j < perGoroutine; j++ {
				ctrl.Consumed(1)
			}
			done <- struct{}{}
		}()
	}
	for i := 0; i < goroutines; i++ {
		<-done
	}
	if got := ctrl.TotalConsumed(); got != goroutines*perGoroutine {
		t.Fatalf("TotalConsumed() = %d, want %d", got, goroutines*perGoroutine)
	}
}

func TestAggregatingFuelController_Budget(t *testing.T) {
	parent := NewSimpleFuelController(1_000_000)
	child := NewAggregatingFuelController(parent, 100_000)
	if got := child.Budget(); got != 100_000 {
		t.Fatalf("child.Budget() = %d, want 100000", got)
	}
	// Parent budget is independent.
	if got := parent.Budget(); got != 1_000_000 {
		t.Fatalf("parent.Budget() = %d, want 1000000", got)
	}
}

func TestAggregatingFuelController_Consumed(t *testing.T) {
	parent := NewSimpleFuelController(1_000_000)
	child := NewAggregatingFuelController(parent, 100_000)

	child.Consumed(500)

	// Child tracks its own consumption.
	if got := child.TotalConsumed(); got != 500 {
		t.Fatalf("child.TotalConsumed() = %d, want 500", got)
	}
	// Parent also sees the consumption.
	if got := parent.TotalConsumed(); got != 500 {
		t.Fatalf("parent.TotalConsumed() = %d, want 500", got)
	}
}

func TestAggregatingFuelController_NestedAggregation(t *testing.T) {
	root := NewSimpleFuelController(10_000_000)
	alice := NewAggregatingFuelController(root, 1_000_000)
	bob := NewAggregatingFuelController(alice, 100_000)

	bob.Consumed(42)
	alice.Consumed(100)

	// Bob sees only his own.
	if got := bob.TotalConsumed(); got != 42 {
		t.Fatalf("bob.TotalConsumed() = %d, want 42", got)
	}
	// Alice sees bob's + her own.
	if got := alice.TotalConsumed(); got != 142 {
		t.Fatalf("alice.TotalConsumed() = %d, want 142", got)
	}
	// Root sees everything.
	if got := root.TotalConsumed(); got != 142 {
		t.Fatalf("root.TotalConsumed() = %d, want 142", got)
	}
}

func TestAggregatingFuelController_NilParent(t *testing.T) {
	// Should not panic.
	ctrl := NewAggregatingFuelController(nil, 1000)
	ctrl.Consumed(500)
	if got := ctrl.TotalConsumed(); got != 500 {
		t.Fatalf("TotalConsumed() = %d, want 500", got)
	}
}

func TestWithFuelController_NilDoesNothing(t *testing.T) {
	ctx := context.Background()
	result := WithFuelController(ctx, nil)
	if result != ctx {
		t.Fatal("WithFuelController(ctx, nil) should return the same context")
	}
}

func TestGetFuelController_NotSet(t *testing.T) {
	ctx := context.Background()
	fc := GetFuelController(ctx)
	if fc != nil {
		t.Fatal("GetFuelController on empty context should return nil")
	}
}

func TestWithFuelController_RoundTrip(t *testing.T) {
	ctx := context.Background()
	ctrl := NewSimpleFuelController(42)
	ctx = WithFuelController(ctx, ctrl)

	got := GetFuelController(ctx)
	if got == nil {
		t.Fatal("GetFuelController should return non-nil")
	}
	if got.Budget() != 42 {
		t.Fatalf("Budget() = %d, want 42", got.Budget())
	}
}

func TestWithFuelController_Override(t *testing.T) {
	ctx := context.Background()
	ctrl1 := NewSimpleFuelController(100)
	ctx = WithFuelController(ctx, ctrl1)

	ctrl2 := NewSimpleFuelController(200)
	ctx = WithFuelController(ctx, ctrl2)

	got := GetFuelController(ctx)
	if got.Budget() != 200 {
		t.Fatalf("Budget() = %d, want 200 (overridden)", got.Budget())
	}
}
