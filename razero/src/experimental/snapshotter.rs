use std::sync::{Arc, Mutex};

use crate::ctx_keys::Context;

#[derive(Clone, Default)]
pub struct Snapshot {
    restored_results: Arc<Mutex<Option<Vec<u64>>>>,
}

impl Snapshot {
    pub(crate) fn new(restored_results: Arc<Mutex<Option<Vec<u64>>>>) -> Self {
        Self { restored_results }
    }

    pub fn restore(&self, results: &[u64]) {
        *self.restored_results.lock().expect("snapshot poisoned") = Some(results.to_vec());
    }
}

pub trait Snapshotter: Send + Sync {
    fn snapshot(&self) -> Snapshot;
}

pub fn with_snapshotter(ctx: &Context) -> Context {
    let mut cloned = ctx.clone();
    cloned.snapshotter_enabled = true;
    cloned
}

pub fn get_snapshotter(ctx: &Context) -> Option<Arc<dyn Snapshotter>> {
    ctx.invocation
        .as_ref()
        .and_then(|invocation| invocation.snapshotter.clone())
}
