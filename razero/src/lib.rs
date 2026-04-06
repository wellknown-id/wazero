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
pub use config::{CompiledModule, ModuleConfig, RuntimeConfig};
pub use ctx_keys::{Context, ContextKey};
pub use experimental::{
    add_fuel, get_close_notifier, get_fuel_controller, get_memory_allocator, get_snapshotter,
    get_yielder, remaining_fuel, with_close_notifier, with_fuel_controller,
    with_function_listener_factory, with_memory_allocator, with_snapshotter, with_yielder,
    AggregatingFuelController, CloseNotifier, CloseNotifyFn, DefaultMemoryAllocator,
    FuelController, FunctionListener, FunctionListenerFactory, FunctionListenerFactoryFn,
    LinearMemory, MemoryAllocator, Resumer, SimpleFuelController, Snapshot, Snapshotter, Table,
    YieldError, Yielder,
};
pub use filecache::FileCompilationCache;
pub use logging::{LogLevel, Logger, NoopLogger};
pub use runtime::Runtime;
pub use version::{version, VERSION};
