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
