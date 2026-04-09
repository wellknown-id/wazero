package experimental

import (
	"context"
	"fmt"
	"reflect"
	"strings"

	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/internal/expctxkeys"
)

// ImportResolver is an experimental func type that, if set,
// will be used as the first step in resolving imports.
// See issue 2294.
// If the import name is not found, it should return nil.
type ImportResolver func(name string) api.Module

// ImportResolverConfig controls import resolution during instantiation.
//
// Resolver is consulted before the runtime falls back to resolving imports from
// instantiated modules in the store. ACL is evaluated before any resolver or
// store lookup, allowing embedders to express common allow/deny rules without
// replacing the resolver. When FailClosed is true, unresolved imports do not
// fall back to the store: if Resolver is nil or returns nil, instantiation
// fails.
type ImportResolverConfig struct {
	Resolver   ImportResolver
	ACL        *ImportACL
	FailClosed bool
}

// ImportACL is a small helper for common import allow/deny rules.
//
// Zero value semantics are "allow everything". Deny rules take precedence over
// allow rules. When any allow rule is present, imports that do not match an
// allow rule are denied.
type ImportACL struct {
	allowModules  map[string]struct{}
	allowPrefixes []string
	denyModules   map[string]struct{}
	denyPrefixes  []string
}

// NewImportACL returns a new ImportACL.
func NewImportACL() *ImportACL {
	return &ImportACL{}
}

// AllowModules adds exact-match module names to the allow list.
func (acl *ImportACL) AllowModules(names ...string) *ImportACL {
	if acl == nil {
		return nil
	}
	if acl.allowModules == nil {
		acl.allowModules = map[string]struct{}{}
	}
	for _, name := range names {
		if name == "" {
			continue
		}
		acl.allowModules[name] = struct{}{}
	}
	return acl
}

// AllowModulePrefixes adds module name prefixes to the allow list.
func (acl *ImportACL) AllowModulePrefixes(prefixes ...string) *ImportACL {
	if acl == nil {
		return nil
	}
	for _, prefix := range prefixes {
		if prefix == "" {
			continue
		}
		acl.allowPrefixes = append(acl.allowPrefixes, prefix)
	}
	return acl
}

// DenyModules adds exact-match module names to the deny list.
func (acl *ImportACL) DenyModules(names ...string) *ImportACL {
	if acl == nil {
		return nil
	}
	if acl.denyModules == nil {
		acl.denyModules = map[string]struct{}{}
	}
	for _, name := range names {
		if name == "" {
			continue
		}
		acl.denyModules[name] = struct{}{}
	}
	return acl
}

// DenyModulePrefixes adds module name prefixes to the deny list.
func (acl *ImportACL) DenyModulePrefixes(prefixes ...string) *ImportACL {
	if acl == nil {
		return nil
	}
	for _, prefix := range prefixes {
		if prefix == "" {
			continue
		}
		acl.denyPrefixes = append(acl.denyPrefixes, prefix)
	}
	return acl
}

// WithImportResolverACL returns a derived context with the given ImportACL.
//
// If acl is nil or empty, ctx is returned unchanged. Existing import resolver
// settings on ctx are preserved.
func WithImportResolverACL(ctx context.Context, acl *ImportACL) context.Context {
	if acl == nil || acl.isEmpty() {
		return ctx
	}
	cfg := GetImportResolverConfig(ctx)
	if cfg == nil {
		return context.WithValue(ctx, expctxkeys.ImportResolverKey{}, ImportResolverConfig{ACL: acl})
	}
	next := *cfg
	next.ACL = acl
	return context.WithValue(ctx, expctxkeys.ImportResolverKey{}, next)
}

// WithImportResolver returns a new context with the given ImportResolver.
func WithImportResolver(ctx context.Context, resolver ImportResolver) context.Context {
	if isNilImportResolver(resolver) {
		return ctx
	}
	return context.WithValue(ctx, expctxkeys.ImportResolverKey{}, resolver)
}

// GetImportResolver returns the ImportResolver from ctx, or nil if none is set.
//
// If ctx contains an ImportResolverConfig, its Resolver is returned.
func GetImportResolver(ctx context.Context) ImportResolver {
	if ctx == nil {
		return nil
	}
	switch v := ctx.Value(expctxkeys.ImportResolverKey{}).(type) {
	case ImportResolver:
		if isNilImportResolver(v) {
			return nil
		}
		return v
	case ImportResolverConfig:
		if isNilImportResolver(v.Resolver) {
			return nil
		}
		return v.Resolver
	default:
		return nil
	}
}

// WithImportResolverConfig returns a derived context with the given
// ImportResolverConfig.
//
// If cfg is empty, ctx is returned unchanged.
func WithImportResolverConfig(ctx context.Context, cfg ImportResolverConfig) context.Context {
	if cfg.isEmpty() {
		return ctx
	}
	return context.WithValue(ctx, expctxkeys.ImportResolverKey{}, cfg)
}

// GetImportResolverConfig returns the ImportResolverConfig from ctx, or nil if
// none is set.
//
// If ctx contains only an ImportResolver, it is surfaced as a config with
// FailClosed set to false.
func GetImportResolverConfig(ctx context.Context) *ImportResolverConfig {
	if ctx == nil {
		return nil
	}
	switch v := ctx.Value(expctxkeys.ImportResolverKey{}).(type) {
	case ImportResolverConfig:
		if v.isEmpty() {
			return nil
		}
		cfg := v
		return &cfg
	case ImportResolver:
		if isNilImportResolver(v) {
			return nil
		}
		cfg := ImportResolverConfig{Resolver: v}
		return &cfg
	default:
		return nil
	}
}

func isNilImportResolver(resolver ImportResolver) bool {
	if resolver == nil {
		return true
	}
	v := reflect.ValueOf(resolver)
	return v.Kind() == reflect.Func && v.IsNil()
}

func (cfg ImportResolverConfig) isEmpty() bool {
	return isNilImportResolver(cfg.Resolver) && (cfg.ACL == nil || cfg.ACL.isEmpty())
}

func (acl *ImportACL) isEmpty() bool {
	if acl == nil {
		return true
	}
	return len(acl.allowModules) == 0 &&
		len(acl.allowPrefixes) == 0 &&
		len(acl.denyModules) == 0 &&
		len(acl.denyPrefixes) == 0
}

// CheckImport reports whether moduleName is permitted by this ACL.
func (acl *ImportACL) CheckImport(moduleName string) error {
	if acl == nil || acl.isEmpty() {
		return nil
	}
	if acl.matchesModule(acl.denyModules, acl.denyPrefixes, moduleName) {
		return fmt.Errorf("module[%s] denied by import ACL", moduleName)
	}
	if acl.hasAllowRules() && !acl.matchesModule(acl.allowModules, acl.allowPrefixes, moduleName) {
		return fmt.Errorf("module[%s] not allowed by import ACL", moduleName)
	}
	return nil
}

func (acl *ImportACL) hasAllowRules() bool {
	return acl != nil && (len(acl.allowModules) > 0 || len(acl.allowPrefixes) > 0)
}

func (acl *ImportACL) matchesModule(exact map[string]struct{}, prefixes []string, moduleName string) bool {
	if acl == nil {
		return false
	}
	if _, ok := exact[moduleName]; ok {
		return true
	}
	for _, prefix := range prefixes {
		if strings.HasPrefix(moduleName, prefix) {
			return true
		}
	}
	return false
}
