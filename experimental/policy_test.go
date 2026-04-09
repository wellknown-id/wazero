package experimental

import (
	"context"
	"testing"

	"github.com/tetratelabs/wazero/api"
)

type stubHostCallPolicy struct{}

func (*stubHostCallPolicy) AllowHostCall(context.Context, api.Module, api.FunctionDefinition) bool {
	return false
}

type stubHostCallPolicyObserver struct{}

func (*stubHostCallPolicyObserver) ObserveHostCallPolicy(context.Context, HostCallPolicyObservation) {
}

type stubYieldPolicyObserver struct{}

func (*stubYieldPolicyObserver) ObserveYieldPolicy(context.Context, YieldPolicyObservation) {
}

type stubYieldPolicy struct{}

func (*stubYieldPolicy) AllowYield(context.Context, api.Module, api.FunctionDefinition) bool {
	return false
}

func TestWithHostCallPolicy_NilDoesNothing(t *testing.T) {
	ctx := context.Background()
	result := WithHostCallPolicy(ctx, nil)
	if result != ctx {
		t.Fatal("WithHostCallPolicy(ctx, nil) should return the same context")
	}
}

func TestGetHostCallPolicy_NotSet(t *testing.T) {
	if got := GetHostCallPolicy(context.Background()); got != nil {
		t.Fatal("GetHostCallPolicy on empty context should return nil")
	}
}

func TestGetHostCallPolicy_NilContext(t *testing.T) {
	if got := GetHostCallPolicy(nil); got != nil {
		t.Fatal("GetHostCallPolicy(nil) should return nil")
	}
}

func TestWithHostCallPolicy_TypedNilDoesNothing(t *testing.T) {
	ctx := context.Background()

	var funcPolicy HostCallPolicyFunc
	if result := WithHostCallPolicy(ctx, funcPolicy); result != ctx {
		t.Fatal("WithHostCallPolicy should ignore typed-nil HostCallPolicyFunc values")
	}

	var ptrPolicy *stubHostCallPolicy
	if result := WithHostCallPolicy(ctx, ptrPolicy); result != ctx {
		t.Fatal("WithHostCallPolicy should ignore typed-nil HostCallPolicy values")
	}
}

func TestWithHostCallPolicy_RoundTrip(t *testing.T) {
	called := false
	policy := HostCallPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
		called = true
		return false
	})

	got := GetHostCallPolicy(WithHostCallPolicy(context.Background(), policy))
	if got == nil {
		t.Fatal("GetHostCallPolicy should return non-nil")
	}
	if got.AllowHostCall(context.Background(), nil, nil) {
		t.Fatal("round-tripped policy should preserve deny result")
	}
	if !called {
		t.Fatal("round-tripped policy should be invoked")
	}
}

func TestHostCallPolicyFunc_NilAllows(t *testing.T) {
	var policy HostCallPolicyFunc
	if !policy.AllowHostCall(context.Background(), nil, nil) {
		t.Fatal("nil HostCallPolicyFunc should behave as absent and allow the call")
	}
}

func TestWithHostCallPolicyObserver_NilDoesNothing(t *testing.T) {
	ctx := context.Background()
	if result := WithHostCallPolicyObserver(ctx, nil); result != ctx {
		t.Fatal("WithHostCallPolicyObserver(ctx, nil) should return the same context")
	}
}

func TestGetHostCallPolicyObserver_NotSet(t *testing.T) {
	if got := GetHostCallPolicyObserver(context.Background()); got != nil {
		t.Fatal("GetHostCallPolicyObserver on empty context should return nil")
	}
}

func TestGetHostCallPolicyObserver_NilContext(t *testing.T) {
	if got := GetHostCallPolicyObserver(nil); got != nil {
		t.Fatal("GetHostCallPolicyObserver(nil) should return nil")
	}
}

func TestWithHostCallPolicyObserver_TypedNilDoesNothing(t *testing.T) {
	ctx := context.Background()

	var funcObserver HostCallPolicyObserverFunc
	if result := WithHostCallPolicyObserver(ctx, funcObserver); result != ctx {
		t.Fatal("WithHostCallPolicyObserver should ignore typed-nil HostCallPolicyObserverFunc values")
	}

	var ptrObserver *stubHostCallPolicyObserver
	if result := WithHostCallPolicyObserver(ctx, ptrObserver); result != ctx {
		t.Fatal("WithHostCallPolicyObserver should ignore typed-nil HostCallPolicyObserver values")
	}
}

func TestWithHostCallPolicyObserver_RoundTrip(t *testing.T) {
	observer := HostCallPolicyObserverFunc(func(context.Context, HostCallPolicyObservation) {})

	got := GetHostCallPolicyObserver(WithHostCallPolicyObserver(context.Background(), observer))
	if got == nil {
		t.Fatal("GetHostCallPolicyObserver should return non-nil")
	}
	if _, ok := got.(HostCallPolicyObserverFunc); !ok {
		t.Fatal("GetHostCallPolicyObserver should return the registered observer type")
	}
}

func TestWithYieldPolicy_NilDoesNothing(t *testing.T) {
	ctx := context.Background()
	result := WithYieldPolicy(ctx, nil)
	if result != ctx {
		t.Fatal("WithYieldPolicy(ctx, nil) should return the same context")
	}
}

func TestGetYieldPolicy_NotSet(t *testing.T) {
	if got := GetYieldPolicy(context.Background()); got != nil {
		t.Fatal("GetYieldPolicy on empty context should return nil")
	}
}

func TestGetYieldPolicy_NilContext(t *testing.T) {
	if got := GetYieldPolicy(nil); got != nil {
		t.Fatal("GetYieldPolicy(nil) should return nil")
	}
}

func TestWithYieldPolicy_TypedNilDoesNothing(t *testing.T) {
	ctx := context.Background()

	var funcPolicy YieldPolicyFunc
	if result := WithYieldPolicy(ctx, funcPolicy); result != ctx {
		t.Fatal("WithYieldPolicy should ignore typed-nil YieldPolicyFunc values")
	}

	var ptrPolicy *stubYieldPolicy
	if result := WithYieldPolicy(ctx, ptrPolicy); result != ctx {
		t.Fatal("WithYieldPolicy should ignore typed-nil YieldPolicy values")
	}
}

func TestWithYieldPolicy_RoundTrip(t *testing.T) {
	called := false
	policy := YieldPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
		called = true
		return false
	})

	got := GetYieldPolicy(WithYieldPolicy(context.Background(), policy))
	if got == nil {
		t.Fatal("GetYieldPolicy should return non-nil")
	}
	if got.AllowYield(context.Background(), nil, nil) {
		t.Fatal("round-tripped policy should preserve deny result")
	}
	if !called {
		t.Fatal("round-tripped policy should be invoked")
	}
}

func TestYieldPolicyFunc_NilAllows(t *testing.T) {
	var policy YieldPolicyFunc
	if !policy.AllowYield(context.Background(), nil, nil) {
		t.Fatal("nil YieldPolicyFunc should behave as absent and allow the yield")
	}
}

func TestWithYieldPolicyObserver_NilDoesNothing(t *testing.T) {
	ctx := context.Background()
	if result := WithYieldPolicyObserver(ctx, nil); result != ctx {
		t.Fatal("WithYieldPolicyObserver(ctx, nil) should return the same context")
	}
}

func TestGetYieldPolicyObserver_NotSet(t *testing.T) {
	if got := GetYieldPolicyObserver(context.Background()); got != nil {
		t.Fatal("GetYieldPolicyObserver on empty context should return nil")
	}
}

func TestGetYieldPolicyObserver_NilContext(t *testing.T) {
	if got := GetYieldPolicyObserver(nil); got != nil {
		t.Fatal("GetYieldPolicyObserver(nil) should return nil")
	}
}

func TestWithYieldPolicyObserver_TypedNilDoesNothing(t *testing.T) {
	ctx := context.Background()

	var funcObserver YieldPolicyObserverFunc
	if result := WithYieldPolicyObserver(ctx, funcObserver); result != ctx {
		t.Fatal("WithYieldPolicyObserver should ignore typed-nil YieldPolicyObserverFunc values")
	}

	var ptrObserver *stubYieldPolicyObserver
	if result := WithYieldPolicyObserver(ctx, ptrObserver); result != ctx {
		t.Fatal("WithYieldPolicyObserver should ignore typed-nil YieldPolicyObserver values")
	}
}

func TestWithYieldPolicyObserver_RoundTrip(t *testing.T) {
	observer := YieldPolicyObserverFunc(func(context.Context, YieldPolicyObservation) {})

	got := GetYieldPolicyObserver(WithYieldPolicyObserver(context.Background(), observer))
	if got == nil {
		t.Fatal("GetYieldPolicyObserver should return non-nil")
	}
	if _, ok := got.(YieldPolicyObserverFunc); !ok {
		t.Fatal("GetYieldPolicyObserver should return the registered observer type")
	}
}
