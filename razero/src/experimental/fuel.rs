use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc,
};

use crate::{
    api::error::{Result, RuntimeError},
    ctx_keys::Context,
};

pub trait FuelController: Send + Sync {
    fn budget(&self) -> i64;
    fn consumed(&self, amount: i64);
}

#[derive(Debug)]
pub struct SimpleFuelController {
    budget: i64,
    consumed: AtomicI64,
}

impl SimpleFuelController {
    pub fn new(budget: i64) -> Self {
        Self {
            budget,
            consumed: AtomicI64::new(0),
        }
    }

    pub fn total_consumed(&self) -> i64 {
        self.consumed.load(Ordering::SeqCst)
    }
}

impl FuelController for SimpleFuelController {
    fn budget(&self) -> i64 {
        self.budget
    }

    fn consumed(&self, amount: i64) {
        self.consumed.fetch_add(amount, Ordering::SeqCst);
    }
}

pub struct AggregatingFuelController {
    parent: Option<Arc<dyn FuelController>>,
    budget: i64,
    consumed: AtomicI64,
}

impl AggregatingFuelController {
    pub fn new(parent: Option<Arc<dyn FuelController>>, budget: i64) -> Self {
        Self {
            parent,
            budget,
            consumed: AtomicI64::new(0),
        }
    }

    pub fn total_consumed(&self) -> i64 {
        self.consumed.load(Ordering::SeqCst)
    }
}

impl FuelController for AggregatingFuelController {
    fn budget(&self) -> i64 {
        self.budget
    }

    fn consumed(&self, amount: i64) {
        self.consumed.fetch_add(amount, Ordering::SeqCst);
        if let Some(parent) = &self.parent {
            parent.consumed(amount);
        }
    }
}

pub trait IntoFuelController {
    fn into_fuel_controller(self) -> Option<Arc<dyn FuelController>>;
}

impl<T> IntoFuelController for T
where
    T: FuelController + 'static,
{
    fn into_fuel_controller(self) -> Option<Arc<dyn FuelController>> {
        Some(Arc::new(self))
    }
}

impl<T> IntoFuelController for Option<T>
where
    T: FuelController + 'static,
{
    fn into_fuel_controller(self) -> Option<Arc<dyn FuelController>> {
        self.map(|controller| Arc::new(controller) as Arc<dyn FuelController>)
    }
}

pub fn with_fuel_controller(ctx: &Context, controller: impl IntoFuelController) -> Context {
    let Some(controller) = controller.into_fuel_controller() else {
        return ctx.clone();
    };
    let mut cloned = ctx.clone();
    cloned.fuel_controller = Some(controller);
    cloned
}

pub fn get_fuel_controller(ctx: &Context) -> Option<Arc<dyn FuelController>> {
    ctx.fuel_controller.clone()
}

pub fn add_fuel(ctx: &Context, amount: i64) -> Result<()> {
    let accessor = ctx
        .invocation
        .as_ref()
        .and_then(|invocation| invocation.fuel_remaining.as_ref())
        .cloned()
        .ok_or_else(|| {
            RuntimeError::new(
                "no fuel accessor in context: fuel not enabled or not in a host function",
            )
        })?;
    accessor.fetch_add(amount, Ordering::SeqCst);
    Ok(())
}

pub fn remaining_fuel(ctx: &Context) -> Result<i64> {
    let accessor = ctx
        .invocation
        .as_ref()
        .and_then(|invocation| invocation.fuel_remaining.as_ref())
        .cloned()
        .ok_or_else(|| {
            RuntimeError::new(
                "no fuel accessor in context: fuel not enabled or not in a host function",
            )
        })?;
    Ok(accessor.load(Ordering::SeqCst))
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicI64, Ordering},
            Arc,
        },
        thread,
    };

    use super::{
        add_fuel, get_fuel_controller, remaining_fuel, with_fuel_controller,
        AggregatingFuelController, FuelController, SimpleFuelController,
    };
    use crate::ctx_keys::{Context, InvocationContext};

    #[test]
    fn simple_fuel_controller_budget() {
        let controller = SimpleFuelController::new(42);
        assert_eq!(42, controller.budget());
    }

    #[test]
    fn simple_fuel_controller_consumed() {
        let controller = SimpleFuelController::new(1000);
        controller.consumed(100);
        controller.consumed(200);
        assert_eq!(300, controller.total_consumed());
    }

    #[test]
    fn simple_fuel_controller_concurrent_consumption() {
        let controller = Arc::new(SimpleFuelController::new(1_000_000));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let controller = controller.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    controller.consumed(1);
                }
            }));
        }
        for handle in handles {
            handle.join().expect("fuel worker panicked");
        }
        assert_eq!(10_000, controller.total_consumed());
    }

    #[test]
    fn aggregating_fuel_controller_budget() {
        let parent = Arc::new(SimpleFuelController::new(1_000_000));
        let child = AggregatingFuelController::new(Some(parent.clone()), 100_000);
        assert_eq!(100_000, child.budget());
        assert_eq!(1_000_000, parent.budget());
    }

    #[test]
    fn aggregating_fuel_controller_consumed() {
        let parent = Arc::new(SimpleFuelController::new(1_000_000));
        let child = AggregatingFuelController::new(Some(parent.clone()), 100_000);
        child.consumed(500);
        assert_eq!(500, child.total_consumed());
        assert_eq!(500, parent.total_consumed());
    }

    #[test]
    fn aggregating_fuel_controller_nested_aggregation() {
        let root = Arc::new(SimpleFuelController::new(10_000_000));
        let alice = Arc::new(AggregatingFuelController::new(
            Some(root.clone()),
            1_000_000,
        ));
        let bob = AggregatingFuelController::new(Some(alice.clone()), 100_000);

        bob.consumed(42);
        alice.consumed(100);

        assert_eq!(42, bob.total_consumed());
        assert_eq!(142, alice.total_consumed());
        assert_eq!(142, root.total_consumed());
    }

    #[test]
    fn aggregating_fuel_controller_nil_parent() {
        let controller = AggregatingFuelController::new(None, 1000);
        controller.consumed(500);
        assert_eq!(500, controller.total_consumed());
    }

    #[test]
    fn get_fuel_controller_not_set() {
        assert!(get_fuel_controller(&Context::default()).is_none());
    }

    #[test]
    fn with_fuel_controller_round_trip() {
        let ctx = with_fuel_controller(&Context::default(), SimpleFuelController::new(42));
        let controller = get_fuel_controller(&ctx).expect("controller should be present");
        assert_eq!(42, controller.budget());
    }

    #[test]
    fn with_fuel_controller_override() {
        let ctx = with_fuel_controller(&Context::default(), SimpleFuelController::new(100));
        let ctx = with_fuel_controller(&ctx, SimpleFuelController::new(200));
        let controller = get_fuel_controller(&ctx).expect("controller should be present");
        assert_eq!(200, controller.budget());
    }

    #[test]
    fn with_fuel_controller_none_is_noop() {
        let mut ctx = Context::default();
        ctx.insert(crate::ctx_keys::ContextKey::custom("marker"), "ok");

        let updated = with_fuel_controller(&ctx, Option::<SimpleFuelController>::None);

        assert!(get_fuel_controller(&updated).is_none());
        assert_eq!(
            Some("ok"),
            updated.get(&crate::ctx_keys::ContextKey::custom("marker"))
        );
    }

    #[test]
    fn add_fuel_without_accessor_fails() {
        let err = add_fuel(&Context::default(), 100).unwrap_err();
        assert_eq!(
            "no fuel accessor in context: fuel not enabled or not in a host function",
            err.to_string()
        );
    }

    #[test]
    fn remaining_fuel_without_accessor_fails() {
        let err = remaining_fuel(&Context::default()).unwrap_err();
        assert_eq!(
            "no fuel accessor in context: fuel not enabled or not in a host function",
            err.to_string()
        );
    }

    #[test]
    fn add_fuel_with_accessor_mutates_remaining() {
        let fuel = Arc::new(AtomicI64::new(1000));
        let ctx = Context::default().with_invocation(InvocationContext {
            fuel_remaining: Some(fuel.clone()),
            snapshotter: None,
            yielder: None,
            function_listener: None,
            function_definition: None,
            listener_stack: Vec::new(),
        });

        assert_eq!(1000, remaining_fuel(&ctx).unwrap());
        add_fuel(&ctx, 500).unwrap();
        assert_eq!(1500, remaining_fuel(&ctx).unwrap());
        assert_eq!(1500, fuel.load(Ordering::SeqCst));
    }

    #[test]
    fn add_fuel_negative_can_overdraw() {
        let fuel = Arc::new(AtomicI64::new(1000));
        let ctx = Context::default().with_invocation(InvocationContext {
            fuel_remaining: Some(fuel.clone()),
            snapshotter: None,
            yielder: None,
            function_listener: None,
            function_definition: None,
            listener_stack: Vec::new(),
        });

        add_fuel(&ctx, -300).unwrap();
        assert_eq!(700, fuel.load(Ordering::SeqCst));
        add_fuel(&ctx, -800).unwrap();
        assert_eq!(-100, fuel.load(Ordering::SeqCst));
    }
}
