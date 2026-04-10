use std::sync::Arc;

use crate::{
    api::wasm::Module, ctx_keys::Context, experimental::host_call_policy::HostCallPolicyRequest,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostCallPolicyDecision {
    Allowed,
    Denied,
}

#[derive(Clone)]
pub struct HostCallPolicyObservation {
    pub module: Module,
    pub request: HostCallPolicyRequest,
    pub decision: HostCallPolicyDecision,
}

pub trait HostCallPolicyObserver: Send + Sync {
    fn observe_host_call_policy(&self, ctx: &Context, observation: HostCallPolicyObservation);
}

impl<F> HostCallPolicyObserver for F
where
    F: Fn(&Context, HostCallPolicyObservation) + Send + Sync,
{
    fn observe_host_call_policy(&self, ctx: &Context, observation: HostCallPolicyObservation) {
        (self)(ctx, observation);
    }
}

pub fn with_host_call_policy_observer(
    ctx: &Context,
    observer: impl HostCallPolicyObserver + 'static,
) -> Context {
    let mut cloned = ctx.clone();
    cloned.host_call_policy_observer = Some(Arc::new(observer));
    cloned
}

pub fn get_host_call_policy_observer(ctx: &Context) -> Option<Arc<dyn HostCallPolicyObserver>> {
    ctx.host_call_policy_observer.clone()
}

pub(crate) fn notify_host_call_policy_observer(
    ctx: &Context,
    module: &Module,
    request: &HostCallPolicyRequest,
    decision: HostCallPolicyDecision,
) {
    let Some(observer) = get_host_call_policy_observer(ctx) else {
        return;
    };
    observer.observe_host_call_policy(
        ctx,
        HostCallPolicyObservation {
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
        get_host_call_policy_observer, with_host_call_policy_observer, HostCallPolicyDecision,
        HostCallPolicyObservation,
    };
    use crate::{config::ModuleConfig, runtime::Runtime, Context};

    #[test]
    fn host_call_policy_observer_round_trips_through_context() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = with_host_call_policy_observer(&Context::default(), {
            let events = events.clone();
            move |_ctx: &Context, observation: HostCallPolicyObservation| {
                events.lock().expect("observer events poisoned").push((
                    observation.module.name().map(str::to_string),
                    observation.request.caller_module_name().map(str::to_string),
                    observation.decision,
                ));
            }
        });

        let observer = get_host_call_policy_observer(&ctx).expect("observer should exist");
        let runtime = Runtime::new();
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            ])
            .unwrap();
        let module = runtime
            .instantiate(&compiled, ModuleConfig::new().with_name("guest"))
            .unwrap();
        observer.observe_host_call_policy(
            &ctx,
            HostCallPolicyObservation {
                module,
                request: crate::HostCallPolicyRequest::new().with_caller_module_name("caller"),
                decision: HostCallPolicyDecision::Denied,
            },
        );

        assert_eq!(
            vec![(
                Some("guest".to_string()),
                Some("caller".to_string()),
                HostCallPolicyDecision::Denied,
            )],
            *events.lock().expect("observer events poisoned")
        );
    }
}
