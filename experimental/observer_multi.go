package experimental

import "context"

// MultiTrapObserver constructs a TrapObserver which invokes each non-nil
// observer in order.
func MultiTrapObserver(observers ...TrapObserver) TrapObserver {
	filtered := compactObservers(observers, isNilTrapObserver)
	switch len(filtered) {
	case 0:
		return nil
	case 1:
		return filtered[0]
	default:
		return multiTrapObserver(filtered)
	}
}

type multiTrapObserver []TrapObserver

func (multi multiTrapObserver) ObserveTrap(ctx context.Context, observation TrapObservation) {
	for _, observer := range multi {
		observer.ObserveTrap(ctx, observation)
	}
}

// MultiYieldObserver constructs a YieldObserver which invokes each non-nil
// observer in order.
func MultiYieldObserver(observers ...YieldObserver) YieldObserver {
	filtered := compactObservers(observers, isNilYieldObserver)
	switch len(filtered) {
	case 0:
		return nil
	case 1:
		return filtered[0]
	default:
		return multiYieldObserver(filtered)
	}
}

type multiYieldObserver []YieldObserver

func (multi multiYieldObserver) ObserveYield(ctx context.Context, observation YieldObservation) {
	for _, observer := range multi {
		observer.ObserveYield(ctx, observation)
	}
}

// MultiFuelObserver constructs a FuelObserver which invokes each non-nil
// observer in order.
func MultiFuelObserver(observers ...FuelObserver) FuelObserver {
	filtered := compactObservers(observers, isNilFuelObserver)
	switch len(filtered) {
	case 0:
		return nil
	case 1:
		return filtered[0]
	default:
		return multiFuelObserver(filtered)
	}
}

type multiFuelObserver []FuelObserver

func (multi multiFuelObserver) ObserveFuel(ctx context.Context, observation FuelObservation) {
	for _, observer := range multi {
		observer.ObserveFuel(ctx, observation)
	}
}

// MultiHostCallPolicyObserver constructs a HostCallPolicyObserver which invokes
// each non-nil observer in order.
func MultiHostCallPolicyObserver(observers ...HostCallPolicyObserver) HostCallPolicyObserver {
	filtered := compactObservers(observers, isNilHostCallPolicyObserver)
	switch len(filtered) {
	case 0:
		return nil
	case 1:
		return filtered[0]
	default:
		return multiHostCallPolicyObserver(filtered)
	}
}

type multiHostCallPolicyObserver []HostCallPolicyObserver

func (multi multiHostCallPolicyObserver) ObserveHostCallPolicy(ctx context.Context, observation HostCallPolicyObservation) {
	for _, observer := range multi {
		observer.ObserveHostCallPolicy(ctx, observation)
	}
}

// MultiYieldPolicyObserver constructs a YieldPolicyObserver which invokes each
// non-nil observer in order.
func MultiYieldPolicyObserver(observers ...YieldPolicyObserver) YieldPolicyObserver {
	filtered := compactObservers(observers, isNilYieldPolicyObserver)
	switch len(filtered) {
	case 0:
		return nil
	case 1:
		return filtered[0]
	default:
		return multiYieldPolicyObserver(filtered)
	}
}

type multiYieldPolicyObserver []YieldPolicyObserver

func (multi multiYieldPolicyObserver) ObserveYieldPolicy(ctx context.Context, observation YieldPolicyObservation) {
	for _, observer := range multi {
		observer.ObserveYieldPolicy(ctx, observation)
	}
}

// MultiImportResolverObserver constructs an ImportResolverObserver which invokes
// each non-nil observer in order.
func MultiImportResolverObserver(observers ...ImportResolverObserver) ImportResolverObserver {
	filtered := compactObservers(observers, isNilImportResolverObserver)
	switch len(filtered) {
	case 0:
		return nil
	case 1:
		return filtered[0]
	default:
		return multiImportResolverObserver(filtered)
	}
}

type multiImportResolverObserver []ImportResolverObserver

func (multi multiImportResolverObserver) ObserveImportResolution(ctx context.Context, observation ImportResolverObservation) {
	for _, observer := range multi {
		observer.ObserveImportResolution(ctx, observation)
	}
}

func compactObservers[T any](observers []T, isNil func(T) bool) []T {
	filtered := make([]T, 0, len(observers))
	for _, observer := range observers {
		if !isNil(observer) {
			filtered = append(filtered, observer)
		}
	}
	return filtered
}
