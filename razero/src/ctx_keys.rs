use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use crate::api::wasm::FunctionDefinition;
use crate::experimental::{
    close_notifier::CloseNotifier,
    fuel::FuelController,
    fuel_observer::FuelObserver,
    host_call_policy::HostCallPolicy,
    listener::FunctionListenerFactory,
    listener::{FunctionListener, StackFrame},
    memory::MemoryAllocator,
    r#yield::Yielder,
    snapshotter::Snapshotter,
    time_provider::TimeProvider,
    yield_policy::YieldPolicy,
};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ContextKey {
    Fuel,
    FunctionListener,
    Snapshotter,
    Yielder,
    Resumer,
    MemoryAllocator,
    CloseNotifier,
    Custom(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextDoneError {
    Canceled,
    DeadlineExceeded,
}

impl ContextKey {
    pub fn custom(name: impl Into<String>) -> Self {
        Self::Custom(name.into())
    }
}

#[derive(Clone, Default)]
pub struct Context {
    values: BTreeMap<ContextKey, String>,
    lifecycle: Option<ContextLifecycle>,
    pub(crate) compilation_workers: Option<isize>,
    pub(crate) fuel_controller: Option<Arc<dyn FuelController>>,
    pub(crate) fuel_observer: Option<Arc<dyn FuelObserver>>,
    pub(crate) function_listener_factory: Option<Arc<dyn FunctionListenerFactory>>,
    pub(crate) snapshotter_enabled: bool,
    pub(crate) yielder_enabled: bool,
    pub(crate) memory_allocator: Option<Arc<dyn MemoryAllocator>>,
    pub(crate) close_notifier: Option<Arc<dyn CloseNotifier>>,
    pub(crate) host_call_policy: Option<Arc<dyn HostCallPolicy>>,
    pub(crate) import_resolver: Option<crate::experimental::import_resolver::ImportResolverConfig>,
    pub(crate) import_resolver_observer:
        Option<Arc<dyn crate::experimental::import_resolver_observer::ImportResolverObserver>>,
    pub(crate) host_call_policy_observer:
        Option<Arc<dyn crate::experimental::host_call_policy_observer::HostCallPolicyObserver>>,
    pub(crate) trap_observer: Option<Arc<dyn crate::experimental::trap::TrapObserver>>,
    pub(crate) time_provider: Option<Arc<dyn TimeProvider>>,
    pub(crate) yield_policy: Option<Arc<dyn YieldPolicy>>,
    pub(crate) yield_policy_observer:
        Option<Arc<dyn crate::experimental::yield_policy_observer::YieldPolicyObserver>>,
    pub(crate) invocation: Option<InvocationContext>,
}

#[derive(Clone)]
struct ContextLifecycle {
    inner: Arc<ContextLifecycleInner>,
}

struct ContextLifecycleInner {
    state: Mutex<ContextLifecycleState>,
    cv: Condvar,
}

#[derive(Clone, Copy, Default)]
struct ContextLifecycleState {
    deadline: Option<Instant>,
    done: Option<ContextDoneError>,
}

#[derive(Clone)]
pub struct CancelHandle {
    lifecycle: ContextLifecycle,
}

#[derive(Clone)]
pub(crate) struct InvocationContext {
    pub(crate) fuel_remaining: Option<Arc<AtomicI64>>,
    pub(crate) snapshotter: Option<Arc<dyn Snapshotter>>,
    pub(crate) yielder: Option<Arc<dyn Yielder>>,
    #[allow(dead_code)]
    pub(crate) function_listener: Option<Arc<dyn FunctionListener>>,
    #[allow(dead_code)]
    pub(crate) function_definition: Option<FunctionDefinition>,
    pub(crate) listener_stack: Vec<StackFrame>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: ContextKey, value: impl Into<String>) -> Option<String> {
        self.values.insert(key, value.into())
    }

    pub fn get(&self, key: &ContextKey) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn with_cancel(&self) -> (Self, CancelHandle) {
        self.with_child_lifecycle(None)
    }

    pub fn with_timeout(&self, timeout: Duration) -> Self {
        self.with_deadline(
            Instant::now()
                .checked_add(timeout)
                .unwrap_or_else(Instant::now),
        )
    }

    pub fn with_deadline(&self, deadline: Instant) -> Self {
        self.with_child_lifecycle(Some(deadline)).0
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.lifecycle.as_ref().and_then(ContextLifecycle::deadline)
    }

    pub fn done_error(&self) -> Option<ContextDoneError> {
        self.lifecycle
            .as_ref()
            .and_then(ContextLifecycle::done_error)
    }

    pub fn is_done(&self) -> bool {
        self.done_error().is_some()
    }

    fn with_child_lifecycle(&self, deadline: Option<Instant>) -> (Self, CancelHandle) {
        let lifecycle = ContextLifecycle::new(match (self.deadline(), deadline) {
            (Some(parent), Some(child)) => Some(parent.min(child)),
            (Some(parent), None) => Some(parent),
            (None, Some(child)) => Some(child),
            (None, None) => None,
        });

        if let Some(reason) = self.done_error() {
            lifecycle.trigger(reason);
        } else if let Some(parent) = self.lifecycle.clone() {
            let child = lifecycle.clone();
            thread::spawn(move || loop {
                if child.done_error().is_some() {
                    return;
                }
                if let Some(reason) = parent.done_error() {
                    child.trigger(reason);
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            });
        }

        let mut cloned = self.clone();
        cloned.lifecycle = Some(lifecycle.clone());
        (cloned, CancelHandle { lifecycle })
    }

    pub(crate) fn with_invocation(&self, invocation: InvocationContext) -> Self {
        let mut cloned = self.clone();
        cloned.invocation = Some(invocation);
        cloned
    }

    pub(crate) fn with_listener_stack(&self, listener_stack: Vec<StackFrame>) -> Self {
        let mut cloned = self.clone();
        let invocation = cloned.invocation.take().unwrap_or(InvocationContext {
            fuel_remaining: None,
            snapshotter: None,
            yielder: None,
            function_listener: None,
            function_definition: None,
            listener_stack: Vec::new(),
        });
        cloned.invocation = Some(InvocationContext {
            listener_stack,
            ..invocation
        });
        cloned
    }

    pub(crate) fn with_function_definition(&self, function_definition: FunctionDefinition) -> Self {
        let mut cloned = self.clone();
        let invocation = cloned.invocation.take().unwrap_or(InvocationContext {
            fuel_remaining: None,
            snapshotter: None,
            yielder: None,
            function_listener: None,
            function_definition: None,
            listener_stack: Vec::new(),
        });
        cloned.invocation = Some(InvocationContext {
            function_definition: Some(function_definition),
            ..invocation
        });
        cloned
    }

    pub(crate) fn has_lifecycle(&self) -> bool {
        self.lifecycle.is_some()
    }

    pub(crate) fn wait_until_done_or_stopped(&self, stop: &AtomicBool) -> Option<ContextDoneError> {
        self.lifecycle
            .as_ref()
            .and_then(|lifecycle| lifecycle.wait_until_done_or_stopped(stop))
    }
}

impl ContextLifecycle {
    fn new(deadline: Option<Instant>) -> Self {
        Self {
            inner: Arc::new(ContextLifecycleInner {
                state: Mutex::new(ContextLifecycleState {
                    deadline,
                    done: None,
                }),
                cv: Condvar::new(),
            }),
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.inner
            .state
            .lock()
            .expect("context lifecycle poisoned")
            .deadline
    }

    fn done_error(&self) -> Option<ContextDoneError> {
        let mut state = self.inner.state.lock().expect("context lifecycle poisoned");
        if state.done.is_none()
            && state
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            state.done = Some(ContextDoneError::DeadlineExceeded);
            self.inner.cv.notify_all();
        }
        state.done
    }

    fn trigger(&self, reason: ContextDoneError) {
        let mut state = self.inner.state.lock().expect("context lifecycle poisoned");
        if state.done.is_none() {
            state.done = Some(reason);
            self.inner.cv.notify_all();
        }
    }

    fn wait_until_done_or_stopped(&self, stop: &AtomicBool) -> Option<ContextDoneError> {
        let mut state = self.inner.state.lock().expect("context lifecycle poisoned");
        loop {
            if stop.load(Ordering::SeqCst) {
                return None;
            }

            if state.done.is_none()
                && state
                    .deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
            {
                state.done = Some(ContextDoneError::DeadlineExceeded);
                self.inner.cv.notify_all();
            }

            if let Some(reason) = state.done {
                return Some(reason);
            }

            let wait_for = state
                .deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|| Duration::from_millis(50))
                .min(Duration::from_millis(50));
            let (next, _) = self
                .inner
                .cv
                .wait_timeout(state, wait_for)
                .expect("context lifecycle poisoned");
            state = next;
        }
    }
}

impl CancelHandle {
    pub fn cancel(&self) {
        self.lifecycle.trigger(ContextDoneError::Canceled);
    }
}

#[cfg(test)]
mod tests {
    use super::{Context, ContextDoneError};
    use std::time::{Duration, Instant};

    #[test]
    fn cancel_handle_marks_context_done() {
        let (ctx, cancel) = Context::default().with_cancel();
        assert!(!ctx.is_done());
        cancel.cancel();
        assert_eq!(Some(ContextDoneError::Canceled), ctx.done_error());
    }

    #[test]
    fn timeout_marks_context_done() {
        let ctx = Context::default().with_timeout(Duration::from_millis(1));
        let deadline = ctx.deadline().expect("deadline should be set");
        assert!(deadline <= Instant::now() + Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(Some(ContextDoneError::DeadlineExceeded), ctx.done_error());
    }
}
