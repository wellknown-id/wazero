package experimental

import (
	"context"
	"reflect"

	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/internal/expctxkeys"
)

// ImportResolverEvent identifies an import-resolution decision during module
// instantiation.
type ImportResolverEvent string

const (
	ImportResolverEventACLAllowed       ImportResolverEvent = "acl_allowed"
	ImportResolverEventACLDenied        ImportResolverEvent = "acl_denied"
	ImportResolverEventResolverResolved ImportResolverEvent = "resolver_resolved"
	ImportResolverEventStoreFallback    ImportResolverEvent = "store_fallback"
	ImportResolverEventFailClosedDenied ImportResolverEvent = "fail_closed_denied"
)

// ImportResolverObservation describes an opt-in import-resolution decision.
type ImportResolverObservation struct {
	Module         api.Module
	ImportModule   string
	ResolvedModule api.Module
	Event          ImportResolverEvent
}

// ImportResolverObserver receives opt-in notifications for import-resolution
// decisions during instantiation.
type ImportResolverObserver interface {
	ObserveImportResolution(ctx context.Context, observation ImportResolverObservation)
}

// ImportResolverObserverFunc adapts a function into an ImportResolverObserver.
type ImportResolverObserverFunc func(context.Context, ImportResolverObservation)

// ObserveImportResolution implements ImportResolverObserver.
func (f ImportResolverObserverFunc) ObserveImportResolution(ctx context.Context, observation ImportResolverObservation) {
	if f != nil {
		f(ctx, observation)
	}
}

// WithImportResolverObserver returns a derived context with the given
// ImportResolverObserver.
//
// If observer is nil, including a typed-nil ImportResolverObserver value, ctx
// is returned unchanged.
func WithImportResolverObserver(ctx context.Context, observer ImportResolverObserver) context.Context {
	if isNilImportResolverObserver(observer) {
		return ctx
	}
	return context.WithValue(ctx, expctxkeys.ImportResolverObserverKey{}, observer)
}

// GetImportResolverObserver returns the ImportResolverObserver from ctx, or nil
// if none is set.
func GetImportResolverObserver(ctx context.Context) ImportResolverObserver {
	if ctx == nil {
		return nil
	}
	observer, _ := ctx.Value(expctxkeys.ImportResolverObserverKey{}).(ImportResolverObserver)
	if isNilImportResolverObserver(observer) {
		return nil
	}
	return observer
}

func isNilImportResolverObserver(observer ImportResolverObserver) bool {
	if observer == nil {
		return true
	}
	v := reflect.ValueOf(observer)
	switch v.Kind() {
	case reflect.Chan, reflect.Func, reflect.Interface, reflect.Map, reflect.Pointer, reflect.Slice:
		return v.IsNil()
	default:
		return false
	}
}
