use std::sync::Arc;

use razero_compiler::call_engine::CallEngineError;
use razero_compiler::wazevoapi::exitcode::{ExitCode, EXIT_CODE_MASK};
use razero_wasm::wasmruntime;

use crate::{
    api::{error::RuntimeError, wasm::Module},
    ctx_keys::Context,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrapCause {
    InvalidConversionToInteger,
    IntegerOverflow,
    IntegerDivideByZero,
    Unreachable,
    OutOfBoundsMemoryAccess,
    InvalidTableAccess,
    IndirectCallTypeMismatch,
    UnalignedAtomic,
    FuelExhausted,
    PolicyDenied,
    MemoryFault,
}

#[derive(Clone)]
pub struct TrapObservation {
    pub module: Module,
    pub cause: TrapCause,
    pub err: RuntimeError,
}

pub trait TrapObserver: Send + Sync {
    fn observe_trap(&self, ctx: &Context, observation: TrapObservation);
}

impl<F> TrapObserver for F
where
    F: Fn(&Context, TrapObservation) + Send + Sync,
{
    fn observe_trap(&self, ctx: &Context, observation: TrapObservation) {
        (self)(ctx, observation);
    }
}

pub fn with_trap_observer(ctx: &Context, observer: impl TrapObserver + 'static) -> Context {
    let mut cloned = ctx.clone();
    cloned.trap_observer = Some(Arc::new(observer));
    cloned
}

pub fn get_trap_observer(ctx: &Context) -> Option<Arc<dyn TrapObserver>> {
    ctx.trap_observer.clone()
}

pub fn trap_cause_of(err: &RuntimeError) -> Option<TrapCause> {
    err.exit_code()
        .and_then(trap_cause_from_exit_code)
        .or_else(|| trap_cause_from_message(&err.message()))
}

pub(crate) fn trap_cause_of_call_engine_error(err: &CallEngineError) -> Option<TrapCause> {
    match err {
        CallEngineError::ModuleExit(err) => {
            trap_cause_from_exit_code(err.exit_code & EXIT_CODE_MASK)
        }
        CallEngineError::Runtime(err) => trap_cause_from_wasmruntime(*err),
        CallEngineError::UnsupportedExit { exit_code, .. } => {
            trap_cause_from_exit_code(exit_code.raw() & EXIT_CODE_MASK)
        }
        CallEngineError::InvalidParamCount { .. } | CallEngineError::Host(_) => None,
    }
}

fn trap_cause_from_exit_code(exit_code: u32) -> Option<TrapCause> {
    match ExitCode::new(exit_code & EXIT_CODE_MASK) {
        ExitCode::INVALID_CONVERSION_TO_INTEGER => Some(TrapCause::InvalidConversionToInteger),
        ExitCode::INTEGER_OVERFLOW => Some(TrapCause::IntegerOverflow),
        ExitCode::INTEGER_DIVISION_BY_ZERO => Some(TrapCause::IntegerDivideByZero),
        ExitCode::UNREACHABLE => Some(TrapCause::Unreachable),
        ExitCode::MEMORY_OUT_OF_BOUNDS => Some(TrapCause::OutOfBoundsMemoryAccess),
        ExitCode::TABLE_OUT_OF_BOUNDS | ExitCode::INDIRECT_CALL_NULL_POINTER => {
            Some(TrapCause::InvalidTableAccess)
        }
        ExitCode::INDIRECT_CALL_TYPE_MISMATCH => Some(TrapCause::IndirectCallTypeMismatch),
        ExitCode::UNALIGNED_ATOMIC => Some(TrapCause::UnalignedAtomic),
        ExitCode::FUEL_EXHAUSTED => Some(TrapCause::FuelExhausted),
        ExitCode::POLICY_DENIED => Some(TrapCause::PolicyDenied),
        ExitCode::MEMORY_FAULT => Some(TrapCause::MemoryFault),
        _ => None,
    }
}

fn trap_cause_from_message(message: &str) -> Option<TrapCause> {
    if message.contains("invalid conversion to integer") {
        Some(TrapCause::InvalidConversionToInteger)
    } else if message.contains("integer overflow") {
        Some(TrapCause::IntegerOverflow)
    } else if message.contains("integer divide by zero")
        || message.contains("integer division by zero")
    {
        Some(TrapCause::IntegerDivideByZero)
    } else if message.contains("unreachable") {
        Some(TrapCause::Unreachable)
    } else if message.contains("out of bounds memory access") {
        Some(TrapCause::OutOfBoundsMemoryAccess)
    } else if message.contains("invalid table access") || message.contains("table out of bounds") {
        Some(TrapCause::InvalidTableAccess)
    } else if message.contains("indirect call type mismatch") {
        Some(TrapCause::IndirectCallTypeMismatch)
    } else if message.contains("unaligned atomic") {
        Some(TrapCause::UnalignedAtomic)
    } else if message.contains("fuel exhausted") {
        Some(TrapCause::FuelExhausted)
    } else if message.contains("policy denied") {
        Some(TrapCause::PolicyDenied)
    } else if message.contains("memory fault") {
        Some(TrapCause::MemoryFault)
    } else {
        None
    }
}

fn trap_cause_from_wasmruntime(err: wasmruntime::RuntimeError) -> Option<TrapCause> {
    if err == wasmruntime::ERR_RUNTIME_INVALID_CONVERSION_TO_INTEGER {
        Some(TrapCause::InvalidConversionToInteger)
    } else if err == wasmruntime::ERR_RUNTIME_INTEGER_OVERFLOW {
        Some(TrapCause::IntegerOverflow)
    } else if err == wasmruntime::ERR_RUNTIME_INTEGER_DIVIDE_BY_ZERO {
        Some(TrapCause::IntegerDivideByZero)
    } else if err == wasmruntime::ERR_RUNTIME_UNREACHABLE {
        Some(TrapCause::Unreachable)
    } else if err == wasmruntime::ERR_RUNTIME_OUT_OF_BOUNDS_MEMORY_ACCESS {
        Some(TrapCause::OutOfBoundsMemoryAccess)
    } else if err == wasmruntime::ERR_RUNTIME_INVALID_TABLE_ACCESS {
        Some(TrapCause::InvalidTableAccess)
    } else if err == wasmruntime::ERR_RUNTIME_INDIRECT_CALL_TYPE_MISMATCH {
        Some(TrapCause::IndirectCallTypeMismatch)
    } else if err == wasmruntime::ERR_RUNTIME_UNALIGNED_ATOMIC {
        Some(TrapCause::UnalignedAtomic)
    } else if err == wasmruntime::ERR_RUNTIME_FUEL_EXHAUSTED {
        Some(TrapCause::FuelExhausted)
    } else if err == wasmruntime::ERR_RUNTIME_POLICY_DENIED {
        Some(TrapCause::PolicyDenied)
    } else if err == wasmruntime::ERR_RUNTIME_MEMORY_FAULT {
        Some(TrapCause::MemoryFault)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use razero_compiler::wazevoapi::exitcode::ExitCode;

    use razero_compiler::call_engine::CallEngineError;

    use super::{
        get_trap_observer, trap_cause_of, trap_cause_of_call_engine_error, with_trap_observer,
        TrapCause,
    };
    use crate::{api::error::RuntimeError, config::ModuleConfig, runtime::Runtime, Context};

    #[test]
    fn trap_observer_round_trips_through_context() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = with_trap_observer(&Context::default(), {
            let events = events.clone();
            move |_ctx: &Context, observation: super::TrapObservation| {
                events.lock().expect("observer events poisoned").push((
                    observation.module.name().map(str::to_string),
                    observation.cause,
                    observation.err.exit_code(),
                ));
            }
        });

        let observer = get_trap_observer(&ctx).expect("observer should exist");
        let runtime = Runtime::new();
        let compiled = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            ])
            .unwrap();
        let module = runtime
            .instantiate(&compiled, ModuleConfig::new().with_name("guest"))
            .unwrap();
        observer.observe_trap(
            &ctx,
            super::TrapObservation {
                module,
                cause: TrapCause::MemoryFault,
                err: RuntimeError::new("memory fault"),
            },
        );

        assert_eq!(
            vec![(Some("guest".to_string()), TrapCause::MemoryFault, None)],
            *events.lock().expect("observer events poisoned")
        );
    }

    #[test]
    fn trap_cause_of_prefers_exit_codes() {
        assert_eq!(
            Some(TrapCause::MemoryFault),
            trap_cause_of(&RuntimeError::from(crate::ExitError::new(
                ExitCode::MEMORY_FAULT.raw()
            )))
        );
        assert_eq!(
            Some(TrapCause::FuelExhausted),
            trap_cause_of(&RuntimeError::new("fuel exhausted"))
        );
        assert_eq!(None, trap_cause_of(&RuntimeError::new("boom")));
        assert_eq!(
            Some(TrapCause::MemoryFault),
            trap_cause_of_call_engine_error(&CallEngineError::Runtime(
                razero_wasm::wasmruntime::ERR_RUNTIME_MEMORY_FAULT,
            ))
        );
    }
}
