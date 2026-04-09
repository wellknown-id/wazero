package wazero

import (
	"context"
	"fmt"

	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/filecache"
	"github.com/tetratelabs/wazero/internal/internalapi"
	"github.com/tetratelabs/wazero/internal/wasm"
)

// RuntimeConfig controls runtime behavior, with the default implementation as
// NewRuntimeConfig
//
// The example below explicitly limits to Wasm Core 1.0 features as opposed to
// relying on defaults:
//
//	rConfig = wazero.NewRuntimeConfig().WithCoreFeatures(api.CoreFeaturesV1)
//
// # Notes
//
//   - This is an interface for decoupling, not third-party implementations.
//     All implementations are in wazero.
//   - RuntimeConfig is immutable. Each WithXXX function returns a new instance
//     including the corresponding change.
type RuntimeConfig interface {
	// WithCoreFeatures sets the WebAssembly Core specification features this
	// runtime supports. Defaults to api.CoreFeaturesV2.
	//
	// Example of disabling a specific feature:
	//	features := api.CoreFeaturesV2.SetEnabled(api.CoreFeatureMutableGlobal, false)
	//	rConfig = wazero.NewRuntimeConfig().WithCoreFeatures(features)
	//
	// # Why default to version 2.0?
	//
	// Many compilers that target WebAssembly require features after
	// api.CoreFeaturesV1 by default. For example, TinyGo v0.24+ requires
	// api.CoreFeatureBulkMemoryOperations. To avoid runtime errors, wazero
	// defaults to api.CoreFeaturesV2, even though it is not yet a Web
	// Standard (REC).
	WithCoreFeatures(api.CoreFeatures) RuntimeConfig

	// WithMemoryLimitPages overrides the maximum pages allowed per memory. The
	// default is 65536, allowing 4GB total memory per instance if the maximum is
	// not encoded in a Wasm binary. Setting a value larger than default will panic.
	//
	// This example reduces the largest possible memory size from 4GB to 128KB:
	//	rConfig = wazero.NewRuntimeConfig().WithMemoryLimitPages(2)
	//
	// Note: Wasm has 32-bit memory and each page is 65536 (2^16) bytes. This
	// implies a max of 65536 (2^16) addressable pages.
	// See https://www.w3.org/TR/2019/REC-wasm-core-1-20191205/#grow-mem
	WithMemoryLimitPages(memoryLimitPages uint32) RuntimeConfig

	// WithMemoryCapacityFromMax eagerly allocates max memory, unless max is
	// not defined. The default is false, which means minimum memory is
	// allocated and any call to grow memory results in re-allocations.
	//
	// This example ensures any memory.grow instruction will never re-allocate:
	//	rConfig = wazero.NewRuntimeConfig().WithMemoryCapacityFromMax(true)
	//
	// See https://www.w3.org/TR/2019/REC-wasm-core-1-20191205/#grow-mem
	//
	// Note: if the memory maximum is not encoded in a Wasm binary, this
	// results in allocating 4GB. See the doc on WithMemoryLimitPages for detail.
	WithMemoryCapacityFromMax(memoryCapacityFromMax bool) RuntimeConfig

	// WithDebugInfoEnabled toggles DWARF based stack traces in the face of
	// runtime errors. Defaults to true.
	//
	// Those who wish to disable this, can like so:
	//
	//	r := wazero.NewRuntimeWithConfig(wazero.NewRuntimeConfig().WithDebugInfoEnabled(false)
	//
	// When disabled, a stack trace message looks like:
	//
	//	wasm stack trace:
	//		.runtime._panic(i32)
	//		.myFunc()
	//		.main.main()
	//		.runtime.run()
	//		._start()
	//
	// When enabled, the stack trace includes source code information:
	//
	//	wasm stack trace:
	//		.runtime._panic(i32)
	//		  0x16e2: /opt/homebrew/Cellar/tinygo/0.26.0/src/runtime/runtime_tinygowasm.go:73:6
	//		.myFunc()
	//		  0x190b: /Users/XXXXX/wazero/internal/testing/dwarftestdata/testdata/main.go:19:7
	//		.main.main()
	//		  0x18ed: /Users/XXXXX/wazero/internal/testing/dwarftestdata/testdata/main.go:4:3
	//		.runtime.run()
	//		  0x18cc: /opt/homebrew/Cellar/tinygo/0.26.0/src/runtime/scheduler_none.go:26:10
	//		._start()
	//		  0x18b6: /opt/homebrew/Cellar/tinygo/0.26.0/src/runtime/runtime_wasm_wasi.go:22:5
	//
	// Note: This only takes into effect when the original Wasm binary has the
	// DWARF "custom sections" that are often stripped, depending on
	// optimization flags passed to the compiler.
	WithDebugInfoEnabled(bool) RuntimeConfig

	// WithCompilationCache configures how runtime caches the compiled modules. In the default configuration, compilation results are
	// only in-memory until Runtime.Close is closed, and not shareable by multiple Runtime.
	//
	// Below defines the shared cache across multiple instances of Runtime:
	//
	//	// Creates the new Cache and the runtime configuration with it.
	//	cache := wazero.NewCompilationCache()
	//	defer cache.Close()
	//	config := wazero.NewRuntimeConfig().WithCompilationCache(c)
	//
	//	// Creates two runtimes while sharing compilation caches.
	//	foo := wazero.NewRuntimeWithConfig(context.Background(), config)
	// 	bar := wazero.NewRuntimeWithConfig(context.Background(), config)
	//
	// # Cache Key
	//
	// Cached files are keyed on the version of wazero. This is obtained from go.mod of your application,
	// and we use it to verify the compatibility of caches against the currently-running wazero.
	// However, if you use this in tests of a package not named as `main`, then wazero cannot obtain the correct
	// version of wazero due to the known issue of debug.BuildInfo function: https://github.com/golang/go/issues/33976.
	// As a consequence, your cache won't contain the correct version information and always be treated as `dev` version.
	// To avoid this issue, you can pass -ldflags "-X github.com/tetratelabs/wazero/internal/version.version=foo" when running tests.
	WithCompilationCache(CompilationCache) RuntimeConfig

	// WithCustomSections toggles parsing of "custom sections". Defaults to false.
	//
	// When enabled, it is possible to retrieve custom sections from a CompiledModule:
	//
	//	config := wazero.NewRuntimeConfig().WithCustomSections(true)
	//	r := wazero.NewRuntimeWithConfig(ctx, config)
	//	c, err := r.CompileModule(ctx, wasm)
	//	customSections := c.CustomSections()
	WithCustomSections(bool) RuntimeConfig

	// WithCloseOnContextDone ensures the executions of functions to be terminated under one of the following circumstances:
	//
	// 	- context.Context passed to the Call method of api.Function is canceled during execution. (i.e. ctx by context.WithCancel)
	// 	- context.Context passed to the Call method of api.Function reaches timeout during execution. (i.e. ctx by context.WithTimeout or context.WithDeadline)
	// 	- Close or CloseWithExitCode of api.Module is explicitly called during execution.
	//
	// This is especially useful when one wants to run untrusted Wasm binaries since otherwise, any invocation of
	// api.Function can potentially block the corresponding Goroutine forever. Moreover, it might block the
	// entire underlying OS thread which runs the api.Function call. See "Why it's safe to execute runtime-generated
	// machine codes against async Goroutine preemption" section in RATIONALE.md for detail.
	//
	// Upon the termination of the function executions, api.Module is closed.
	//
	// Note that this comes with a bit of extra cost when enabled. The reason is that internally this forces
	// interpreter and compiler runtimes to insert the periodical checks on the conditions above. For that reason,
	// this is disabled by default.
	//
	// See examples in context_done_example_test.go for the end-to-end demonstrations.
	//
	// When the invocations of api.Function are closed due to this, sys.ExitError is raised to the callers and
	// the api.Module from which the functions are derived is made closed.
	WithCloseOnContextDone(bool) RuntimeConfig

	// WithSecureMode enables security-hardened execution for untrusted workloads.
	// When enabled, wazero prefers guard-page-backed linear memory on unix and
	// windows targets. On the compiler's Linux amd64/arm64 secure-mode path,
	// out-of-bounds guest memory faults are converted into Wasm traps instead of
	// relying on the normal software bounds-check path.
	//
	// On other targets, secure mode falls back to software bounds checks for
	// ordinary execution. On platforms without guard-page support, execution
	// remains fully software-checked with reduced isolation guarantees.
	//
	// Default: false (upstream-compatible behaviour).
	//
	// See SUPPORT_MATRIX.md for the runtime-mode/platform support matrix and
	// THREAT_MODEL.md for the security model.
	WithSecureMode(bool) RuntimeConfig

	// WithFuel sets the default fuel budget for each Wasm function call.
	// Fuel is a deterministic CPU metering mechanism: compiled code decrements
	// a counter at function entries and loop back-edges, and when the counter
	// drops below zero, execution terminates with ErrRuntimeFuelExhausted.
	//
	// A value of 0 (the default) means unlimited — no fuel metering overhead
	// is incurred and behavior matches upstream wazero exactly.
	//
	// This can be overridden per-call by setting an experimental.FuelController
	// on the context passed to api.Function.Call. See experimental.WithFuelController.
	// Fuel lifecycle hooks can be attached per-call with
	// experimental.WithFuelObserver.
	//
	// Note: fuel metering is currently supported only by the compiler (wazevo)
	// engine. The interpreter ignores this setting, including when
	// NewRuntimeConfig() auto-falls back to the interpreter.
	//
	// See SUPPORT_MATRIX.md for the current support and fallback matrix.
	WithFuel(fuel int64) RuntimeConfig

	// WithTimeProvider sets the default host-visible time provider for modules
	// instantiated by this runtime.
	//
	// The configured provider is surfaced to host functions through
	// experimental.GetTimeProvider(ctx). A provider attached directly to the
	// call context with experimental.WithTimeProvider takes precedence.
	//
	// A nil provider disables the runtime default and preserves current
	// behavior unless a call context provides one explicitly.
	WithTimeProvider(experimental.TimeProvider) RuntimeConfig

	// WithHostCallPolicy sets the default host-call policy for modules
	// instantiated by this runtime.
	//
	// The configured policy is surfaced to imported host function calls when the
	// call context does not already carry one via experimental.WithHostCallPolicy.
	// An explicit call-scoped policy therefore takes precedence. Embedders that
	// want to further narrow a runtime default can compose both policies in the
	// call-scoped policy they attach.
	//
	// A nil policy disables the runtime default and preserves current behavior
	// unless a call context provides one explicitly.
	WithHostCallPolicy(experimental.HostCallPolicy) RuntimeConfig

	// WithYieldPolicy sets the default yield policy for modules instantiated by
	// this runtime.
	//
	// The configured policy is surfaced to host-function yields when the call
	// context does not already carry one via experimental.WithYieldPolicy. An
	// explicit call-scoped policy therefore takes precedence. Embedders that
	// want to further narrow a runtime default can compose both policies in the
	// call-scoped policy they attach.
	//
	// A nil policy disables the runtime default and preserves current behavior
	// unless a call context provides one explicitly.
	WithYieldPolicy(experimental.YieldPolicy) RuntimeConfig
}

// NewRuntimeConfig returns a RuntimeConfig using the compiler if it is supported in this environment,
// or the interpreter otherwise.
func NewRuntimeConfig() RuntimeConfig {
	ret := engineLessConfig.clone()
	ret.engineKind = engineKindAuto
	return ret
}

type newEngine func(context.Context, api.CoreFeatures, filecache.Cache) wasm.Engine

type runtimeConfig struct {
	enabledFeatures       api.CoreFeatures
	memoryLimitPages      uint32
	memoryCapacityFromMax bool
	engineKind            engineKind
	dwarfDisabled         bool // negative as defaults to enabled
	newEngine             newEngine
	cache                 CompilationCache
	storeCustomSections   bool
	ensureTermination     bool
	secureMode            bool
	fuel                  int64
	timeProvider          experimental.TimeProvider
	hostCallPolicy        experimental.HostCallPolicy
	yieldPolicy           experimental.YieldPolicy
}

// engineLessConfig helps avoid copy/pasting the wrong defaults.
var engineLessConfig = &runtimeConfig{
	enabledFeatures:       api.CoreFeaturesV2,
	memoryLimitPages:      wasm.MemoryLimitPages,
	memoryCapacityFromMax: false,
	dwarfDisabled:         false,
}

type engineKind int

const (
	engineKindAuto engineKind = iota - 1
	engineKindCompiler
	engineKindInterpreter
	engineKindCount
)

// NewRuntimeConfigCompiler compiles WebAssembly modules into
// runtime.GOARCH-specific assembly for optimal performance.
//
// The default implementation is AOT (Ahead of Time) compilation, applied at
// Runtime.CompileModule. This allows consistent runtime performance, as well
// the ability to reduce any first request penalty.
//
// Note: While this is technically AOT, this does not imply any action on your
// part. wazero automatically performs ahead-of-time compilation as needed when
// Runtime.CompileModule is invoked.
//
// # Warning
//
//   - This panics at runtime if the runtime.GOOS or runtime.GOARCH does not
//     support compiler. Use NewRuntimeConfig to safely detect and fallback to
//     NewRuntimeConfigInterpreter if needed.
//
//   - If you are using wazero in buildmode=c-archive or c-shared, make sure that you set up the alternate signal stack
//     by using, e.g. `sigaltstack` combined with `SA_ONSTACK` flag on `sigaction` on Linux,
//     before calling any api.Function. This is because the Go runtime does not set up the alternate signal stack
//     for c-archive or c-shared modes, and wazero uses the different stack than the calling Goroutine.
//     Hence, the signal handler might get invoked on the wazero's stack, which may cause a stack overflow.
//     https://github.com/tetratelabs/wazero/blob/2092c0a879f30d49d7b37f333f4547574b8afe0d/internal/integration_test/fuzz/fuzz/tests/sigstack.rs#L19-L36
func NewRuntimeConfigCompiler() RuntimeConfig {
	ret := engineLessConfig.clone()
	ret.engineKind = engineKindCompiler
	return ret
}

// NewRuntimeConfigInterpreter interprets WebAssembly modules instead of compiling them into assembly.
func NewRuntimeConfigInterpreter() RuntimeConfig {
	ret := engineLessConfig.clone()
	ret.engineKind = engineKindInterpreter
	return ret
}

// clone makes a deep copy of this runtime config.
func (c *runtimeConfig) clone() *runtimeConfig {
	ret := *c // copy except maps which share a ref
	return &ret
}

// WithCoreFeatures implements RuntimeConfig.WithCoreFeatures
func (c *runtimeConfig) WithCoreFeatures(features api.CoreFeatures) RuntimeConfig {
	ret := c.clone()
	ret.enabledFeatures = features
	return ret
}

// WithCloseOnContextDone implements RuntimeConfig.WithCloseOnContextDone
func (c *runtimeConfig) WithCloseOnContextDone(ensure bool) RuntimeConfig {
	ret := c.clone()
	ret.ensureTermination = ensure
	return ret
}

// WithMemoryLimitPages implements RuntimeConfig.WithMemoryLimitPages
func (c *runtimeConfig) WithMemoryLimitPages(memoryLimitPages uint32) RuntimeConfig {
	ret := c.clone()
	// This panics instead of returning an error as it is unlikely.
	if memoryLimitPages > wasm.MemoryLimitPages {
		panic(fmt.Errorf("memoryLimitPages invalid: %d > %d", memoryLimitPages, wasm.MemoryLimitPages))
	}
	ret.memoryLimitPages = memoryLimitPages
	return ret
}

// WithCompilationCache implements RuntimeConfig.WithCompilationCache
func (c *runtimeConfig) WithCompilationCache(ca CompilationCache) RuntimeConfig {
	ret := c.clone()
	ret.cache = ca
	return ret
}

// WithMemoryCapacityFromMax implements RuntimeConfig.WithMemoryCapacityFromMax
func (c *runtimeConfig) WithMemoryCapacityFromMax(memoryCapacityFromMax bool) RuntimeConfig {
	ret := c.clone()
	ret.memoryCapacityFromMax = memoryCapacityFromMax
	return ret
}

// WithDebugInfoEnabled implements RuntimeConfig.WithDebugInfoEnabled
func (c *runtimeConfig) WithDebugInfoEnabled(dwarfEnabled bool) RuntimeConfig {
	ret := c.clone()
	ret.dwarfDisabled = !dwarfEnabled
	return ret
}

// WithCustomSections implements RuntimeConfig.WithCustomSections
func (c *runtimeConfig) WithCustomSections(storeCustomSections bool) RuntimeConfig {
	ret := c.clone()
	ret.storeCustomSections = storeCustomSections
	return ret
}

// WithSecureMode implements RuntimeConfig.WithSecureMode
func (c *runtimeConfig) WithSecureMode(secureMode bool) RuntimeConfig {
	ret := c.clone()
	ret.secureMode = secureMode
	return ret
}

// WithFuel implements RuntimeConfig.WithFuel
func (c *runtimeConfig) WithFuel(fuel int64) RuntimeConfig {
	ret := c.clone()
	if fuel < 0 {
		fuel = 0
	}
	ret.fuel = fuel
	return ret
}

// WithTimeProvider implements RuntimeConfig.WithTimeProvider.
func (c *runtimeConfig) WithTimeProvider(provider experimental.TimeProvider) RuntimeConfig {
	ret := c.clone()
	ret.timeProvider = provider
	return ret
}

// WithHostCallPolicy implements RuntimeConfig.WithHostCallPolicy.
func (c *runtimeConfig) WithHostCallPolicy(policy experimental.HostCallPolicy) RuntimeConfig {
	ret := c.clone()
	ret.hostCallPolicy = experimental.GetHostCallPolicy(experimental.WithHostCallPolicy(context.Background(), policy))
	return ret
}

// WithYieldPolicy implements RuntimeConfig.WithYieldPolicy.
func (c *runtimeConfig) WithYieldPolicy(policy experimental.YieldPolicy) RuntimeConfig {
	ret := c.clone()
	ret.yieldPolicy = experimental.GetYieldPolicy(experimental.WithYieldPolicy(context.Background(), policy))
	return ret
}

// CompiledModule is a WebAssembly module ready to be instantiated (Runtime.InstantiateModule) as an api.Module.
//
// In WebAssembly terminology, this is a decoded, validated, and possibly also compiled module. wazero avoids using
// the name "Module" for both before and after instantiation as the name conflation has caused confusion.
// See https://www.w3.org/TR/2019/REC-wasm-core-1-20191205/#semantic-phases%E2%91%A0
//
// # Notes
//
//   - This is an interface for decoupling, not third-party implementations.
//     All implementations are in wazero.
//   - Closing the wazero.Runtime closes any CompiledModule it compiled.
type CompiledModule interface {
	// Name returns the module name encoded into the binary or empty if not.
	Name() string

	// ImportedFunctions returns all the imported functions
	// (api.FunctionDefinition) in this module or nil if there are none.
	//
	// Note: Unlike ExportedFunctions, there is no unique constraint on
	// imports.
	ImportedFunctions() []api.FunctionDefinition

	// ExportedFunctions returns all the exported functions
	// (api.FunctionDefinition) in this module keyed on export name.
	ExportedFunctions() map[string]api.FunctionDefinition

	// ImportedMemories returns all the imported memories
	// (api.MemoryDefinition) in this module or nil if there are none.
	//
	// ## Notes
	//   - As of WebAssembly Core Specification 2.0, there can be at most one
	//     memory.
	//   - Unlike ExportedMemories, there is no unique constraint on imports.
	ImportedMemories() []api.MemoryDefinition

	// ExportedMemories returns all the exported memories
	// (api.MemoryDefinition) in this module keyed on export name.
	//
	// Note: As of WebAssembly Core Specification 2.0, there can be at most one
	// memory.
	ExportedMemories() map[string]api.MemoryDefinition

	// CustomSections returns all the custom sections
	// (api.CustomSection) in this module keyed on the section name.
	CustomSections() []api.CustomSection

	// Close releases all the allocated resources for this CompiledModule.
	//
	// Note: It is safe to call Close while having outstanding calls from an
	// api.Module instantiated from this.
	Close(context.Context) error
}

// compile-time check to ensure compiledModule implements CompiledModule
var _ CompiledModule = &compiledModule{}

type compiledModule struct {
	module *wasm.Module
	// compiledEngine holds an engine on which `module` is compiled.
	compiledEngine wasm.Engine
	// closeWithModule prevents leaking compiled code when a module is compiled implicitly.
	closeWithModule bool
	typeIDs         []wasm.FunctionTypeID
}

// Name implements CompiledModule.Name
func (c *compiledModule) Name() (moduleName string) {
	if ns := c.module.NameSection; ns != nil {
		moduleName = ns.ModuleName
	}
	return
}

// Close implements CompiledModule.Close
func (c *compiledModule) Close(context.Context) error {
	c.compiledEngine.DeleteCompiledModule(c.module)
	// It is possible the underlying may need to return an error later, but in any case this matches api.Module.Close.
	return nil
}

// ImportedFunctions implements CompiledModule.ImportedFunctions
func (c *compiledModule) ImportedFunctions() []api.FunctionDefinition {
	return c.module.ImportedFunctions()
}

// ExportedFunctions implements CompiledModule.ExportedFunctions
func (c *compiledModule) ExportedFunctions() map[string]api.FunctionDefinition {
	return c.module.ExportedFunctions()
}

// ImportedMemories implements CompiledModule.ImportedMemories
func (c *compiledModule) ImportedMemories() []api.MemoryDefinition {
	return c.module.ImportedMemories()
}

// ExportedMemories implements CompiledModule.ExportedMemories
func (c *compiledModule) ExportedMemories() map[string]api.MemoryDefinition {
	return c.module.ExportedMemories()
}

// CustomSections implements CompiledModule.CustomSections
func (c *compiledModule) CustomSections() []api.CustomSection {
	ret := make([]api.CustomSection, len(c.module.CustomSections))
	for i, d := range c.module.CustomSections {
		ret[i] = &customSection{data: d.Data, name: d.Name}
	}
	return ret
}

// customSection implements wasm.CustomSection
type customSection struct {
	internalapi.WazeroOnlyType
	name string
	data []byte
}

// Name implements wasm.CustomSection.Name
func (c *customSection) Name() string {
	return c.name
}

// Data implements wasm.CustomSection.Data
func (c *customSection) Data() []byte {
	return c.data
}

// ModuleConfig configures resources needed by functions that have low-level interactions with the host operating
// system. Using this, resources such as STDIN can be isolated, so that the same module can be safely instantiated
// multiple times.
//
// Here's an example:
//
//	// Initialize base configuration:
//	config := wazero.NewModuleConfig()
//
//	// Assign different configuration on each instantiation
//	mod, _ := r.InstantiateModule(ctx, compiled, config.WithName("rotate"))
//
// While wazero supports Windows as a platform, host functions using ModuleConfig follow a UNIX dialect.
// See RATIONALE.md for design background and relationship to WebAssembly System Interfaces (WASI).
//
// # Notes
//
//   - This is an interface for decoupling, not third-party implementations.
//     All implementations are in wazero.
//   - ModuleConfig is immutable. Each WithXXX function returns a new instance
//     including the corresponding change.
type ModuleConfig interface {
	// WithName configures the module name. Defaults to what was decoded from
	// the name section. Duplicate names are not allowed in a single Runtime.
	//
	// Calling this with the empty string "" makes the module anonymous.
	// That is useful when you want to instantiate the same CompiledModule multiple times like below:
	//
	// 	for i := 0; i < N; i++ {
	//		// Instantiate a new Wasm module from the already compiled `compiledWasm` anonymously without a name.
	//		instance, err := r.InstantiateModule(ctx, compiledWasm, wazero.NewModuleConfig().WithName(""))
	//		// ....
	//	}
	//
	// See the `concurrent-instantiation` example for a complete usage.
	//
	// Non-empty named modules are available for other modules to import by name.
	WithName(string) ModuleConfig
}

type moduleConfig struct {
	name    string
	nameSet bool
}

// NewModuleConfig returns a ModuleConfig that can be used for configuring module instantiation.
func NewModuleConfig() ModuleConfig {
	return &moduleConfig{}
}

// clone makes a deep copy of this module config.
func (c *moduleConfig) clone() *moduleConfig {
	ret := *c // copy except maps which share a ref
	return &ret
}

// WithName implements ModuleConfig.WithName
func (c *moduleConfig) WithName(name string) ModuleConfig {
	ret := c.clone()
	ret.nameSet = true
	ret.name = name
	return ret
}
