use std::sync::Arc;

use crate::{api::wasm::Module, ctx_keys::Context};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportResolverEvent {
    AclAllowed,
    AclDenied,
    ResolverResolved,
    StoreFallback,
    FailClosedDenied,
}

#[derive(Clone)]
pub struct ImportResolverObservation {
    pub module_name: String,
    pub import_module: String,
    pub resolved_module: Option<Module>,
    pub event: ImportResolverEvent,
}

pub trait ImportResolverObserver: Send + Sync {
    fn observe_import_resolution(&self, ctx: &Context, observation: ImportResolverObservation);
}

impl<F> ImportResolverObserver for F
where
    F: Fn(&Context, ImportResolverObservation) + Send + Sync,
{
    fn observe_import_resolution(&self, ctx: &Context, observation: ImportResolverObservation) {
        (self)(ctx, observation);
    }
}

pub fn with_import_resolver_observer(
    ctx: &Context,
    observer: impl ImportResolverObserver + 'static,
) -> Context {
    let mut cloned = ctx.clone();
    cloned.import_resolver_observer = Some(Arc::new(observer));
    cloned
}

pub fn get_import_resolver_observer(ctx: &Context) -> Option<Arc<dyn ImportResolverObserver>> {
    ctx.import_resolver_observer.clone()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        get_import_resolver_observer, with_import_resolver_observer, ImportResolverEvent,
        ImportResolverObservation,
    };
    use crate::Context;

    #[test]
    fn import_resolver_observer_round_trips_through_context() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = with_import_resolver_observer(&Context::default(), {
            let events = events.clone();
            move |_ctx: &Context, observation: ImportResolverObservation| {
                events
                    .lock()
                    .expect("observer events poisoned")
                    .push((observation.import_module, observation.event));
            }
        });

        let observer = get_import_resolver_observer(&ctx).expect("observer should exist");
        observer.observe_import_resolution(
            &ctx,
            ImportResolverObservation {
                module_name: String::new(),
                import_module: "env".to_string(),
                resolved_module: None,
                event: ImportResolverEvent::StoreFallback,
            },
        );

        assert_eq!(
            vec![("env".to_string(), ImportResolverEvent::StoreFallback)],
            *events.lock().expect("observer events poisoned")
        );
    }
}
