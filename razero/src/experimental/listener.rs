use std::sync::Arc;

use crate::{
    api::{
        error::RuntimeError,
        wasm::{FunctionDefinition, Module},
    },
    ctx_keys::Context,
};

pub trait FunctionListenerFactory: Send + Sync {
    fn new_listener(&self, definition: &FunctionDefinition) -> Option<Arc<dyn FunctionListener>>;
}

pub trait FunctionListener: Send + Sync {
    fn before(
        &self,
        _ctx: &Context,
        _module: &Module,
        _definition: &FunctionDefinition,
        _params: &[u64],
    ) {
    }

    fn after(
        &self,
        _ctx: &Context,
        _module: &Module,
        _definition: &FunctionDefinition,
        _results: &[u64],
    ) {
    }

    fn abort(
        &self,
        _ctx: &Context,
        _module: &Module,
        _definition: &FunctionDefinition,
        _error: &RuntimeError,
    ) {
    }
}

pub struct FunctionListenerFactoryFn<F>(F);

impl<F> FunctionListenerFactoryFn<F> {
    pub fn new(factory: F) -> Self {
        Self(factory)
    }
}

impl<F> FunctionListenerFactory for FunctionListenerFactoryFn<F>
where
    F: Fn(&FunctionDefinition) -> Option<Arc<dyn FunctionListener>> + Send + Sync,
{
    fn new_listener(&self, definition: &FunctionDefinition) -> Option<Arc<dyn FunctionListener>> {
        (self.0)(definition)
    }
}

pub fn with_function_listener_factory(
    ctx: &Context,
    factory: impl FunctionListenerFactory + 'static,
) -> Context {
    let mut cloned = ctx.clone();
    cloned.function_listener_factory = Some(Arc::new(factory));
    cloned
}
