#![doc = "Public razero API."]

pub mod api;
pub mod assemblyscript;
pub mod builder;
pub mod cache;
pub mod config;
pub mod ctx_keys;
pub mod experimental;
pub mod filecache;
pub mod logging;
pub mod runtime;
pub mod version;

pub use api::{
    error::{ExitError, Result, RuntimeError},
    features::CoreFeatures,
    wasm::{
        CustomSection, ExternType, Function, FunctionDefinition, Global, GlobalValue, Instance,
        Memory, MemoryDefinition, Module, ValueType,
    },
};
pub use assemblyscript::{
    host_module_builder as assemblyscript_host_module_builder,
    ABORT_NAME as ASSEMBLYSCRIPT_ABORT_NAME, MODULE_NAME as ASSEMBLYSCRIPT_MODULE_NAME,
    SEED_NAME as ASSEMBLYSCRIPT_SEED_NAME, TRACE_NAME as ASSEMBLYSCRIPT_TRACE_NAME,
};
pub use builder::{HostFunction, HostFunctionBuilder, HostModuleBuilder};
pub use cache::{CompilationCache, InMemoryCompilationCache};
pub use config::{CompiledModule, ModuleConfig, RuntimeConfig, RuntimeEngineKind};
pub use ctx_keys::{CancelHandle, Context, ContextDoneError, ContextKey};
pub use experimental::{
    add_fuel, benchmark_function_listener, get_close_notifier, get_compilation_workers,
    get_fuel_controller, get_host_call_policy, get_host_call_policy_observer, get_import_resolver,
    get_import_resolver_config, get_import_resolver_observer, get_memory_allocator,
    get_snapshotter, get_trap_observer, get_yield_policy, get_yielder, new_stack_iterator,
    remaining_fuel, trap_cause_of, with_close_notifier, with_compilation_workers,
    with_fuel_controller, with_function_listener_factory, with_host_call_policy,
    with_host_call_policy_observer, with_import_resolver, with_import_resolver_acl,
    with_import_resolver_config, with_import_resolver_observer, with_memory_allocator,
    with_snapshotter, with_trap_observer, with_yield_policy, with_yielder,
    AggregatingFuelController, CloseNotifier, CloseNotifyFn, DefaultMemoryAllocator, ErrYielded,
    FrameStackIterator, FuelController, FunctionListener, FunctionListenerFactory,
    FunctionListenerFactoryFn, FunctionListenerFn, HostCallPolicy, HostCallPolicyDecision,
    HostCallPolicyObservation, HostCallPolicyObserver, HostCallPolicyRequest, ImportACL,
    ImportResolver, ImportResolverConfig, ImportResolverEvent, ImportResolverObservation,
    ImportResolverObserver, InternalFunction, InternalModule, IntoCloseNotifier,
    IntoFuelController, IntoHostCallPolicy, IntoMemoryAllocator, IntoYieldPolicy, LinearMemory,
    MemoryAllocator, MemoryAllocatorFn, MultiFunctionListenerFactory, ProgramCounter, Resumer,
    SimpleFuelController, Snapshot, Snapshotter, StackFrame, StackIterator, Table, TrapCause,
    TrapObservation, TrapObserver, YieldError, YieldPolicy, YieldPolicyRequest, Yielder,
    CORE_FEATURES_EXTENDED_CONST, CORE_FEATURES_TAIL_CALL, CORE_FEATURES_THREADS, ERR_YIELDED,
};
pub use filecache::FileCompilationCache;
pub use logging::{LogLevel, Logger, NoopLogger};
pub use runtime::Runtime;
pub use runtime::{PrecompiledArtifact, PrecompiledArtifactError};
pub use version::{version, VERSION};
