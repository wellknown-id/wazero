use std::sync::Arc;

use crate::{api::wasm::Module, ctx_keys::Context};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum YieldEvent {
    Yielded,
    Resumed,
    Cancelled,
}

#[derive(Clone)]
#[non_exhaustive]
pub struct YieldObservation {
    pub module: Module,
    pub event: YieldEvent,
    pub yield_count: u64,
    pub expected_host_results: i32,
    pub suspended_nanos: i64,
}

pub trait YieldObserver: Send + Sync {
    fn observe_yield(&self, ctx: &Context, observation: YieldObservation);
}

impl<F> YieldObserver for F
where
    F: Fn(&Context, YieldObservation) + Send + Sync,
{
    fn observe_yield(&self, ctx: &Context, observation: YieldObservation) {
        (self)(ctx, observation);
    }
}

pub fn with_yield_observer(ctx: &Context, observer: impl YieldObserver + 'static) -> Context {
    let mut cloned = ctx.clone();
    cloned.yield_observer = Some(Arc::new(observer));
    cloned
}

pub fn get_yield_observer(ctx: &Context) -> Option<Arc<dyn YieldObserver>> {
    ctx.yield_observer.clone()
}

#[allow(dead_code)]
pub(crate) fn notify_yield_observer(
    ctx: &Context,
    module: &Module,
    event: YieldEvent,
    yield_count: u64,
    expected_host_results: i32,
    suspended_nanos: i64,
) {
    let Some(observer) = get_yield_observer(ctx) else {
        return;
    };
    observer.observe_yield(
        ctx,
        YieldObservation {
            module: module.clone(),
            event,
            yield_count,
            expected_host_results,
            suspended_nanos,
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{get_yield_observer, with_yield_observer, YieldEvent, YieldObservation};
    use crate::{config::ModuleConfig, runtime::Runtime, Context};

    #[test]
    fn yield_observer_round_trips_through_context() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = with_yield_observer(&Context::default(), {
            let events = events.clone();
            move |_ctx: &Context, observation: YieldObservation| {
                events.lock().expect("observer events poisoned").push((
                    observation.module.name().map(str::to_string),
                    observation.event,
                    observation.yield_count,
                    observation.expected_host_results,
                    observation.suspended_nanos,
                ));
            }
        });

        let observer = get_yield_observer(&ctx).expect("observer should exist");
        let runtime = Runtime::new();
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            ])
            .unwrap();
        let module = runtime
            .instantiate(&compiled, ModuleConfig::new().with_name("guest"))
            .unwrap();
        observer.observe_yield(
            &ctx,
            YieldObservation {
                module,
                event: YieldEvent::Yielded,
                yield_count: 1,
                expected_host_results: 0,
                suspended_nanos: 7,
            },
        );

        assert_eq!(
            vec![(Some("guest".to_string()), YieldEvent::Yielded, 1, 0, 7)],
            *events.lock().expect("observer events poisoned")
        );
    }
}
