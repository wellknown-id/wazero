use std::sync::Arc;

use crate::{
    api::wasm::{FunctionDefinition, ValueType},
    ctx_keys::Context,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct HostCallPolicyRequest {
    pub function: Option<FunctionDefinition>,
    pub caller_module_name: Option<String>,
}

impl HostCallPolicyRequest {
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
    use crate::{ctx_keys::ContextKey, Context, FunctionDefinition, ValueType};

    fn allow_all(_ctx: &Context, _request: &HostCallPolicyRequest) -> bool {
        true
    }

    #[test]
    fn host_call_policy_request_tracks_function_metadata() {
        let function = FunctionDefinition::new("host.call")
            .with_module_name(Some("env".to_string()))
            .with_export_name("call");
        let request = HostCallPolicyRequest::new()
            .with_function(function.clone())
            .with_caller_module_name("guest");

        assert_eq!(Some(function), request.function);
        assert_eq!(Some("guest"), request.caller_module_name());
    }

    #[test]
    fn host_call_policy_request_exposes_signature_metadata() {
        let function = FunctionDefinition::new("host.call")
            .with_signature(vec![ValueType::I32, ValueType::I64], vec![ValueType::F32])
            .with_parameter_names(vec!["ptr".to_string(), "len".to_string()])
            .with_result_names(vec!["ok".to_string()]);
        let request = HostCallPolicyRequest::new().with_function(function);

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
    fn host_call_policy_request_exposes_import_metadata() {
        let function = FunctionDefinition::new("host.call").with_import("env", "clock_time_get");
        let request = HostCallPolicyRequest::new().with_function(function);

        assert_eq!(Some(("env", "clock_time_get")), request.import());
    }

    #[test]
    fn host_call_policy_request_defaults_to_empty_metadata() {
        let request = HostCallPolicyRequest::new();

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
    fn host_call_policy_request_exposes_function_name() {
        let function = FunctionDefinition::new("host.call");
        let request = HostCallPolicyRequest::new().with_function(function);

        assert_eq!(Some("host.call"), request.name());
    }

    #[test]
    fn host_call_policy_request_exposes_module_name() {
        let function =
            FunctionDefinition::new("host.call").with_module_name(Some("env".to_string()));
        let request = HostCallPolicyRequest::new().with_function(function);

        assert_eq!(Some("env"), request.module_name());
    }

    #[test]
    fn host_call_policy_request_exposes_export_names() {
        let function = FunctionDefinition::new("host.call")
            .with_export_name("call_v1")
            .with_export_name("call_v2");
        let request = HostCallPolicyRequest::new().with_function(function);

        assert_eq!(
            &["call_v1".to_string(), "call_v2".to_string()],
            request.export_names()
        );
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
