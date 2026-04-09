package experimental

import (
	"context"
	"slices"
	"testing"

	"github.com/tetratelabs/wazero/api"
)

func TestMultiTrapObserver(t *testing.T) {
	ctx := context.Background()

	var nilFunc TrapObserverFunc
	var nilPtr *trapObserver

	if got := MultiTrapObserver(nil, nilFunc, nilPtr); got != nil {
		t.Fatal("MultiTrapObserver should return nil when all observers are nil")
	}

	single := &trapObserver{}
	if got := MultiTrapObserver(nil, single, nilPtr); got != single {
		t.Fatal("MultiTrapObserver should return the sole non-nil observer")
	}

	var calls []string
	observer := MultiTrapObserver(
		nil,
		nilFunc,
		nilPtr,
		TrapObserverFunc(func(context.Context, TrapObservation) { calls = append(calls, "first") }),
		TrapObserverFunc(func(context.Context, TrapObservation) { calls = append(calls, "second") }),
	)
	observer.ObserveTrap(ctx, TrapObservation{})

	if !slices.Equal(calls, []string{"first", "second"}) {
		t.Fatalf("MultiTrapObserver calls = %v, want [first second]", calls)
	}
}

func TestMultiHostCallPolicy(t *testing.T) {
	ctx := context.Background()

	var nilFunc HostCallPolicyFunc
	var nilPtr *stubHostCallPolicy

	if got := MultiHostCallPolicy(nil, nilFunc, nilPtr); got != nil {
		t.Fatal("MultiHostCallPolicy should return nil when all policies are nil")
	}

	single := &stubHostCallPolicy{}
	if got := MultiHostCallPolicy(nil, single, nilPtr); got != single {
		t.Fatal("MultiHostCallPolicy should return the sole non-nil policy")
	}

	var calls []string
	policy := MultiHostCallPolicy(
		nil,
		nilFunc,
		nilPtr,
		HostCallPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
			calls = append(calls, "first")
			return true
		}),
		HostCallPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
			calls = append(calls, "second")
			return true
		}),
	)
	if !policy.AllowHostCall(ctx, nil, nil) {
		t.Fatal("MultiHostCallPolicy should allow when all policies allow")
	}

	if !slices.Equal(calls, []string{"first", "second"}) {
		t.Fatalf("MultiHostCallPolicy calls = %v, want [first second]", calls)
	}
}

func TestMultiHostCallPolicy_ShortCircuitDeny(t *testing.T) {
	ctx := context.Background()

	var calls []string
	policy := MultiHostCallPolicy(
		HostCallPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
			calls = append(calls, "first")
			return true
		}),
		HostCallPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
			calls = append(calls, "second")
			return false
		}),
		HostCallPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
			calls = append(calls, "third")
			return true
		}),
	)
	if policy.AllowHostCall(ctx, nil, nil) {
		t.Fatal("MultiHostCallPolicy should deny when any policy denies")
	}

	if !slices.Equal(calls, []string{"first", "second"}) {
		t.Fatalf("MultiHostCallPolicy calls = %v, want [first second]", calls)
	}
}

func TestMultiYieldPolicy(t *testing.T) {
	ctx := context.Background()

	var nilFunc YieldPolicyFunc
	var nilPtr *stubYieldPolicy

	if got := MultiYieldPolicy(nil, nilFunc, nilPtr); got != nil {
		t.Fatal("MultiYieldPolicy should return nil when all policies are nil")
	}

	single := &stubYieldPolicy{}
	if got := MultiYieldPolicy(nil, single, nilPtr); got != single {
		t.Fatal("MultiYieldPolicy should return the sole non-nil policy")
	}

	var calls []string
	policy := MultiYieldPolicy(
		nil,
		nilFunc,
		nilPtr,
		YieldPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
			calls = append(calls, "first")
			return true
		}),
		YieldPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
			calls = append(calls, "second")
			return true
		}),
	)
	if !policy.AllowYield(ctx, nil, nil) {
		t.Fatal("MultiYieldPolicy should allow when all policies allow")
	}

	if !slices.Equal(calls, []string{"first", "second"}) {
		t.Fatalf("MultiYieldPolicy calls = %v, want [first second]", calls)
	}
}

func TestMultiYieldPolicy_ShortCircuitDeny(t *testing.T) {
	ctx := context.Background()

	var calls []string
	policy := MultiYieldPolicy(
		YieldPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
			calls = append(calls, "first")
			return true
		}),
		YieldPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
			calls = append(calls, "second")
			return false
		}),
		YieldPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool {
			calls = append(calls, "third")
			return true
		}),
	)
	if policy.AllowYield(ctx, nil, nil) {
		t.Fatal("MultiYieldPolicy should deny when any policy denies")
	}

	if !slices.Equal(calls, []string{"first", "second"}) {
		t.Fatalf("MultiYieldPolicy calls = %v, want [first second]", calls)
	}
}

func TestMultiYieldObserver(t *testing.T) {
	ctx := context.Background()

	var nilFunc YieldObserverFunc
	var nilPtr *yieldObserver

	if got := MultiYieldObserver(nil, nilFunc, nilPtr); got != nil {
		t.Fatal("MultiYieldObserver should return nil when all observers are nil")
	}

	single := &yieldObserver{}
	if got := MultiYieldObserver(nil, single, nilPtr); got != single {
		t.Fatal("MultiYieldObserver should return the sole non-nil observer")
	}

	var calls []string
	observer := MultiYieldObserver(
		nil,
		nilFunc,
		nilPtr,
		YieldObserverFunc(func(context.Context, YieldObservation) { calls = append(calls, "first") }),
		YieldObserverFunc(func(context.Context, YieldObservation) { calls = append(calls, "second") }),
	)
	observer.ObserveYield(ctx, YieldObservation{})

	if !slices.Equal(calls, []string{"first", "second"}) {
		t.Fatalf("MultiYieldObserver calls = %v, want [first second]", calls)
	}
}

func TestMultiFuelObserver(t *testing.T) {
	ctx := context.Background()

	var nilFunc FuelObserverFunc
	var nilPtr *recordingFuelObserver

	if got := MultiFuelObserver(nil, nilFunc, nilPtr); got != nil {
		t.Fatal("MultiFuelObserver should return nil when all observers are nil")
	}

	single := &recordingFuelObserver{}
	if got := MultiFuelObserver(nil, single, nilPtr); got != single {
		t.Fatal("MultiFuelObserver should return the sole non-nil observer")
	}

	var calls []string
	observer := MultiFuelObserver(
		nil,
		nilFunc,
		nilPtr,
		FuelObserverFunc(func(context.Context, FuelObservation) { calls = append(calls, "first") }),
		FuelObserverFunc(func(context.Context, FuelObservation) { calls = append(calls, "second") }),
	)
	observer.ObserveFuel(ctx, FuelObservation{})

	if !slices.Equal(calls, []string{"first", "second"}) {
		t.Fatalf("MultiFuelObserver calls = %v, want [first second]", calls)
	}
}

func TestMultiHostCallPolicyObserver(t *testing.T) {
	ctx := context.Background()

	var nilFunc HostCallPolicyObserverFunc
	var nilPtr *stubHostCallPolicyObserver

	if got := MultiHostCallPolicyObserver(nil, nilFunc, nilPtr); got != nil {
		t.Fatal("MultiHostCallPolicyObserver should return nil when all observers are nil")
	}

	single := &stubHostCallPolicyObserver{}
	if got := MultiHostCallPolicyObserver(nil, single, nilPtr); got != single {
		t.Fatal("MultiHostCallPolicyObserver should return the sole non-nil observer")
	}

	var calls []string
	observer := MultiHostCallPolicyObserver(
		nil,
		nilFunc,
		nilPtr,
		HostCallPolicyObserverFunc(func(context.Context, HostCallPolicyObservation) { calls = append(calls, "first") }),
		HostCallPolicyObserverFunc(func(context.Context, HostCallPolicyObservation) { calls = append(calls, "second") }),
	)
	observer.ObserveHostCallPolicy(ctx, HostCallPolicyObservation{})

	if !slices.Equal(calls, []string{"first", "second"}) {
		t.Fatalf("MultiHostCallPolicyObserver calls = %v, want [first second]", calls)
	}
}

func TestMultiYieldPolicyObserver(t *testing.T) {
	ctx := context.Background()

	var nilFunc YieldPolicyObserverFunc
	var nilPtr *stubYieldPolicyObserver

	if got := MultiYieldPolicyObserver(nil, nilFunc, nilPtr); got != nil {
		t.Fatal("MultiYieldPolicyObserver should return nil when all observers are nil")
	}

	single := &stubYieldPolicyObserver{}
	if got := MultiYieldPolicyObserver(nil, single, nilPtr); got != single {
		t.Fatal("MultiYieldPolicyObserver should return the sole non-nil observer")
	}

	var calls []string
	observer := MultiYieldPolicyObserver(
		nil,
		nilFunc,
		nilPtr,
		YieldPolicyObserverFunc(func(context.Context, YieldPolicyObservation) { calls = append(calls, "first") }),
		YieldPolicyObserverFunc(func(context.Context, YieldPolicyObservation) { calls = append(calls, "second") }),
	)
	observer.ObserveYieldPolicy(ctx, YieldPolicyObservation{})

	if !slices.Equal(calls, []string{"first", "second"}) {
		t.Fatalf("MultiYieldPolicyObserver calls = %v, want [first second]", calls)
	}
}

func TestMultiImportResolverObserver(t *testing.T) {
	ctx := context.Background()

	var nilFunc ImportResolverObserverFunc
	var nilPtr *stubImportResolverObserver

	if got := MultiImportResolverObserver(nil, nilFunc, nilPtr); got != nil {
		t.Fatal("MultiImportResolverObserver should return nil when all observers are nil")
	}

	single := &stubImportResolverObserver{}
	if got := MultiImportResolverObserver(nil, single, nilPtr); got != single {
		t.Fatal("MultiImportResolverObserver should return the sole non-nil observer")
	}

	var calls []string
	observer := MultiImportResolverObserver(
		nil,
		nilFunc,
		nilPtr,
		ImportResolverObserverFunc(func(context.Context, ImportResolverObservation) { calls = append(calls, "first") }),
		ImportResolverObserverFunc(func(context.Context, ImportResolverObservation) { calls = append(calls, "second") }),
	)
	observer.ObserveImportResolution(ctx, ImportResolverObservation{})

	if !slices.Equal(calls, []string{"first", "second"}) {
		t.Fatalf("MultiImportResolverObserver calls = %v, want [first second]", calls)
	}
}
