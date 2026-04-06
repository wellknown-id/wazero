use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicI64, Arc},
};

use crate::experimental::{
    close_notifier::CloseNotifier, fuel::FuelController, listener::FunctionListenerFactory,
    memory::MemoryAllocator, r#yield::Yielder, snapshotter::Snapshotter,
};

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
    pub(crate) invocation: Option<InvocationContext>,
}

#[derive(Clone)]
pub(crate) struct InvocationContext {
    pub(crate) fuel_remaining: Option<Arc<AtomicI64>>,
    pub(crate) snapshotter: Option<Arc<dyn Snapshotter>>,
    pub(crate) yielder: Option<Arc<dyn Yielder>>,
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
}
