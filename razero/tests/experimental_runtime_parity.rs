use std::{
    error::Error,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicI64, AtomicU32, Ordering},
        Arc, Mutex,
    },
    thread,
};

use razero::{
    add_fuel, get_fuel_controller, get_import_resolver, get_snapshotter, get_yielder,
    remaining_fuel, with_fuel_controller, with_function_listener_factory, with_import_resolver,
    with_snapshotter, with_trap_observer, with_yield_policy, with_yielder, trap_cause_of, Context,
    FunctionDefinition, FunctionListener,
    FunctionListenerFactory, Module, ModuleConfig, Runtime, RuntimeConfig, RuntimeError, Snapshot,
    TrapCause, ValueType, YieldError, ERR_YIELDED,
};

const YIELD_WASM: &[u8] = include_bytes!("../../experimental/testdata/yield.wasm");
const SNAPSHOT_WASM: &[u8] = include_bytes!("../../experimental/testdata/snapshot.wasm");
const OOB_LOAD_WASM: &[u8] = include_bytes!("../../testdata/oob_load.wasm");
const DIV_BY_ZERO_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, 0x03,
    0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x09, 0x01, 0x07,
    0x00, 0x41, 0x05, 0x41, 0x00, 0x6d, 0x0b,
];
const DIV_OVERFLOW_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, 0x03,
    0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x0d, 0x01, 0x0b,
    0x00, 0x41, 0x80, 0x80, 0x80, 0x80, 0x78, 0x41, 0x7f, 0x6d, 0x0b,
];
const GUEST_IMPORT_INC_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f,
    0x02, 0x0b, 0x01, 0x03, b'e', b'n', b'v', 0x03, b'i', b'n', b'c', 0x00, 0x00, 0x03, 0x02, 0x01,
    0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01, 0x0a, 0x08, 0x01, 0x06, 0x00, 0x20,
    0x00, 0x10, 0x00, 0x0b,
];
const LOOP_EXPORT_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02,
    0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x09, 0x01, 0x07, 0x00,
    0x03, 0x40, 0x0c, 0x00, 0x0b, 0x0b,
];

struct TrackingFuelController {
    budget: i64,
    consumed: Arc<AtomicI64>,
}

impl razero::FuelController for TrackingFuelController {
    fn budget(&self) -> i64 {
        self.budget
    }

    fn consumed(&self, amount: i64) {
        self.consumed.fetch_add(amount, Ordering::SeqCst);
    }
}

fn setup_yield_runtime() -> (Runtime, Module) {
    setup_yield_runtime_with_config(RuntimeConfig::new())
}

fn setup_yield_runtime_with_config(config: RuntimeConfig) -> (Runtime, Module) {
    let runtime = Runtime::with_config(config);
    runtime
        .new_host_module_builder("example")
        .new_function_builder()
        .with_func(
            |ctx, _module, _params| {
                get_yielder(&ctx)
                    .expect("yielder should be injected")
                    .r#yield();
                Ok(vec![0])
            },
            &[],
            &[ValueType::I32],
        )
        .with_name("async_work")
        .export("async_work")
        .instantiate(&Context::default())
        .unwrap();

    let guest = runtime
        .instantiate_binary(YIELD_WASM, ModuleConfig::new())
        .unwrap();
    (runtime, guest)
}

fn deny_all_yields(_ctx: &Context, _request: &razero::YieldPolicyRequest) -> bool {
    false
}

fn allow_all_yields(_ctx: &Context, _request: &razero::YieldPolicyRequest) -> bool {
    true
}

fn record_trap_observations(
    observations: Arc<Mutex<Vec<(TrapCause, String)>>>,
) -> impl Fn(&Context, razero::TrapObservation) + Send + Sync + 'static {
    move |_ctx: &Context, observation: razero::TrapObservation| {
        observations
            .lock()
            .expect("trap observations poisoned")
            .push((observation.cause, observation.err.to_string()));
    }
}

fn yielded(err: RuntimeError) -> YieldError {
    let dyn_err: &(dyn Error + 'static) = &err;
    assert!(YieldError::is_yielded(dyn_err));
    match err {
        RuntimeError::Yield(yield_error) => yield_error,
        other => panic!("expected yield error, got {other}"),
    }
}

#[test]
fn yield_basic_yield_and_resume() {
    let (_runtime, guest) = setup_yield_runtime();

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err();
    let yield_error = yielded(err);
    let results = yield_error
        .resumer()
        .expect("resumer should be present")
        .resume(&with_yielder(&Context::default()), &[42])
        .unwrap();

    assert_eq!(vec![142], results);
}

#[test]
fn yield_can_resume_from_another_thread() {
    let (_runtime, guest) = setup_yield_runtime();

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");

    let handle = thread::spawn(move || resumer.resume(&with_yielder(&Context::default()), &[7]));
    let results = handle.join().expect("resume thread panicked").unwrap();

    assert_eq!(vec![107], results);
}

#[test]
fn yield_call_while_suspended_is_rejected() {
    let (_runtime, guest) = setup_yield_runtime();
    let run = guest.exported_function("run").unwrap();

    let err = run
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");

    let err = run
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err();
    assert_eq!(
        "cannot call: module has suspended execution; resume or cancel the outstanding Resumer first",
        err.to_string()
    );

    assert_eq!(
        vec![142],
        resumer
            .resume(&with_yielder(&Context::default()), &[42])
            .unwrap()
    );
}

#[test]
fn yield_other_export_while_suspended_is_rejected() {
    let (_runtime, guest) = setup_yield_runtime();

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err();
    assert_eq!(
        "cannot call: module has suspended execution; resume or cancel the outstanding Resumer first",
        err.to_string()
    );

    assert_eq!(
        vec![142],
        resumer
            .resume(&with_yielder(&Context::default()), &[42])
            .unwrap()
    );
}

#[test]
fn yield_resume_rejects_wrong_host_result_count_and_can_retry() {
    let (_runtime, guest) = setup_yield_runtime();

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");

    let err = resumer
        .resume(&with_yielder(&Context::default()), &[])
        .unwrap_err();
    assert_eq!(
        "cannot resume: expected 1 host results, but got 0",
        err.to_string()
    );

    assert_eq!(
        vec![142],
        resumer
            .resume(&with_yielder(&Context::default()), &[42])
            .unwrap()
    );
}

#[test]
fn yield_spent_resumer_cannot_be_reused() {
    let (_runtime, guest) = setup_yield_runtime();

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");

    assert_eq!(
        vec![142],
        resumer
            .resume(&with_yielder(&Context::default()), &[42])
            .unwrap()
    );

    let err = resumer
        .resume(&with_yielder(&Context::default()), &[42])
        .unwrap_err();
    assert_eq!(
        "cannot resume: resumer has already been used",
        err.to_string()
    );
}

#[test]
fn yield_reyield_uses_fresh_resumer() {
    let (_runtime, guest) = setup_yield_runtime();

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err();
    let first_resumer = yielded(err).resumer().expect("resumer should be present");

    let err = first_resumer
        .resume(&with_yielder(&Context::default()), &[40])
        .unwrap_err();
    let second_resumer = yielded(err).resumer().expect("resumer should be present");

    first_resumer.cancel();
    let err = first_resumer
        .resume(&with_yielder(&Context::default()), &[1])
        .unwrap_err();
    assert_eq!(
        "cannot resume: resumer has already been used",
        err.to_string()
    );

    assert_eq!(
        vec![42],
        second_resumer
            .resume(&with_yielder(&Context::default()), &[2])
            .unwrap()
    );
}

#[test]
fn yield_policy_denies_guest_suspension_via_public_runtime() {
    let (_runtime, guest) =
        setup_yield_runtime_with_config(RuntimeConfig::new().with_yield_policy(deny_all_yields));

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err();

    assert_eq!("policy denied: cooperative yield", err.to_string());
    assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
}

#[test]
fn yield_policy_denial_notifies_trap_observer_via_public_runtime() {
    let (_runtime, guest) =
        setup_yield_runtime_with_config(RuntimeConfig::new().with_yield_policy(deny_all_yields));
    let observations = Arc::new(Mutex::new(Vec::new()));
    let base = Context::default();
    let yielding = with_yielder(&base);
    let ctx = with_trap_observer(&yielding, record_trap_observations(observations.clone()));

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&ctx, &[])
        .unwrap_err();

    assert_eq!("policy denied: cooperative yield", err.to_string());
    assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
    assert_eq!(
        vec![(TrapCause::PolicyDenied, "policy denied: cooperative yield".to_string())],
        *observations.lock().expect("trap observations poisoned")
    );
}

#[test]
fn yield_policy_denies_follow_on_suspension_via_public_resume_path() {
    let (_runtime, guest) = setup_yield_runtime();

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");

    let err = resumer
        .resume(
            &with_yield_policy(&with_yielder(&Context::default()), deny_all_yields),
            &[40],
        )
        .unwrap_err();

    assert_eq!("policy denied: cooperative yield", err.to_string());
    assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
}

#[test]
fn yield_policy_denial_notifies_trap_observer_via_public_resume_path() {
    let (_runtime, guest) = setup_yield_runtime();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let base = Context::default();
    let yielding = with_yielder(&base);
    let initial_ctx = with_trap_observer(&yielding, record_trap_observations(observations.clone()));

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");
    assert!(
        observations
            .lock()
            .expect("trap observations poisoned")
            .is_empty()
    );

    let resumed_base = Context::default();
    let resumed_yielding = with_yielder(&resumed_base);
    let resumed_policy = with_yield_policy(&resumed_yielding, deny_all_yields);
    let resumed_ctx = with_trap_observer(
        &resumed_policy,
        record_trap_observations(observations.clone()),
    );
    let err = resumer.resume(&resumed_ctx, &[40]).unwrap_err();

    assert_eq!("policy denied: cooperative yield", err.to_string());
    assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
    assert_eq!(
        vec![(TrapCause::PolicyDenied, "policy denied: cooperative yield".to_string())],
        *observations.lock().expect("trap observations poisoned")
    );
}

#[test]
fn yield_policy_context_overrides_runtime_default_via_public_runtime() {
    let (_runtime, guest) =
        setup_yield_runtime_with_config(RuntimeConfig::new().with_yield_policy(deny_all_yields));

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(
            &with_yield_policy(&with_yielder(&Context::default()), allow_all_yields),
            &[],
        )
        .unwrap_err();
    let yield_error = yielded(err);
    yield_error
        .resumer()
        .expect("resumer should be present")
        .cancel();
}

#[test]
fn yield_cancel_is_idempotent_and_blocks_resume() {
    let (_runtime, guest) = setup_yield_runtime();

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");

    resumer.cancel();
    resumer.cancel();

    let err = resumer
        .resume(&with_yielder(&Context::default()), &[42])
        .unwrap_err();
    assert!(err.to_string().contains("cancelled"));
}

#[test]
fn yield_without_enable_has_no_yielder() {
    assert!(get_yielder(&Context::default()).is_none());
}

#[test]
fn yield_error_matches_sentinel() {
    let err = YieldError::new(None);
    let dyn_err: &(dyn Error + 'static) = &err;

    assert!(YieldError::is_yielded(dyn_err));
    assert_eq!("wasm execution yielded", ERR_YIELDED.to_string());
}

#[test]
fn host_function_can_observe_and_mutate_fuel_accessor() {
    let runtime = Runtime::new();
    let module = runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |ctx, _module, params| {
                assert_eq!(4, remaining_fuel(&ctx).unwrap());
                add_fuel(&ctx, -2).unwrap();
                Ok(vec![params[0] + params[1]])
            },
            &[ValueType::I32, ValueType::I32],
            &[ValueType::I32],
        )
        .with_name("add")
        .export("add")
        .instantiate(&Context::default())
        .unwrap();

    let consumed = Arc::new(AtomicI64::new(0));
    let call_ctx = with_fuel_controller(
        &Context::default(),
        TrackingFuelController {
            budget: 5,
            consumed: consumed.clone(),
        },
    );

    let results = module
        .exported_function("add")
        .unwrap()
        .call_with_context(&call_ctx, &[20, 22])
        .unwrap();

    assert_eq!(vec![42], results);
    assert_eq!(3, consumed.load(Ordering::SeqCst));
}

#[test]
fn fuel_controller_override_takes_precedence_in_host_calls() {
    let runtime = Runtime::new();
    let module = runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |ctx, _module, _params| {
                let controller = get_fuel_controller(&ctx).expect("fuel controller should exist");
                Ok(vec![
                    controller.budget() as u64,
                    remaining_fuel(&ctx).unwrap() as u64,
                ])
            },
            &[],
            &[ValueType::I64, ValueType::I64],
        )
        .with_name("inspect")
        .export("inspect")
        .instantiate(&Context::default())
        .unwrap();

    let parent = with_fuel_controller(
        &Context::default(),
        TrackingFuelController {
            budget: 100,
            consumed: Arc::new(AtomicI64::new(0)),
        },
    );
    let overridden = with_fuel_controller(
        &parent,
        TrackingFuelController {
            budget: 2,
            consumed: Arc::new(AtomicI64::new(0)),
        },
    );

    let results = module
        .exported_function("inspect")
        .unwrap()
        .call_with_context(&overridden, &[])
        .unwrap();

    assert_eq!(vec![2, 1], results);
}

#[test]
fn fuel_exhaustion_after_host_debit_traps_call() {
    let runtime = Runtime::new();
    let module = runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |ctx, _module, _params| {
                assert_eq!(0, remaining_fuel(&ctx).unwrap());
                add_fuel(&ctx, -2).unwrap();
                Ok(vec![1])
            },
            &[],
            &[ValueType::I32],
        )
        .with_name("burn")
        .export("burn")
        .instantiate(&Context::default())
        .unwrap();

    let err = module
        .exported_function("burn")
        .unwrap()
        .call_with_context(
            &with_fuel_controller(
                &Context::default(),
                TrackingFuelController {
                    budget: 1,
                    consumed: Arc::new(AtomicI64::new(0)),
                },
            ),
            &[],
        )
        .unwrap_err();

    assert_eq!("fuel exhausted", err.to_string());
}

#[test]
fn interpreter_guest_host_callbacks_can_observe_runtime_fuel() {
    let runtime = Runtime::with_config(RuntimeConfig::new_interpreter().with_fuel(2));
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |ctx, _module, params| {
                assert_eq!(1, remaining_fuel(&ctx).unwrap());
                Ok(vec![params[0] + 1])
            },
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .export("inc")
        .instantiate(&Context::default())
        .unwrap();

    let guest = runtime
        .instantiate_binary(GUEST_IMPORT_INC_WASM, ModuleConfig::new())
        .unwrap();

    assert_eq!(
        vec![42],
        guest.exported_function("run").unwrap().call(&[41]).unwrap()
    );
}

#[test]
fn interpreter_guest_loops_trap_when_fuel_exhausts() {
    let runtime = Runtime::with_config(RuntimeConfig::new_interpreter().with_fuel(1));
    let observations = Arc::new(Mutex::new(Vec::new()));
    let guest = runtime
        .instantiate_binary(LOOP_EXPORT_WASM, ModuleConfig::new())
        .unwrap();
    let ctx = with_trap_observer(
        &Context::default(),
        record_trap_observations(observations.clone()),
    );

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&ctx, &[])
        .unwrap_err();

    assert_eq!("fuel exhausted", err.to_string());
    assert_eq!(Some(TrapCause::FuelExhausted), trap_cause_of(&err));
    assert_eq!(
        vec![(TrapCause::FuelExhausted, "fuel exhausted".to_string())],
        *observations.lock().expect("trap observations poisoned")
    );
}

#[test]
fn interpreter_oob_trap_notifies_observer() {
    let runtime = Runtime::with_config(RuntimeConfig::new_interpreter());
    let observations = Arc::new(Mutex::new(Vec::new()));
    let guest = runtime
        .instantiate_binary(OOB_LOAD_WASM, ModuleConfig::new())
        .unwrap();
    let ctx = with_trap_observer(
        &Context::default(),
        record_trap_observations(observations.clone()),
    );

    let err = guest
        .exported_function("oob")
        .unwrap()
        .call_with_context(&ctx, &[])
        .unwrap_err();

    assert_eq!(
        Some(TrapCause::OutOfBoundsMemoryAccess),
        trap_cause_of(&err)
    );
    assert_eq!(
        vec![(
            TrapCause::OutOfBoundsMemoryAccess,
            "out of bounds memory access".to_string()
        )],
        *observations.lock().expect("trap observations poisoned")
    );
}

#[test]
fn interpreter_divide_by_zero_trap_notifies_observer() {
    let runtime = Runtime::with_config(RuntimeConfig::new_interpreter());
    let observations = Arc::new(Mutex::new(Vec::new()));
    let guest = runtime
        .instantiate_binary(DIV_BY_ZERO_WASM, ModuleConfig::new())
        .unwrap();
    let ctx = with_trap_observer(
        &Context::default(),
        record_trap_observations(observations.clone()),
    );

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&ctx, &[])
        .unwrap_err();

    assert_eq!(Some(TrapCause::IntegerDivideByZero), trap_cause_of(&err));
    assert_eq!(
        vec![(
            TrapCause::IntegerDivideByZero,
            "integer divide by zero".to_string()
        )],
        *observations.lock().expect("trap observations poisoned")
    );
}

#[test]
fn interpreter_divide_overflow_trap_notifies_observer() {
    let runtime = Runtime::with_config(RuntimeConfig::new_interpreter());
    let observations = Arc::new(Mutex::new(Vec::new()));
    let guest = runtime
        .instantiate_binary(DIV_OVERFLOW_WASM, ModuleConfig::new())
        .unwrap();
    let ctx = with_trap_observer(
        &Context::default(),
        record_trap_observations(observations.clone()),
    );

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&ctx, &[])
        .unwrap_err();

    assert_eq!(Some(TrapCause::IntegerOverflow), trap_cause_of(&err));
    assert_eq!(
        vec![(TrapCause::IntegerOverflow, "integer overflow".to_string())],
        *observations.lock().expect("trap observations poisoned")
    );
}

#[test]
fn snapshot_restore_within_nested_invocation_overrides_result() {
    let runtime = Runtime::new();
    let sidechannel = Arc::new(AtomicI64::new(0));
    let snapshot = Arc::new(Mutex::new(None::<Snapshot>));

    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            {
                let sidechannel = sidechannel.clone();
                let snapshot = snapshot.clone();
                move |ctx, module, _params| {
                    *snapshot.lock().expect("snapshot poisoned") = Some(
                        get_snapshotter(&ctx)
                            .expect("snapshotter should be injected")
                            .snapshot(),
                    );
                    let restored = module
                        .exported_function("restore")
                        .expect("restore export should exist")
                        .call_with_context(&ctx, &[])?;
                    assert!(restored.is_empty());
                    sidechannel.store(10, Ordering::SeqCst);
                    Ok(vec![2])
                }
            },
            &[],
            &[ValueType::I32],
        )
        .with_name("snapshot")
        .export("snapshot")
        .new_function_builder()
        .with_func(
            {
                let snapshot = snapshot.clone();
                move |_ctx, _module, _params| {
                    snapshot
                        .lock()
                        .expect("snapshot poisoned")
                        .as_ref()
                        .expect("snapshot should be present")
                        .restore(&[12]);
                    Ok(Vec::new())
                }
            },
            &[],
            &[],
        )
        .with_name("restore")
        .export("restore")
        .instantiate(&Context::default())
        .unwrap();

    let module = runtime
        .module("env")
        .expect("host module should be registered");
    let results = module
        .exported_function("snapshot")
        .expect("snapshot export should exist")
        .call_with_context(&with_snapshotter(&Context::default()), &[])
        .unwrap();

    assert_eq!(vec![12], results);
    assert_eq!(10, sidechannel.load(Ordering::SeqCst));
}

#[test]
fn snapshot_restore_from_later_invocation_panics() {
    let runtime = Runtime::new();
    let snapshots = Arc::new(Mutex::new(Vec::<Snapshot>::new()));

    runtime
        .new_host_module_builder("example")
        .new_function_builder()
        .with_func(
            {
                let snapshots = snapshots.clone();
                move |ctx, module, params| {
                    let snapshot = get_snapshotter(&ctx)
                        .expect("snapshotter should be injected")
                        .snapshot();
                    let mut snapshots = snapshots.lock().expect("snapshots poisoned");
                    let idx = snapshots.len() as u32;
                    snapshots.push(snapshot);
                    drop(snapshots);

                    assert!(module
                        .memory()
                        .expect("guest memory should be present")
                        .write_u32_le(params[0] as u32, idx));
                    Ok(vec![0])
                }
            },
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .with_name("snapshot")
        .export("snapshot")
        .new_function_builder()
        .with_func(
            {
                let snapshots = snapshots.clone();
                move |_ctx, module, params| {
                    let idx = module
                        .memory()
                        .expect("guest memory should be present")
                        .read_u32_le(params[0] as u32)
                        .expect("snapshot index should be written")
                        as usize;
                    snapshots.lock().expect("snapshots poisoned")[idx].restore(&[12]);
                    Ok(Vec::new())
                }
            },
            &[ValueType::I32],
            &[],
        )
        .with_name("restore")
        .export("restore")
        .instantiate(&Context::default())
        .unwrap();

    let guest = runtime
        .instantiate_binary(SNAPSHOT_WASM, ModuleConfig::new())
        .unwrap();
    let ctx = with_snapshotter(&Context::default());
    let results = guest
        .exported_function("snapshot")
        .unwrap()
        .call_with_context(&ctx, &[0])
        .unwrap();
    assert_eq!(vec![0], results);

    let panic = catch_unwind(AssertUnwindSafe(|| {
        let _ = guest
            .exported_function("restore")
            .unwrap()
            .call_with_context(&ctx, &[0]);
    }))
    .expect_err("expected stale snapshot restore to panic");
    let message = panic
        .downcast_ref::<&str>()
        .map(|value| value.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .expect("panic payload should be string");

    assert_eq!(
        "unhandled snapshot restore, this generally indicates restore was called from a different exported function invocation than snapshot",
        message
    );
}

#[test]
fn checkpoint_example_flow_restores_example_result() {
    let runtime = Runtime::new();
    let snapshots = Arc::new(Mutex::new(Vec::<Snapshot>::new()));

    runtime
        .new_host_module_builder("example")
        .new_function_builder()
        .with_func(
            {
                let snapshots = snapshots.clone();
                move |ctx, module, params| {
                    let snapshot = get_snapshotter(&ctx)
                        .expect("snapshotter should be injected")
                        .snapshot();
                    let mut snapshots = snapshots.lock().expect("snapshots poisoned");
                    let idx = snapshots.len() as u32;
                    snapshots.push(snapshot);
                    drop(snapshots);

                    assert!(module
                        .memory()
                        .expect("guest memory should be present")
                        .write_u32_le(params[0] as u32, idx));
                    Ok(vec![0])
                }
            },
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .with_name("snapshot")
        .export("snapshot")
        .new_function_builder()
        .with_func(
            {
                let snapshots = snapshots.clone();
                move |_ctx, module, params| {
                    let idx = module
                        .memory()
                        .expect("guest memory should be present")
                        .read_u32_le(params[0] as u32)
                        .expect("snapshot index should be written")
                        as usize;
                    snapshots.lock().expect("snapshots poisoned")[idx].restore(&[5]);
                    Ok(Vec::new())
                }
            },
            &[ValueType::I32],
            &[],
        )
        .with_name("restore")
        .export("restore")
        .instantiate(&Context::default())
        .unwrap();

    let guest = runtime
        .instantiate_binary(SNAPSHOT_WASM, ModuleConfig::new())
        .unwrap();
    let results = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&with_snapshotter(&Context::default()), &[])
        .unwrap();

    assert_eq!(vec![5], results);
    assert_eq!(Some(0), guest.memory().unwrap().read_u32_le(0));
    assert_eq!(1, snapshots.lock().expect("snapshots poisoned").len());
}

#[test]
fn import_resolver_resolves_runtime_imports_for_anonymous_modules() {
    for i in 0..5 {
        let runtime = Runtime::new();
        let call_count = Arc::new(AtomicU32::new(0));
        let compiled_host = runtime
            .new_host_module_builder(format!("env{i}"))
            .new_function_builder()
            .with_func(
                {
                    let call_count = call_count.clone();
                    move |_ctx, _module, _params| {
                        call_count.fetch_add(1, Ordering::SeqCst);
                        Ok(Vec::new())
                    }
                },
                &[],
                &[],
            )
            .with_name("start")
            .export("start")
            .compile(&Context::default())
            .unwrap();
        let instance_import = runtime
            .instantiate_with_context(
                &Context::default(),
                &compiled_host,
                ModuleConfig::new().with_name(""),
            )
            .unwrap();

        let resolver_ctx = with_import_resolver(&Context::default(), move |name| {
            (name == "env").then_some(instance_import.clone())
        });
        let resolver = get_import_resolver(&resolver_ctx).expect("resolver should be present");
        assert!(resolver("env").is_some());
        assert!(resolver("missing").is_none());

        let compiled_guest = runtime
            .compile(&[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
                0x02, 0x0d, 0x01, 0x03, b'e', b'n', b'v', 0x05, b's', b't', b'a', b'r', b't', 0x00,
                0x00, 0x03, 0x02, 0x01, 0x00, 0x08, 0x01, 0x01, 0x0a, 0x06, 0x01, 0x04, 0x00, 0x10,
                0x00, 0x0b,
            ])
            .unwrap();

        let _guest = runtime
            .instantiate_with_context(&resolver_ctx, &compiled_guest, ModuleConfig::new())
            .unwrap();
        assert_eq!(1, call_count.load(Ordering::SeqCst));
    }
}

struct AbortRecordingListener {
    events: Arc<Mutex<Vec<String>>>,
}

impl FunctionListener for AbortRecordingListener {
    fn before(
        &self,
        _ctx: &Context,
        _module: &Module,
        definition: &FunctionDefinition,
        _params: &[u64],
        _stack_iterator: &mut dyn razero::StackIterator,
    ) {
        self.events
            .lock()
            .expect("events poisoned")
            .push(format!("before:{}", definition.name()));
    }

    fn after(
        &self,
        _ctx: &Context,
        _module: &Module,
        definition: &FunctionDefinition,
        _results: &[u64],
    ) {
        self.events
            .lock()
            .expect("events poisoned")
            .push(format!("after:{}", definition.name()));
    }

    fn abort(
        &self,
        _ctx: &Context,
        _module: &Module,
        definition: &FunctionDefinition,
        error: &RuntimeError,
    ) {
        self.events.lock().expect("events poisoned").push(format!(
            "abort:{}:{}",
            definition.name(),
            error
        ));
    }
}

struct AbortRecordingFactory {
    events: Arc<Mutex<Vec<String>>>,
}

impl FunctionListenerFactory for AbortRecordingFactory {
    fn new_listener(&self, _definition: &FunctionDefinition) -> Option<Arc<dyn FunctionListener>> {
        Some(Arc::new(AbortRecordingListener {
            events: self.events.clone(),
        }))
    }
}

#[test]
fn listener_abort_runs_without_after_on_host_error() {
    let runtime = Runtime::new();
    let module = runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_callback(
            |_ctx, _module, _params| Err(RuntimeError::new("boom")),
            &[],
            &[],
        )
        .with_name("fail")
        .export("fail")
        .instantiate(&Context::default())
        .unwrap();

    let events = Arc::new(Mutex::new(Vec::new()));
    let call_ctx = with_function_listener_factory(
        &Context::default(),
        AbortRecordingFactory {
            events: events.clone(),
        },
    );

    let err = module
        .exported_function("fail")
        .unwrap()
        .call_with_context(&call_ctx, &[])
        .unwrap_err();

    assert_eq!("boom", err.to_string());
    assert_eq!(
        vec!["before:fail".to_string(), "abort:fail:boom".to_string()],
        *events.lock().expect("events poisoned")
    );
}
