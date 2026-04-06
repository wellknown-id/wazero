#![doc = "Host function dispatch primitives."]

use std::any::Any;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub type HostFuncResult = Result<(), HostFuncError>;
pub type HostFuncRef = Arc<dyn HostFunction>;

pub trait HostModuleContext: Any + Send + Sync {
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T> HostModuleContext for T
where
    T: Any + Send + Sync,
{
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[derive(Default)]
pub struct Caller<'a> {
    module: Option<&'a mut dyn HostModuleContext>,
    data: Option<&'a mut dyn Any>,
}

impl<'a> Caller<'a> {
    pub fn new(module: Option<&'a mut dyn HostModuleContext>) -> Self {
        Self { module, data: None }
    }

    pub fn with_data(
        module: Option<&'a mut dyn HostModuleContext>,
        data: Option<&'a mut dyn Any>,
    ) -> Self {
        Self { module, data }
    }

    pub fn module_mut<T>(&mut self) -> Option<&mut T>
    where
        T: Any + Send + Sync,
    {
        self.module
            .as_deref_mut()
            .and_then(|module| module.as_any_mut().downcast_mut::<T>())
    }

    pub fn data_mut<T>(&mut self) -> Option<&mut T>
    where
        T: Any,
    {
        self.data
            .as_deref_mut()
            .and_then(|data| data.downcast_mut::<T>())
    }
}

impl std::fmt::Debug for Caller<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Caller")
            .field("has_module", &self.module.is_some())
            .field("has_data", &self.data.is_some())
            .finish()
    }
}

pub trait HostFunction: Send + Sync + 'static {
    fn call(&self, caller: &mut Caller<'_>, stack: &mut [u64]) -> HostFuncResult;
}

struct ClosureHostFunction<F>(F);

impl<F> HostFunction for ClosureHostFunction<F>
where
    F: for<'a> Fn(&mut Caller<'a>, &mut [u64]) -> HostFuncResult + Send + Sync + 'static,
{
    fn call(&self, caller: &mut Caller<'_>, stack: &mut [u64]) -> HostFuncResult {
        (self.0)(caller, stack)
    }
}

struct StackHostFunction<F>(F);

impl<F> HostFunction for StackHostFunction<F>
where
    F: Fn(&mut [u64]) -> HostFuncResult + Send + Sync + 'static,
{
    fn call(&self, _caller: &mut Caller<'_>, stack: &mut [u64]) -> HostFuncResult {
        (self.0)(stack)
    }
}

pub fn host_func<F>(func: F) -> HostFuncRef
where
    F: for<'a> Fn(&mut Caller<'a>, &mut [u64]) -> HostFuncResult + Send + Sync + 'static,
{
    Arc::new(ClosureHostFunction(func))
}

pub fn stack_host_func<F>(func: F) -> HostFuncRef
where
    F: Fn(&mut [u64]) -> HostFuncResult + Send + Sync + 'static,
{
    Arc::new(StackHostFunction(func))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostFuncError {
    message: String,
}

impl HostFuncError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for HostFuncError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for HostFuncError {}

impl From<&str> for HostFuncError {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for HostFuncError {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{host_func, stack_host_func, Caller, HostFuncError};

    #[derive(Debug, Default)]
    struct TestModule {
        seen: u64,
    }

    #[derive(Debug, Default)]
    struct TestData {
        calls: u32,
    }

    #[test]
    fn stack_host_function_updates_stack() {
        let func = stack_host_func(|stack| {
            stack[0] = stack[0].wrapping_add(stack[1]);
            Ok(())
        });

        let mut caller = Caller::default();
        let mut stack = [20, 22];
        func.call(&mut caller, &mut stack).unwrap();

        assert_eq!([42, 22], stack);
    }

    #[test]
    fn caller_host_function_can_access_module_and_data() {
        let func = host_func(|caller, stack| {
            caller.module_mut::<TestModule>().unwrap().seen = stack[0];
            caller.data_mut::<TestData>().unwrap().calls += 1;
            stack[0] = 7;
            Ok(())
        });

        let mut module = TestModule::default();
        let mut data = TestData::default();
        let mut caller = Caller::with_data(Some(&mut module), Some(&mut data));
        let mut stack = [99];

        func.call(&mut caller, &mut stack).unwrap();

        assert_eq!(99, module.seen);
        assert_eq!(1, data.calls);
        assert_eq!([7], stack);
    }

    #[test]
    fn host_func_error_preserves_message() {
        let error = HostFuncError::new("boom");
        assert_eq!("boom", error.message());
        assert_eq!("boom", error.to_string());
    }
}
