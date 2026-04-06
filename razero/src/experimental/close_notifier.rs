use std::sync::Arc;

use crate::ctx_keys::Context;

pub trait CloseNotifier: Send + Sync {
    fn close_notify(&self, _ctx: &Context, _exit_code: u32) {}
}

pub struct CloseNotifyFn<F>(F);

impl<F> CloseNotifyFn<F> {
    pub fn new(callback: F) -> Self {
        Self(callback)
    }
}

impl<F> CloseNotifier for CloseNotifyFn<F>
where
    F: Fn(&Context, u32) + Send + Sync,
{
    fn close_notify(&self, ctx: &Context, exit_code: u32) {
        (self.0)(ctx, exit_code);
    }
}

pub fn with_close_notifier(
    ctx: &Context,
    notifier: impl CloseNotifier + 'static,
) -> Context {
    let mut cloned = ctx.clone();
    cloned.close_notifier = Some(Arc::new(notifier));
    cloned
}

pub fn get_close_notifier(ctx: &Context) -> Option<Arc<dyn CloseNotifier>> {
    ctx.close_notifier.clone()
}
