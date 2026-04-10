use std::sync::Arc;

use crate::{api::wasm::FunctionDefinition, ctx_keys::Context};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct HostCallPolicyRequest {
    pub function: Option<FunctionDefinition>,
}

impl HostCallPolicyRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_function(mut self, function: FunctionDefinition) -> Self {
        self.function = Some(function);
        self
    }
}

pub trait HostCallPolicy: Send + Sync {
    fn allow_host_call(&self, ctx: &Context, request: &HostCallPolicyRequest) -> bool;
}

impl<F> HostCallPolicy for F
where
    F: Fn(&Context, &HostCallPolicyRequest) -> bool + Send + Sync,
{
    fn allow_host_call(&self, ctx: &Context, request: &HostCallPolicyRequest) -> bool {
        (self)(ctx, request)
    }
}

pub trait IntoHostCallPolicy {
    fn into_host_call_policy(self) -> Option<Arc<dyn HostCallPolicy>>;
}

impl<T> IntoHostCallPolicy for T
where
    T: HostCallPolicy + 'static,
{
    fn into_host_call_policy(self) -> Option<Arc<dyn HostCallPolicy>> {
        Some(Arc::new(self))
    }
}

impl<T> IntoHostCallPolicy for Option<T>
where
    T: HostCallPolicy + 'static,
{
    fn into_host_call_policy(self) -> Option<Arc<dyn HostCallPolicy>> {
        self.map(|policy| Arc::new(policy) as Arc<dyn HostCallPolicy>)
    }
}

pub fn with_host_call_policy(ctx: &Context, policy: impl IntoHostCallPolicy) -> Context {
    let Some(policy) = policy.into_host_call_policy() else {
        return ctx.clone();
    };
    let mut cloned = ctx.clone();
    cloned.host_call_policy = Some(policy);
    cloned
}

pub fn get_host_call_policy(ctx: &Context) -> Option<Arc<dyn HostCallPolicy>> {
    ctx.host_call_policy.clone()
}

#[cfg(test)]
mod tests {
    use super::{
        get_host_call_policy, with_host_call_policy, HostCallPolicyRequest, IntoHostCallPolicy,
    };
    use crate::{ctx_keys::ContextKey, Context, FunctionDefinition};

    fn allow_all(_ctx: &Context, _request: &HostCallPolicyRequest) -> bool {
        true
    }

    #[test]
    fn host_call_policy_request_tracks_function_metadata() {
        let function = FunctionDefinition::new("host.call")
            .with_module_name(Some("env".to_string()))
            .with_export_name("call");
        let request = HostCallPolicyRequest::new().with_function(function.clone());

        assert_eq!(Some(function), request.function);
    }

    #[test]
    fn host_call_policy_round_trips_through_context() {
        let ctx = with_host_call_policy(&Context::default(), allow_all);
        let policy = get_host_call_policy(&ctx).expect("policy should be present");

        assert!(policy.allow_host_call(&ctx, &HostCallPolicyRequest::new()));
    }

    #[test]
    fn host_call_policy_accepts_closure_that_reads_request() {
        struct EnvOnlyPolicy;

        impl super::HostCallPolicy for EnvOnlyPolicy {
            fn allow_host_call(&self, _ctx: &Context, request: &HostCallPolicyRequest) -> bool {
                request
                    .function
                    .as_ref()
                    .and_then(FunctionDefinition::module_name)
                    == Some("env")
            }
        }

        let function = FunctionDefinition::new("host.call")
            .with_module_name(Some("env".to_string()))
            .with_export_name("call");
        let ctx = with_host_call_policy(&Context::default(), EnvOnlyPolicy);
        let policy = get_host_call_policy(&ctx).expect("policy should be present");

        assert!(policy.allow_host_call(&ctx, &HostCallPolicyRequest::new().with_function(function)));
    }

    #[test]
    fn with_host_call_policy_none_is_noop() {
        let mut ctx = Context::default();
        ctx.insert(ContextKey::custom("marker"), "ok");

        let updated = with_host_call_policy(
            &ctx,
            Option::<fn(&Context, &HostCallPolicyRequest) -> bool>::None,
        );

        assert!(get_host_call_policy(&updated).is_none());
        assert_eq!(Some("ok"), updated.get(&ContextKey::custom("marker")));
    }

    #[test]
    fn into_host_call_policy_wraps_trait_impls() {
        struct AllowAll;

        impl super::HostCallPolicy for AllowAll {
            fn allow_host_call(&self, _ctx: &Context, _request: &HostCallPolicyRequest) -> bool {
                true
            }
        }

        let policy = AllowAll
            .into_host_call_policy()
            .expect("policy should be wrapped");

        assert!(policy.allow_host_call(&Context::default(), &HostCallPolicyRequest::new()));
    }
}
