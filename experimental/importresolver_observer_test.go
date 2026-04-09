package experimental

import (
	"context"
	"testing"
)

type stubImportResolverObserver struct{}

func (*stubImportResolverObserver) ObserveImportResolution(context.Context, ImportResolverObservation) {
}

func TestWithImportResolverObserver_NilDoesNothing(t *testing.T) {
	ctx := context.Background()
	if result := WithImportResolverObserver(ctx, nil); result != ctx {
		t.Fatal("WithImportResolverObserver(ctx, nil) should return the same context")
	}
}

func TestGetImportResolverObserver_NotSet(t *testing.T) {
	if got := GetImportResolverObserver(context.Background()); got != nil {
		t.Fatal("GetImportResolverObserver on empty context should return nil")
	}
}

func TestGetImportResolverObserver_NilContext(t *testing.T) {
	if got := GetImportResolverObserver(nil); got != nil {
		t.Fatal("GetImportResolverObserver(nil) should return nil")
	}
}

func TestWithImportResolverObserver_TypedNilDoesNothing(t *testing.T) {
	ctx := context.Background()

	var funcObserver ImportResolverObserverFunc
	if result := WithImportResolverObserver(ctx, funcObserver); result != ctx {
		t.Fatal("WithImportResolverObserver should ignore typed-nil ImportResolverObserverFunc values")
	}

	var ptrObserver *stubImportResolverObserver
	if result := WithImportResolverObserver(ctx, ptrObserver); result != ctx {
		t.Fatal("WithImportResolverObserver should ignore typed-nil ImportResolverObserver values")
	}
}

func TestWithImportResolverObserver_RoundTrip(t *testing.T) {
	observer := ImportResolverObserverFunc(func(context.Context, ImportResolverObservation) {})

	got := GetImportResolverObserver(WithImportResolverObserver(context.Background(), observer))
	if got == nil {
		t.Fatal("GetImportResolverObserver should return non-nil")
	}
	if _, ok := got.(ImportResolverObserverFunc); !ok {
		t.Fatal("GetImportResolverObserver should return the registered observer type")
	}
}
