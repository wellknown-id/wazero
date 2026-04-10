use std::sync::Arc;

use crate::{
    api::wasm::{FunctionDefinition, ValueType},
    ctx_keys::Context,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct YieldPolicyRequest {
    pub function: Option<FunctionDefinition>,
    pub caller_module_name: Option<String>,
}

impl YieldPolicyRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_function(mut self, function: FunctionDefinition) -> Self {
        self.function = Some(function);
        self
    }

    pub fn with_caller_module_name(mut self, caller_module_name: impl Into<String>) -> Self {
        self.caller_module_name = Some(caller_module_name.into());
        self
    }

    pub fn caller_module_name(&self) -> Option<&str> {
        self.caller_module_name.as_deref()
    }

    pub fn param_types(&self) -> Option<&[ValueType]> {
        self.function.as_ref().map(FunctionDefinition::param_types)
    }

    pub fn result_types(&self) -> Option<&[ValueType]> {
        self.function.as_ref().map(FunctionDefinition::result_types)
    }

    pub fn param_names(&self) -> Option<&[String]> {
        self.function.as_ref().map(FunctionDefinition::param_names)
    }

    pub fn result_names(&self) -> Option<&[String]> {
        self.function.as_ref().map(FunctionDefinition::result_names)
    }

    pub fn param_count(&self) -> usize {
        self.param_types().map_or(0, <[ValueType]>::len)
    }

    pub fn result_count(&self) -> usize {
        self.result_types().map_or(0, <[ValueType]>::len)
    }

    pub fn import(&self) -> Option<(&str, &str)> {
        self.function.as_ref().and_then(FunctionDefinition::import)
    }

    pub fn module_name(&self) -> Option<&str> {
        self.function
            .as_ref()
            .and_then(FunctionDefinition::module_name)
    }

    pub fn export_names(&self) -> &[String] {
        self.function
            .as_ref()
            .map_or(&[], FunctionDefinition::export_names)
    }

    pub fn name(&self) -> Option<&str> {
        self.function.as_ref().map(FunctionDefinition::name)
    }
}

pub trait YieldPolicy: Send + Sync {
    fn allow_yield(&self, ctx: &Context, request: &YieldPolicyRequest) -> bool;
}

impl<F> YieldPolicy for F
where
    F: Fn(&Context, &YieldPolicyRequest) -> bool + Send + Sync,
{
    fn allow_yield(&self, ctx: &Context, request: &YieldPolicyRequest) -> bool {
        (self)(ctx, request)
    }
}

pub trait IntoYieldPolicy {
    fn into_yield_policy(self) -> Option<Arc<dyn YieldPolicy>>;
}

impl<T> IntoYieldPolicy for T
where
    T: YieldPolicy + 'static,
{
    fn into_yield_policy(self) -> Option<Arc<dyn YieldPolicy>> {
        Some(Arc::new(self))
    }
}

impl<T> IntoYieldPolicy for Option<T>
where
    T: YieldPolicy + 'static,
{
    fn into_yield_policy(self) -> Option<Arc<dyn YieldPolicy>> {
        self.map(|policy| Arc::new(policy) as Arc<dyn YieldPolicy>)
    }
}

pub fn with_yield_policy(ctx: &Context, policy: impl IntoYieldPolicy) -> Context {
    let Some(policy) = policy.into_yield_policy() else {
        return ctx.clone();
    };
    let mut cloned = ctx.clone();
    cloned.yield_policy = Some(policy);
    cloned
}

pub fn get_yield_policy(ctx: &Context) -> Option<Arc<dyn YieldPolicy>> {
    ctx.yield_policy.clone()
}

#[cfg(test)]
mod tests {
    use super::{get_yield_policy, with_yield_policy, IntoYieldPolicy, YieldPolicyRequest};
    use crate::{ctx_keys::ContextKey, Context, FunctionDefinition, ValueType};

    fn allow_all(_ctx: &Context, _request: &YieldPolicyRequest) -> bool {
        true
    }

    #[test]
    fn yield_policy_request_tracks_function_metadata() {
        let function = FunctionDefinition::new("host.yield")
            .with_module_name(Some("env".to_string()))
            .with_export_name("yield_now");
        let request = YieldPolicyRequest::new()
            .with_function(function.clone())
            .with_caller_module_name("guest_wrapper");

        assert_eq!(Some(function), request.function);
        assert_eq!(Some("guest_wrapper"), request.caller_module_name());
    }

    #[test]
    fn yield_policy_round_trips_through_context() {
        let ctx = with_yield_policy(&Context::default(), allow_all);
        let policy = get_yield_policy(&ctx).expect("policy should be present");

        assert!(policy.allow_yield(&ctx, &YieldPolicyRequest::new()));
    }

    #[test]
    fn yield_policy_accepts_closure_that_reads_request() {
        struct HostYieldPolicy;

        impl super::YieldPolicy for HostYieldPolicy {
            fn allow_yield(&self, _ctx: &Context, request: &YieldPolicyRequest) -> bool {
                request.function.as_ref().map(FunctionDefinition::name) == Some("host.yield")
            }
        }

        let function = FunctionDefinition::new("host.yield")
            .with_module_name(Some("env".to_string()))
            .with_export_name("yield_now");
        let ctx = with_yield_policy(&Context::default(), HostYieldPolicy);
        let policy = get_yield_policy(&ctx).expect("policy should be present");

        assert!(policy.allow_yield(&ctx, &YieldPolicyRequest::new().with_function(function)));
    }

    #[test]
    fn yield_policy_request_exposes_signature_metadata() {
        let function = FunctionDefinition::new("host.yield")
            .with_signature(vec![ValueType::I32, ValueType::I64], vec![ValueType::F32])
            .with_parameter_names(vec!["ptr".to_string(), "len".to_string()])
            .with_result_names(vec!["ok".to_string()]);
        let request = YieldPolicyRequest::new().with_function(function);

        assert_eq!(
            Some(&[ValueType::I32, ValueType::I64][..]),
            request.param_types()
        );
        assert_eq!(Some(&[ValueType::F32][..]), request.result_types());
        assert_eq!(
            Some(&["ptr".to_string(), "len".to_string()][..]),
            request.param_names()
        );
        assert_eq!(Some(&["ok".to_string()][..]), request.result_names());
        assert_eq!(2, request.param_count());
        assert_eq!(1, request.result_count());
    }

    #[test]
    fn yield_policy_request_exposes_import_metadata() {
        let function = FunctionDefinition::new("host.yield").with_import("env", "yield_now");
        let request = YieldPolicyRequest::new().with_function(function);

        assert_eq!(Some(("env", "yield_now")), request.import());
    }

    #[test]
    fn yield_policy_request_defaults_to_empty_metadata() {
        let request = YieldPolicyRequest::new();

        assert_eq!(None, request.param_types());
        assert_eq!(None, request.result_types());
        assert_eq!(None, request.param_names());
        assert_eq!(None, request.result_names());
        assert_eq!(None, request.caller_module_name());
        assert_eq!(0, request.param_count());
        assert_eq!(0, request.result_count());
        assert_eq!(None, request.import());
        assert_eq!(None, request.module_name());
        assert_eq!(None, request.name());
        assert!(request.export_names().is_empty());
    }

    #[test]
    fn yield_policy_request_exposes_function_name() {
        let function = FunctionDefinition::new("host.yield");
        let request = YieldPolicyRequest::new().with_function(function);

        assert_eq!(Some("host.yield"), request.name());
    }

    #[test]
    fn yield_policy_request_exposes_module_name() {
        let function =
            FunctionDefinition::new("host.yield").with_module_name(Some("env".to_string()));
        let request = YieldPolicyRequest::new().with_function(function);

        assert_eq!(Some("env"), request.module_name());
    }

    #[test]
    fn yield_policy_request_exposes_export_names() {
        let function = FunctionDefinition::new("host.yield")
            .with_export_name("yield_v1")
            .with_export_name("yield_v2");
        let request = YieldPolicyRequest::new().with_function(function);

        assert_eq!(
            &["yield_v1".to_string(), "yield_v2".to_string()],
            request.export_names()
        );
    }

    #[test]
    fn with_yield_policy_none_is_noop() {
        let mut ctx = Context::default();
        ctx.insert(ContextKey::custom("marker"), "ok");

        let updated = with_yield_policy(
            &ctx,
            Option::<fn(&Context, &YieldPolicyRequest) -> bool>::None,
        );

        assert!(get_yield_policy(&updated).is_none());
        assert_eq!(Some("ok"), updated.get(&ContextKey::custom("marker")));
    }

    #[test]
    fn into_yield_policy_wraps_trait_impls() {
        struct AllowAll;

        impl super::YieldPolicy for AllowAll {
            fn allow_yield(&self, _ctx: &Context, _request: &YieldPolicyRequest) -> bool {
                true
            }
        }

        let policy = AllowAll
            .into_yield_policy()
            .expect("policy should be wrapped");

        assert!(policy.allow_yield(&Context::default(), &YieldPolicyRequest::new()));
    }
}
