use std::sync::Arc;

use crate::ctx_keys::Context;

pub trait CloseNotifier: Send + Sync {
    fn close_notify(&self, _ctx: &Context, _exit_code: u32) {}
}

impl<F> CloseNotifier for F
where
    F: Fn(&Context, u32) + Send + Sync,
{
    fn close_notify(&self, ctx: &Context, exit_code: u32) {
        (self)(ctx, exit_code);
    }
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

pub trait IntoCloseNotifier {
    fn into_close_notifier(self) -> Option<Arc<dyn CloseNotifier>>;
}

impl<T> IntoCloseNotifier for T
where
    T: CloseNotifier + 'static,
{
    fn into_close_notifier(self) -> Option<Arc<dyn CloseNotifier>> {
        Some(Arc::new(self))
    }
}

impl<T> IntoCloseNotifier for Option<T>
where
    T: CloseNotifier + 'static,
{
    fn into_close_notifier(self) -> Option<Arc<dyn CloseNotifier>> {
        self.map(|notifier| Arc::new(notifier) as Arc<dyn CloseNotifier>)
    }
}

pub fn with_close_notifier(ctx: &Context, notifier: impl IntoCloseNotifier) -> Context {
    let Some(notifier) = notifier.into_close_notifier() else {
        return ctx.clone();
    };
    let mut cloned = ctx.clone();
    cloned.close_notifier = Some(notifier);
    cloned
}

pub fn get_close_notifier(ctx: &Context) -> Option<Arc<dyn CloseNotifier>> {
    ctx.close_notifier.clone()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    };

    use super::{get_close_notifier, with_close_notifier, CloseNotifier, CloseNotifyFn};
    use crate::ctx_keys::Context;

    struct RecordingNotifier {
        exit_code: Arc<AtomicU32>,
    }

    impl CloseNotifier for RecordingNotifier {
        fn close_notify(&self, _ctx: &Context, exit_code: u32) {
            self.exit_code.store(exit_code, Ordering::SeqCst);
        }
    }

    #[test]
    fn get_close_notifier_not_set() {
        assert!(get_close_notifier(&Context::default()).is_none());
    }

    #[test]
    fn with_close_notifier_round_trip() {
        let exit_code = Arc::new(AtomicU32::new(0));
        let ctx = with_close_notifier(
            &Context::default(),
            RecordingNotifier {
                exit_code: exit_code.clone(),
            },
        );

        let notifier = get_close_notifier(&ctx).expect("notifier should be present");
        notifier.close_notify(&ctx, 7);
        assert_eq!(7, exit_code.load(Ordering::SeqCst));
    }

    #[test]
    fn close_notify_fn_invokes_wrapped_callback() {
        let exit_code = Arc::new(AtomicU32::new(0));
        let notifier = CloseNotifyFn::new({
            let exit_code = exit_code.clone();
            move |_ctx: &Context, code| {
                exit_code.store(code, Ordering::SeqCst);
            }
        });

        notifier.close_notify(&Context::default(), 11);
        assert_eq!(11, exit_code.load(Ordering::SeqCst));
    }

    #[test]
    fn with_close_notifier_accepts_closure() {
        let exit_code = Arc::new(AtomicU32::new(0));
        let ctx = with_close_notifier(&Context::default(), {
            let exit_code = exit_code.clone();
            move |_ctx: &Context, code| {
                exit_code.store(code, Ordering::SeqCst);
            }
        });

        get_close_notifier(&ctx)
            .expect("notifier should be present")
            .close_notify(&ctx, 5);
        assert_eq!(5, exit_code.load(Ordering::SeqCst));
    }

    #[test]
    fn with_close_notifier_none_is_noop() {
        let mut ctx = Context::default();
        ctx.insert(crate::ctx_keys::ContextKey::custom("marker"), "ok");

        let updated = with_close_notifier(&ctx, Option::<RecordingNotifier>::None);

        assert!(get_close_notifier(&updated).is_none());
        assert_eq!(
            Some("ok"),
            updated.get(&crate::ctx_keys::ContextKey::custom("marker"))
        );
    }
}
