use std::sync::Arc;

use crate::ctx_keys::Context;

pub trait TimeProvider: Send + Sync {
    fn walltime(&self) -> (i64, i32);
    fn nanotime(&self) -> i64;
    fn nanosleep(&self, ns: i64);
}

pub trait IntoTimeProvider {
    fn into_time_provider(self) -> Option<Arc<dyn TimeProvider>>;
}

impl<T> IntoTimeProvider for T
where
    T: TimeProvider + 'static,
{
    fn into_time_provider(self) -> Option<Arc<dyn TimeProvider>> {
        Some(Arc::new(self))
    }
}

impl<T> IntoTimeProvider for Option<T>
where
    T: TimeProvider + 'static,
{
    fn into_time_provider(self) -> Option<Arc<dyn TimeProvider>> {
        self.map(|provider| Arc::new(provider) as Arc<dyn TimeProvider>)
    }
}

pub fn with_time_provider(ctx: &Context, provider: impl IntoTimeProvider) -> Context {
    let Some(provider) = provider.into_time_provider() else {
        return ctx.clone();
    };
    let mut cloned = ctx.clone();
    cloned.time_provider = Some(provider);
    cloned
}

pub fn get_time_provider(ctx: &Context) -> Option<Arc<dyn TimeProvider>> {
    ctx.time_provider.clone()
}

#[cfg(test)]
mod tests {
    use super::{get_time_provider, with_time_provider, TimeProvider};
    use crate::ctx_keys::Context;
    use std::sync::{Arc, Mutex};

    struct StubTimeProvider {
        sleeps: Arc<Mutex<Vec<i64>>>,
    }

    impl TimeProvider for StubTimeProvider {
        fn walltime(&self) -> (i64, i32) {
            (1, 2)
        }

        fn nanotime(&self) -> i64 {
            3
        }

        fn nanosleep(&self, ns: i64) {
            self.sleeps.lock().expect("sleep log poisoned").push(ns);
        }
    }

    #[test]
    fn get_time_provider_not_set() {
        assert!(get_time_provider(&Context::default()).is_none());
    }

    #[test]
    fn with_time_provider_none_preserves_empty_context() {
        let ctx = with_time_provider(&Context::default(), Option::<StubTimeProvider>::None);
        assert!(get_time_provider(&ctx).is_none());
    }

    #[test]
    fn time_provider_round_trips_through_context() {
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let ctx = with_time_provider(
            &Context::default(),
            StubTimeProvider {
                sleeps: sleeps.clone(),
            },
        );
        let provider = get_time_provider(&ctx).expect("time provider should exist");

        assert_eq!((1, 2), provider.walltime());
        assert_eq!(3, provider.nanotime());
        provider.nanosleep(7);
        assert_eq!(vec![7], *sleeps.lock().expect("sleep log poisoned"));
    }
}
