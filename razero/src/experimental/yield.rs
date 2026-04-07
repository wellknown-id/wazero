use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    sync::Arc,
};

use crate::{api::error::Result, ctx_keys::Context};

pub trait Yielder: Send + Sync {
    fn r#yield(&self);
}

pub trait Resumer: Send + Sync {
    fn resume(&self, ctx: &Context, host_results: &[u64]) -> Result<Vec<u64>>;
    fn cancel(&self);
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ErrYielded;

impl Display for ErrYielded {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("wasm execution yielded")
    }
}

impl Error for ErrYielded {}

pub static ERR_YIELDED: ErrYielded = ErrYielded;

#[derive(Clone)]
pub struct YieldError {
    resumer: Option<Arc<dyn Resumer>>,
}

impl YieldError {
    pub fn new(resumer: Option<Arc<dyn Resumer>>) -> Self {
        Self { resumer }
    }

    pub fn resumer(&self) -> Option<Arc<dyn Resumer>> {
        self.resumer.clone()
    }
}

impl Display for YieldError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("wasm execution yielded")
    }
}

impl fmt::Debug for YieldError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("YieldError").finish_non_exhaustive()
    }
}

impl Error for YieldError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&ERR_YIELDED)
    }
}

impl YieldError {
    pub fn is_yielded(error: &(dyn Error + 'static)) -> bool {
        let mut current = Some(error);
        while let Some(err) = current {
            if err.downcast_ref::<ErrYielded>().is_some()
                || err.downcast_ref::<YieldError>().is_some()
            {
                return true;
            }
            current = err.source();
        }
        false
    }
}

pub fn with_yielder(ctx: &Context) -> Context {
    let mut cloned = ctx.clone();
    cloned.yielder_enabled = true;
    cloned
}

pub fn get_yielder(ctx: &Context) -> Option<Arc<dyn Yielder>> {
    ctx.invocation
        .as_ref()
        .and_then(|invocation| invocation.yielder.clone())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use super::{Resumer, YieldError, ERR_YIELDED};
    use crate::{
        api::error::RuntimeError,
        experimental::{get_yielder, with_yielder},
        Context,
    };

    #[derive(Default)]
    struct TestResumer {
        cancelled: AtomicBool,
    }

    impl Resumer for TestResumer {
        fn resume(
            &self,
            _ctx: &Context,
            host_results: &[u64],
        ) -> crate::api::error::Result<Vec<u64>> {
            Ok(host_results.to_vec())
        }

        fn cancel(&self) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn yield_error_exposes_resumer() {
        let resumer: Arc<dyn Resumer> = Arc::new(TestResumer::default());
        let err = YieldError::new(Some(resumer.clone()));
        assert!(Arc::ptr_eq(
            &resumer,
            &err.resumer().expect("resumer should be present")
        ));
    }

    #[test]
    fn yield_error_matches_sentinel() {
        let err = RuntimeError::from(YieldError::new(Some(Arc::new(TestResumer::default()))));
        let dyn_err: &(dyn std::error::Error + 'static) = &err;
        assert!(YieldError::is_yielded(dyn_err));
        assert_eq!("wasm execution yielded", err.to_string());
        assert_eq!("wasm execution yielded", ERR_YIELDED.to_string());
    }

    #[test]
    fn yield_error_can_exist_without_resumer() {
        let err = YieldError::new(None);
        assert!(err.resumer().is_none());
        assert_eq!("wasm execution yielded", err.to_string());
    }

    #[test]
    fn get_yielder_is_none_without_runtime_injection() {
        assert!(get_yielder(&Context::default()).is_none());
        assert!(get_yielder(&with_yielder(&Context::default())).is_none());
    }

    #[test]
    fn with_yielder_marks_context_for_runtime_injection() {
        let ctx = with_yielder(&Context::default());
        assert!(ctx.yielder_enabled);
        assert!(get_yielder(&ctx).is_none());
    }
}
