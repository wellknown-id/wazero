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

#[derive(Clone)]
pub struct YieldError {
    resumer: Arc<dyn Resumer>,
}

impl YieldError {
    pub fn new(resumer: Arc<dyn Resumer>) -> Self {
        Self { resumer }
    }

    pub fn resumer(&self) -> Arc<dyn Resumer> {
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

impl Error for YieldError {}

#[derive(Clone)]
pub(crate) struct YieldSuspend {
    pub(crate) resumer: Arc<dyn Resumer>,
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
