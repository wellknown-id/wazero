use std::sync::Arc;

use crate::{api::wasm::Module, ctx_keys::Context};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FuelEvent {
    Budgeted,
    Consumed,
    Recharged,
    Exhausted,
}

#[derive(Clone)]
#[non_exhaustive]
pub struct FuelObservation {
    pub module: Module,
    pub event: FuelEvent,
    pub budget: i64,
    pub remaining: i64,
}

pub trait FuelObserver: Send + Sync {
    fn observe_fuel(&self, ctx: &Context, observation: FuelObservation);
}

impl<F> FuelObserver for F
where
    F: Fn(&Context, FuelObservation) + Send + Sync,
{
    fn observe_fuel(&self, ctx: &Context, observation: FuelObservation) {
        (self)(ctx, observation);
    }
}

pub fn with_fuel_observer(ctx: &Context, observer: impl FuelObserver + 'static) -> Context {
    let mut cloned = ctx.clone();
    cloned.fuel_observer = Some(Arc::new(observer));
    cloned
}

pub fn get_fuel_observer(ctx: &Context) -> Option<Arc<dyn FuelObserver>> {
    ctx.fuel_observer.clone()
}

pub(crate) fn notify_fuel_observer(
    ctx: &Context,
    module: &Module,
    event: FuelEvent,
    budget: i64,
    remaining: i64,
) {
    let Some(observer) = get_fuel_observer(ctx) else {
        return;
    };
    observer.observe_fuel(
        ctx,
        FuelObservation {
            module: module.clone(),
            event,
            budget,
            remaining,
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{get_fuel_observer, with_fuel_observer, FuelEvent, FuelObservation};
    use crate::{config::ModuleConfig, runtime::Runtime, Context};

    #[test]
    fn fuel_observer_round_trips_through_context() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = with_fuel_observer(&Context::default(), {
            let events = events.clone();
            move |_ctx: &Context, observation: FuelObservation| {
                events.lock().expect("observer events poisoned").push((
                    observation.module.name().map(str::to_string),
                    observation.event,
                    observation.budget,
                    observation.remaining,
                ));
            }
        });

        let observer = get_fuel_observer(&ctx).expect("observer should exist");
        let runtime = Runtime::new();
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            ])
            .unwrap();
        let module = runtime
            .instantiate(&compiled, ModuleConfig::new().with_name("guest"))
            .unwrap();
        observer.observe_fuel(
            &ctx,
            FuelObservation {
                module,
                event: FuelEvent::Budgeted,
                budget: 7,
                remaining: 7,
            },
        );

        assert_eq!(
            vec![(Some("guest".to_string()), FuelEvent::Budgeted, 7, 7)],
            *events.lock().expect("observer events poisoned")
        );
    }
}
