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

pub fn with_fuel_controller(
    ctx: &Context,
    controller: impl FuelController + 'static,
) -> Context {
    let mut cloned = ctx.clone();
    cloned.fuel_controller = Some(Arc::new(controller));
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
        .ok_or_else(|| RuntimeError::new("no fuel accessor in context: fuel not enabled or not in a host function"))?;
    accessor.fetch_add(amount, Ordering::SeqCst);
    Ok(())
}

pub fn remaining_fuel(ctx: &Context) -> Result<i64> {
    let accessor = ctx
        .invocation
        .as_ref()
        .and_then(|invocation| invocation.fuel_remaining.as_ref())
        .cloned()
        .ok_or_else(|| RuntimeError::new("no fuel accessor in context: fuel not enabled or not in a host function"))?;
    Ok(accessor.load(Ordering::SeqCst))
}
