use std::sync::Arc;

use crate::{api::wasm::Module, ctx_keys::Context, experimental::yield_policy::YieldPolicyRequest};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum YieldPolicyDecision {
    Allowed,
    Denied,
}

#[derive(Clone)]
pub struct YieldPolicyObservation {
    pub module: Module,
    pub request: YieldPolicyRequest,
    pub decision: YieldPolicyDecision,
}

pub trait YieldPolicyObserver: Send + Sync {
    fn observe_yield_policy(&self, ctx: &Context, observation: YieldPolicyObservation);
}

impl<F> YieldPolicyObserver for F
where
    F: Fn(&Context, YieldPolicyObservation) + Send + Sync,
{
    fn observe_yield_policy(&self, ctx: &Context, observation: YieldPolicyObservation) {
        (self)(ctx, observation);
    }
}

pub fn with_yield_policy_observer(
    ctx: &Context,
    observer: impl YieldPolicyObserver + 'static,
) -> Context {
    let mut cloned = ctx.clone();
    cloned.yield_policy_observer = Some(Arc::new(observer));
    cloned
}

pub fn get_yield_policy_observer(ctx: &Context) -> Option<Arc<dyn YieldPolicyObserver>> {
    ctx.yield_policy_observer.clone()
}

pub(crate) fn notify_yield_policy_observer(
    ctx: &Context,
    module: &Module,
    request: &YieldPolicyRequest,
    decision: YieldPolicyDecision,
) {
    let Some(observer) = get_yield_policy_observer(ctx) else {
        return;
    };
    observer.observe_yield_policy(
        ctx,
        YieldPolicyObservation {
            module: module.clone(),
            request: request.clone(),
            decision,
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        get_yield_policy_observer, with_yield_policy_observer, YieldPolicyDecision,
        YieldPolicyObservation,
    };
    use crate::{config::ModuleConfig, runtime::Runtime, Context};

    #[test]
    fn yield_policy_observer_round_trips_through_context() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = with_yield_policy_observer(&Context::default(), {
            let events = events.clone();
            move |_ctx: &Context, observation: YieldPolicyObservation| {
                events.lock().expect("observer events poisoned").push((
                    observation.module.name().map(str::to_string),
                    observation.request.caller_module_name().map(str::to_string),
                    observation.decision,
                ));
            }
        });

        let observer = get_yield_policy_observer(&ctx).expect("observer should exist");
        let runtime = Runtime::new();
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            ])
            .unwrap();
        let module = runtime
            .instantiate(&compiled, ModuleConfig::new().with_name("guest"))
            .unwrap();
        observer.observe_yield_policy(
            &ctx,
            YieldPolicyObservation {
                module,
                request: crate::YieldPolicyRequest::new().with_caller_module_name("caller"),
                decision: YieldPolicyDecision::Denied,
            },
        );

        assert_eq!(
            vec![(
                Some("guest".to_string()),
                Some("caller".to_string()),
                YieldPolicyDecision::Denied,
            )],
            *events.lock().expect("observer events poisoned")
        );
    }
}
