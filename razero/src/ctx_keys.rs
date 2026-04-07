use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicI64, Arc},
};

use crate::experimental::{
    close_notifier::CloseNotifier,
    fuel::FuelController,
    listener::FunctionListenerFactory,
    listener::{FunctionListener, StackFrame},
    memory::MemoryAllocator,
    r#yield::Yielder,
    snapshotter::Snapshotter,
};

use crate::api::wasm::FunctionDefinition;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ContextKey {
    Fuel,
    FunctionListener,
    Snapshotter,
    Yielder,
    Resumer,
    MemoryAllocator,
    CloseNotifier,
    Custom(String),
}

impl ContextKey {
    pub fn custom(name: impl Into<String>) -> Self {
        Self::Custom(name.into())
    }
}

#[derive(Clone, Default)]
pub struct Context {
    values: BTreeMap<ContextKey, String>,
    pub(crate) fuel_controller: Option<Arc<dyn FuelController>>,
    pub(crate) function_listener_factory: Option<Arc<dyn FunctionListenerFactory>>,
    pub(crate) snapshotter_enabled: bool,
    pub(crate) yielder_enabled: bool,
    pub(crate) memory_allocator: Option<Arc<dyn MemoryAllocator>>,
    pub(crate) close_notifier: Option<Arc<dyn CloseNotifier>>,
    pub(crate) compilation_workers: usize,
    pub(crate) import_resolver:
        Option<Arc<dyn Fn(&str) -> Option<crate::api::wasm::Module> + Send + Sync>>,
    pub(crate) invocation: Option<InvocationContext>,
}

#[derive(Clone)]
pub(crate) struct InvocationContext {
    pub(crate) fuel_remaining: Option<Arc<AtomicI64>>,
    pub(crate) snapshotter: Option<Arc<dyn Snapshotter>>,
    pub(crate) yielder: Option<Arc<dyn Yielder>>,
    #[allow(dead_code)]
    pub(crate) function_listener: Option<Arc<dyn FunctionListener>>,
    #[allow(dead_code)]
    pub(crate) function_definition: Option<FunctionDefinition>,
    pub(crate) listener_stack: Vec<StackFrame>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: ContextKey, value: impl Into<String>) -> Option<String> {
        self.values.insert(key, value.into())
    }

    pub fn get(&self, key: &ContextKey) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub(crate) fn with_invocation(&self, invocation: InvocationContext) -> Self {
        let mut cloned = self.clone();
        cloned.invocation = Some(invocation);
        cloned
    }

    pub(crate) fn with_listener_stack(&self, listener_stack: Vec<StackFrame>) -> Self {
        let mut cloned = self.clone();
        let invocation = cloned.invocation.take().unwrap_or(InvocationContext {
            fuel_remaining: None,
            snapshotter: None,
            yielder: None,
            function_listener: None,
            function_definition: None,
            listener_stack: Vec::new(),
        });
        cloned.invocation = Some(InvocationContext {
            listener_stack,
            ..invocation
        });
        cloned
    }
}
