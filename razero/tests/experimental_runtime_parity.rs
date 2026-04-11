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
    add_fuel, get_fuel_controller, get_import_resolver, get_snapshotter, get_time_provider,
    get_yielder, remaining_fuel, trap_cause_of, with_fuel_controller, with_fuel_observer,
    with_function_listener_factory, with_host_call_policy, with_host_call_policy_observer,
    with_import_resolver, with_import_resolver_config, with_import_resolver_observer,
    with_snapshotter, with_time_provider, with_trap_observer, with_yield_observer,
    with_yield_policy, with_yield_policy_observer, with_yielder, Context, CoreFeatures, FuelEvent,
    FuelObservation, FunctionDefinition, FunctionListener, FunctionListenerFactory,
    HostCallPolicyDecision, HostCallPolicyObservation, HostCallPolicyRequest, ImportACL,
    ImportResolverConfig, ImportResolverEvent, ImportResolverObservation, Module, ModuleConfig,
    Runtime, RuntimeConfig, RuntimeError, Snapshot, TimeProvider, TrapCause, ValueType, YieldError,
    YieldEvent, YieldObservation, YieldPolicyDecision, YieldPolicyObservation,
    CORE_FEATURES_THREADS, ERR_YIELDED,
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
const INVALID_CONVERSION_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, 0x03,
    0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x0a, 0x01, 0x08,
    0x00, 0x43, 0x00, 0x00, 0xc0, 0x7f, 0xa8, 0x0b,
];
const INVALID_TABLE_ACCESS_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02,
    0x01, 0x00, 0x04, 0x04, 0x01, 0x70, 0x00, 0x01, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00,
    0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x41, 0x01, 0x11, 0x00, 0x00, 0x0b,
];
const INDIRECT_CALL_TYPE_MISMATCH_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x60, 0x00, 0x01, 0x7f, 0x60,
    0x00, 0x00, 0x03, 0x03, 0x02, 0x01, 0x00, 0x04, 0x04, 0x01, 0x70, 0x00, 0x01, 0x07, 0x07, 0x01,
    0x03, b'r', b'u', b'n', 0x00, 0x01, 0x09, 0x07, 0x01, 0x00, 0x41, 0x00, 0x0b, 0x01, 0x00, 0x0a,
    0x0c, 0x02, 0x02, 0x00, 0x0b, 0x07, 0x00, 0x41, 0x00, 0x11, 0x00, 0x00, 0x0b,
];
const UNREACHABLE_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02,
    0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x05, 0x01, 0x03, 0x00,
    0x00, 0x0b,
];
const UNALIGNED_ATOMIC_STORE_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03, 0x02,
    0x01, 0x00, 0x05, 0x04, 0x01, 0x03, 0x01, 0x01, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00,
    0x00, 0x0a, 0x0c, 0x01, 0x0a, 0x00, 0x41, 0x01, 0x41, 0x2a, 0xfe, 0x17, 0x02, 0x00, 0x0b,
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

struct StubTimeProvider {
    wall: (i64, i32),
    nano: i64,
}

impl TimeProvider for StubTimeProvider {
    fn walltime(&self) -> (i64, i32) {
        self.wall
    }

    fn nanotime(&self) -> i64 {
        self.nano
    }

    fn nanosleep(&self, _ns: i64) {}
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

fn allow_all_host_calls(_ctx: &Context, _request: &HostCallPolicyRequest) -> bool {
    true
}

fn deny_hook_impl_host_call(_ctx: &Context, request: &HostCallPolicyRequest) -> bool {
    request.name() != Some("hook_impl")
}

fn deny_async_work_host_call(_ctx: &Context, request: &HostCallPolicyRequest) -> bool {
    request.name() != Some("async_work")
}

fn deny_inc_host_call(_ctx: &Context, request: &HostCallPolicyRequest) -> bool {
    request.name() != Some("inc")
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

fn record_yield_observations(
    observations: Arc<Mutex<Vec<(YieldEvent, u64, i32)>>>,
) -> impl Fn(&Context, YieldObservation) + Send + Sync + 'static {
    move |_ctx: &Context, observation: YieldObservation| {
        observations
            .lock()
            .expect("yield observations poisoned")
            .push((
                observation.event,
                observation.yield_count,
                observation.expected_host_results,
            ));
    }
}

fn record_host_call_policy_observations(
    observations: Arc<
        Mutex<
            Vec<(
                Option<String>,
                Option<String>,
                Option<String>,
                HostCallPolicyDecision,
            )>,
        >,
    >,
) -> impl Fn(&Context, HostCallPolicyObservation) + Send + Sync + 'static {
    move |_ctx: &Context, observation: HostCallPolicyObservation| {
        observations
            .lock()
            .expect("host call policy observations poisoned")
            .push((
                observation.module.name().map(str::to_string),
                observation.request.name().map(str::to_string),
                observation.request.caller_module_name().map(str::to_string),
                observation.decision,
            ));
    }
}

fn record_yield_policy_observations(
    observations: Arc<
        Mutex<
            Vec<(
                Option<String>,
                Option<String>,
                Option<String>,
                YieldPolicyDecision,
            )>,
        >,
    >,
) -> impl Fn(&Context, YieldPolicyObservation) + Send + Sync + 'static {
    move |_ctx: &Context, observation: YieldPolicyObservation| {
        observations
            .lock()
            .expect("yield policy observations poisoned")
            .push((
                observation.module.name().map(str::to_string),
                observation.request.name().map(str::to_string),
                observation.request.caller_module_name().map(str::to_string),
                observation.decision,
            ));
    }
}

fn record_fuel_observations(
    observations: Arc<Mutex<Vec<FuelEvent>>>,
) -> impl Fn(&Context, FuelObservation) + Send + Sync + 'static {
    move |_ctx: &Context, observation: FuelObservation| {
        observations
            .lock()
            .expect("fuel observations poisoned")
            .push(observation.event);
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
        vec![(
            TrapCause::PolicyDenied,
            "policy denied: cooperative yield".to_string()
        )],
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
fn trap_observer_resume_context_receives_follow_on_policy_denial() {
    let (_runtime, guest) = setup_yield_runtime();
    let initial_observations = Arc::new(Mutex::new(Vec::new()));
    let resumed_observations = Arc::new(Mutex::new(Vec::new()));
    let base = Context::default();
    let yielding = with_yielder(&base);
    let initial_ctx = with_trap_observer(
        &yielding,
        record_trap_observations(initial_observations.clone()),
    );

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");
    assert!(initial_observations
        .lock()
        .expect("initial trap observations poisoned")
        .is_empty());

    let resumed_base = Context::default();
    let resumed_yielding = with_yielder(&resumed_base);
    let resumed_policy = with_yield_policy(&resumed_yielding, deny_all_yields);
    let resumed_ctx = with_trap_observer(
        &resumed_policy,
        record_trap_observations(resumed_observations.clone()),
    );
    let err = resumer.resume(&resumed_ctx, &[40]).unwrap_err();

    assert_eq!("policy denied: cooperative yield", err.to_string());
    assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
    assert!(initial_observations
        .lock()
        .expect("initial trap observations poisoned")
        .is_empty());
    assert_eq!(
        vec![(
            TrapCause::PolicyDenied,
            "policy denied: cooperative yield".to_string()
        )],
        *resumed_observations
            .lock()
            .expect("resumed trap observations poisoned")
    );
}

#[test]
fn trap_observer_initial_context_does_not_persist_when_resume_omits_observer() {
    let (_runtime, guest) = setup_yield_runtime();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let initial_ctx = with_trap_observer(
        &with_yielder(&Context::default()),
        record_trap_observations(observations.clone()),
    );

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");
    assert!(observations
        .lock()
        .expect("trap observations poisoned")
        .is_empty());

    let err = resumer
        .resume(
            &with_yield_policy(&with_yielder(&Context::default()), deny_all_yields),
            &[40],
        )
        .unwrap_err();

    assert_eq!("policy denied: cooperative yield", err.to_string());
    assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
    assert!(observations
        .lock()
        .expect("trap observations poisoned")
        .is_empty());
}

#[test]
fn yield_policy_observer_resume_context_receives_follow_on_denial() {
    let (_runtime, guest) = setup_yield_runtime();
    let initial_observations = Arc::new(Mutex::new(Vec::new()));
    let resumed_observations = Arc::new(Mutex::new(Vec::new()));
    let initial_ctx = with_yield_policy_observer(
        &with_yielder(&Context::default()),
        record_yield_policy_observations(initial_observations.clone()),
    );

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");
    assert!(initial_observations
        .lock()
        .expect("initial yield policy observations poisoned")
        .is_empty());

    let resumed_ctx = with_yield_policy_observer(
        &with_yield_policy(&with_yielder(&Context::default()), deny_all_yields),
        record_yield_policy_observations(resumed_observations.clone()),
    );
    let err = resumer.resume(&resumed_ctx, &[40]).unwrap_err();

    assert_eq!("policy denied: cooperative yield", err.to_string());
    assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
    assert!(initial_observations
        .lock()
        .expect("initial yield policy observations poisoned")
        .is_empty());
    assert_eq!(
        vec![(
            None,
            Some("run_twice".to_string()),
            None,
            YieldPolicyDecision::Denied,
        )],
        *resumed_observations
            .lock()
            .expect("resumed yield policy observations poisoned")
    );
}

#[test]
fn yield_policy_observer_initial_context_does_not_persist_when_resume_omits_observer() {
    let (_runtime, guest) = setup_yield_runtime();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let initial_ctx = with_yield_policy_observer(
        &with_yield_policy(&with_yielder(&Context::default()), allow_all_yields),
        record_yield_policy_observations(observations.clone()),
    );

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");
    assert_eq!(
        vec![(
            None,
            Some("run_twice".to_string()),
            None,
            YieldPolicyDecision::Allowed,
        )],
        *observations
            .lock()
            .expect("yield policy observations poisoned")
    );

    let err = resumer
        .resume(
            &with_yield_policy(&with_yielder(&Context::default()), deny_all_yields),
            &[40],
        )
        .unwrap_err();

    assert_eq!("policy denied: cooperative yield", err.to_string());
    assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
    assert_eq!(
        vec![(
            None,
            Some("run_twice".to_string()),
            None,
            YieldPolicyDecision::Allowed,
        )],
        *observations
            .lock()
            .expect("yield policy observations poisoned")
    );
}

#[test]
fn host_call_policy_observer_reports_direct_host_export_denial_before_trap() {
    let runtime = Runtime::new();
    let host_call_observations = Arc::new(Mutex::new(Vec::new()));
    let trap_observations = Arc::new(Mutex::new(Vec::new()));
    let module = runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_ctx, _module, params| Ok(vec![params[0]]),
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .with_name("hook_impl")
        .export("hook")
        .instantiate(&Context::default())
        .unwrap();

    let ctx = with_trap_observer(
        &with_host_call_policy_observer(
            &with_host_call_policy(&Context::default(), deny_hook_impl_host_call),
            record_host_call_policy_observations(host_call_observations.clone()),
        ),
        record_trap_observations(trap_observations.clone()),
    );

    let err = module
        .exported_function("hook")
        .unwrap()
        .call_with_context(&ctx, &[1])
        .unwrap_err();

    assert_eq!("policy denied: host call", err.to_string());
    assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
    assert_eq!(
        vec![(
            Some("env".to_string()),
            Some("hook_impl".to_string()),
            Some("env".to_string()),
            HostCallPolicyDecision::Denied,
        )],
        *host_call_observations
            .lock()
            .expect("host call policy observations poisoned")
    );
    assert_eq!(
        vec![(
            TrapCause::PolicyDenied,
            "policy denied: host call".to_string()
        )],
        *trap_observations
            .lock()
            .expect("trap observations poisoned")
    );
}

#[test]
fn host_call_policy_observer_reports_allowed_guest_import_metadata() {
    let runtime = Runtime::new();
    let observations = Arc::new(Mutex::new(Vec::new()));
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_ctx, _module, params| Ok(vec![params[0] + 1]),
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .with_name("inc")
        .export("inc")
        .instantiate(&Context::default())
        .unwrap();
    let guest = runtime
        .instantiate_binary(
            GUEST_IMPORT_INC_WASM,
            ModuleConfig::new().with_name("guest"),
        )
        .unwrap();
    let ctx = with_host_call_policy_observer(
        &with_host_call_policy(&Context::default(), allow_all_host_calls),
        record_host_call_policy_observations(observations.clone()),
    );

    assert_eq!(
        vec![42],
        guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&ctx, &[41])
            .unwrap()
    );
    assert_eq!(
        vec![(
            Some("guest".to_string()),
            Some("inc".to_string()),
            Some("guest".to_string()),
            HostCallPolicyDecision::Allowed,
        )],
        *observations
            .lock()
            .expect("host call policy observations poisoned")
    );
}

#[test]
fn host_call_policy_resume_context_controls_follow_on_host_call() {
    let (_runtime, guest) = setup_yield_runtime();
    let initial_observations = Arc::new(Mutex::new(Vec::new()));
    let resumed_observations = Arc::new(Mutex::new(Vec::new()));
    let initial_ctx = with_host_call_policy_observer(
        &with_host_call_policy(&with_yielder(&Context::default()), allow_all_host_calls),
        record_host_call_policy_observations(initial_observations.clone()),
    );

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");
    assert_eq!(
        vec![(
            None,
            Some("async_work".to_string()),
            None,
            HostCallPolicyDecision::Allowed,
        )],
        *initial_observations
            .lock()
            .expect("initial host call observations poisoned")
    );

    let resumed_ctx = with_host_call_policy_observer(
        &with_host_call_policy(
            &with_yielder(&Context::default()),
            deny_async_work_host_call,
        ),
        record_host_call_policy_observations(resumed_observations.clone()),
    );
    let err = resumer.resume(&resumed_ctx, &[40]).unwrap_err();

    assert_eq!("policy denied: host call", err.to_string());
    assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
    assert_eq!(
        vec![(
            None,
            Some("async_work".to_string()),
            None,
            HostCallPolicyDecision::Allowed,
        )],
        *initial_observations
            .lock()
            .expect("initial host call observations poisoned")
    );
    assert_eq!(
        vec![(
            None,
            Some("async_work".to_string()),
            None,
            HostCallPolicyDecision::Denied,
        )],
        *resumed_observations
            .lock()
            .expect("resumed host call observations poisoned")
    );
}

#[test]
fn host_call_policy_initial_observer_does_not_persist_when_resume_omits_one() {
    let (_runtime, guest) = setup_yield_runtime();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let initial_ctx = with_host_call_policy_observer(
        &with_host_call_policy(&with_yielder(&Context::default()), allow_all_host_calls),
        record_host_call_policy_observations(observations.clone()),
    );

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");

    let err = resumer
        .resume(
            &with_host_call_policy(
                &with_yielder(&Context::default()),
                deny_async_work_host_call,
            ),
            &[40],
        )
        .unwrap_err();

    assert_eq!("policy denied: host call", err.to_string());
    assert_eq!(Some(TrapCause::PolicyDenied), trap_cause_of(&err));
    assert_eq!(
        vec![(
            None,
            Some("async_work".to_string()),
            None,
            HostCallPolicyDecision::Allowed,
        )],
        *observations
            .lock()
            .expect("host call observations poisoned")
    );
}

#[test]
fn time_provider_resume_context_overrides_follow_on_host_call() {
    let runtime = Runtime::new();
    let observations = Arc::new(Mutex::new(Vec::new()));
    runtime
        .new_host_module_builder("example")
        .new_function_builder()
        .with_func(
            {
                let observations = observations.clone();
                move |ctx, _module, _params| {
                    let observed = get_time_provider(&ctx).map(|provider| {
                        let (sec, nsec) = provider.walltime();
                        (sec, nsec, provider.nanotime())
                    });
                    observations
                        .lock()
                        .expect("time provider observations poisoned")
                        .push(observed);
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(vec![0])
                }
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

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(
            &with_time_provider(
                &with_yielder(&Context::default()),
                StubTimeProvider {
                    wall: (1, 2),
                    nano: 3,
                },
            ),
            &[],
        )
        .unwrap_err();
    let first_resumer = yielded(err).resumer().expect("resumer should be present");
    assert_eq!(
        vec![Some((1, 2, 3))],
        *observations
            .lock()
            .expect("time provider observations poisoned")
    );

    let err = first_resumer
        .resume(
            &with_time_provider(
                &with_yielder(&Context::default()),
                StubTimeProvider {
                    wall: (4, 5),
                    nano: 6,
                },
            ),
            &[40],
        )
        .unwrap_err();
    let second_resumer = yielded(err).resumer().expect("resumer should be present");

    assert_eq!(
        vec![Some((1, 2, 3)), Some((4, 5, 6))],
        *observations
            .lock()
            .expect("time provider observations poisoned")
    );
    assert_eq!(
        vec![42],
        second_resumer
            .resume(&with_yielder(&Context::default()), &[2])
            .unwrap()
    );
}

#[test]
fn time_provider_initial_context_does_not_persist_when_resume_omits_one() {
    let runtime = Runtime::new();
    let observations = Arc::new(Mutex::new(Vec::new()));
    runtime
        .new_host_module_builder("example")
        .new_function_builder()
        .with_func(
            {
                let observations = observations.clone();
                move |ctx, _module, _params| {
                    let observed = get_time_provider(&ctx).map(|provider| {
                        let (sec, nsec) = provider.walltime();
                        (sec, nsec, provider.nanotime())
                    });
                    observations
                        .lock()
                        .expect("time provider observations poisoned")
                        .push(observed);
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(vec![0])
                }
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

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(
            &with_time_provider(
                &with_yielder(&Context::default()),
                StubTimeProvider {
                    wall: (1, 2),
                    nano: 3,
                },
            ),
            &[],
        )
        .unwrap_err();
    let first_resumer = yielded(err).resumer().expect("resumer should be present");

    let err = first_resumer
        .resume(&with_yielder(&Context::default()), &[40])
        .unwrap_err();
    let second_resumer = yielded(err).resumer().expect("resumer should be present");

    assert_eq!(
        vec![Some((1, 2, 3)), None],
        *observations
            .lock()
            .expect("time provider observations poisoned")
    );
    assert_eq!(
        vec![42],
        second_resumer
            .resume(&with_yielder(&Context::default()), &[2])
            .unwrap()
    );
}

#[test]
fn snapshotter_is_not_reinjected_on_follow_on_resumed_host_call() {
    let runtime = Runtime::new();
    let observations = Arc::new(Mutex::new(Vec::new()));
    runtime
        .new_host_module_builder("example")
        .new_function_builder()
        .with_func(
            {
                let observations = observations.clone();
                move |ctx, _module, _params| {
                    observations
                        .lock()
                        .expect("snapshotter observations poisoned")
                        .push(get_snapshotter(&ctx).is_some());
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(vec![0])
                }
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

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&with_snapshotter(&with_yielder(&Context::default())), &[])
        .unwrap_err();
    let first_resumer = yielded(err).resumer().expect("resumer should be present");
    assert_eq!(
        vec![true],
        *observations
            .lock()
            .expect("snapshotter observations poisoned")
    );

    let err = first_resumer
        .resume(&with_snapshotter(&with_yielder(&Context::default())), &[40])
        .unwrap_err();
    let second_resumer = yielded(err).resumer().expect("resumer should be present");

    assert_eq!(
        vec![true, false],
        *observations
            .lock()
            .expect("snapshotter observations poisoned")
    );
    assert_eq!(
        vec![42],
        second_resumer
            .resume(&with_yielder(&Context::default()), &[2])
            .unwrap()
    );
}

#[test]
fn snapshotter_initial_context_does_not_persist_when_resume_omits_one() {
    let runtime = Runtime::new();
    let observations = Arc::new(Mutex::new(Vec::new()));
    runtime
        .new_host_module_builder("example")
        .new_function_builder()
        .with_func(
            {
                let observations = observations.clone();
                move |ctx, _module, _params| {
                    observations
                        .lock()
                        .expect("snapshotter observations poisoned")
                        .push(get_snapshotter(&ctx).is_some());
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(vec![0])
                }
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

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&with_snapshotter(&with_yielder(&Context::default())), &[])
        .unwrap_err();
    let first_resumer = yielded(err).resumer().expect("resumer should be present");

    let err = first_resumer
        .resume(&with_yielder(&Context::default()), &[40])
        .unwrap_err();
    let second_resumer = yielded(err).resumer().expect("resumer should be present");

    assert_eq!(
        vec![true, false],
        *observations
            .lock()
            .expect("snapshotter observations poisoned")
    );
    assert_eq!(
        vec![42],
        second_resumer
            .resume(&with_yielder(&Context::default()), &[2])
            .unwrap()
    );
}

#[test]
fn fuel_controller_resume_context_overrides_follow_on_host_call() {
    let runtime = Runtime::new();
    let seen_budgets = Arc::new(Mutex::new(Vec::new()));
    runtime
        .new_host_module_builder("example")
        .new_function_builder()
        .with_func(
            {
                let seen_budgets = seen_budgets.clone();
                move |ctx, _module, _params| {
                    let controller =
                        get_fuel_controller(&ctx).expect("fuel controller should exist");
                    seen_budgets
                        .lock()
                        .expect("seen budgets poisoned")
                        .push(controller.budget());
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(vec![0])
                }
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
    let initial_consumed = Arc::new(AtomicI64::new(0));
    let resume_consumed = Arc::new(AtomicI64::new(0));

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(
            &with_fuel_controller(
                &with_yielder(&Context::default()),
                TrackingFuelController {
                    budget: 10,
                    consumed: initial_consumed.clone(),
                },
            ),
            &[],
        )
        .unwrap_err();
    let first_resumer = yielded(err).resumer().expect("resumer should be present");

    let err = first_resumer
        .resume(
            &with_fuel_controller(
                &with_yielder(&Context::default()),
                TrackingFuelController {
                    budget: 1,
                    consumed: resume_consumed.clone(),
                },
            ),
            &[40],
        )
        .unwrap_err();
    let second_resumer = yielded(err).resumer().expect("resumer should be present");

    assert_eq!(
        vec![42],
        second_resumer
            .resume(&with_yielder(&Context::default()), &[2])
            .unwrap()
    );
    assert_eq!(
        vec![10, 1],
        *seen_budgets.lock().expect("seen budgets poisoned")
    );
    assert!(initial_consumed.load(Ordering::SeqCst) > 0);
    assert_eq!(0, resume_consumed.load(Ordering::SeqCst));
}

#[test]
fn fuel_controller_initial_context_does_not_persist_when_resume_omits_one() {
    let runtime = Runtime::new();
    let seen_budgets = Arc::new(Mutex::new(Vec::new()));
    runtime
        .new_host_module_builder("example")
        .new_function_builder()
        .with_func(
            {
                let seen_budgets = seen_budgets.clone();
                move |ctx, _module, _params| {
                    let budget = get_fuel_controller(&ctx).map(|controller| controller.budget());
                    seen_budgets
                        .lock()
                        .expect("seen budgets poisoned")
                        .push(budget);
                    get_yielder(&ctx)
                        .expect("yielder should be injected")
                        .r#yield();
                    Ok(vec![0])
                }
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
    let consumed = Arc::new(AtomicI64::new(0));

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(
            &with_fuel_controller(
                &with_yielder(&Context::default()),
                TrackingFuelController {
                    budget: 10,
                    consumed: consumed.clone(),
                },
            ),
            &[],
        )
        .unwrap_err();
    let first_resumer = yielded(err).resumer().expect("resumer should be present");

    let err = first_resumer
        .resume(&with_yielder(&Context::default()), &[40])
        .unwrap_err();
    let second_resumer = yielded(err).resumer().expect("resumer should be present");

    assert_eq!(
        vec![42],
        second_resumer
            .resume(&with_yielder(&Context::default()), &[2])
            .unwrap()
    );
    assert_eq!(
        vec![Some(10), None],
        *seen_budgets.lock().expect("seen budgets poisoned")
    );
    assert!(consumed.load(Ordering::SeqCst) > 0);
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
fn yield_observer_tracks_yield_and_resume_events_when_installed_on_both_contexts() {
    let (_runtime, guest) = setup_yield_runtime();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let initial_ctx = with_yield_observer(
        &with_yielder(&Context::default()),
        record_yield_observations(observations.clone()),
    );

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
        .unwrap_err();
    let resumed_ctx = with_yield_observer(
        &with_yielder(&Context::default()),
        record_yield_observations(observations.clone()),
    );
    let results = yielded(err)
        .resumer()
        .expect("resumer should be present")
        .resume(&resumed_ctx, &[42])
        .unwrap();

    assert_eq!(vec![142], results);
    assert_eq!(
        vec![(YieldEvent::Yielded, 1, 1), (YieldEvent::Resumed, 1, 1)],
        *observations.lock().expect("yield observations poisoned")
    );
}

#[test]
fn yield_observer_resume_context_receives_subsequent_resume_events() {
    let (_runtime, guest) = setup_yield_runtime();
    let initial_observations = Arc::new(Mutex::new(Vec::new()));
    let resumed_observations = Arc::new(Mutex::new(Vec::new()));
    let initial_ctx = with_yield_observer(
        &with_yielder(&Context::default()),
        record_yield_observations(initial_observations.clone()),
    );

    let err = guest
        .exported_function("run_twice")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
        .unwrap_err();
    let first_resumer = yielded(err).resumer().expect("resumer should be present");
    let resumed_ctx = with_yield_observer(
        &with_yielder(&Context::default()),
        record_yield_observations(resumed_observations.clone()),
    );

    let err = first_resumer.resume(&resumed_ctx, &[40]).unwrap_err();
    let second_resumer = yielded(err).resumer().expect("resumer should be present");
    let final_ctx = with_yield_observer(
        &with_yielder(&Context::default()),
        record_yield_observations(resumed_observations.clone()),
    );
    let results = second_resumer.resume(&final_ctx, &[2]).unwrap();

    assert_eq!(vec![42], results);
    assert_eq!(
        vec![(YieldEvent::Yielded, 1, 1)],
        *initial_observations
            .lock()
            .expect("initial yield observations poisoned")
    );
    assert_eq!(
        vec![(YieldEvent::Resumed, 1, 1), (YieldEvent::Resumed, 1, 1)],
        *resumed_observations
            .lock()
            .expect("resumed yield observations poisoned")
    );
}

#[test]
fn yield_observer_does_not_persist_when_resume_context_omits_observer() {
    let (_runtime, guest) = setup_yield_runtime();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let initial_ctx = with_yield_observer(
        &with_yielder(&Context::default()),
        record_yield_observations(observations.clone()),
    );

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
        .unwrap_err();
    let results = yielded(err)
        .resumer()
        .expect("resumer should be present")
        .resume(&with_yielder(&Context::default()), &[42])
        .unwrap();

    assert_eq!(vec![142], results);
    assert_eq!(
        vec![(YieldEvent::Yielded, 1, 1)],
        *observations.lock().expect("yield observations poisoned")
    );
}

#[test]
fn yield_cancel_does_not_emit_additional_yield_observer_event() {
    let (_runtime, guest) = setup_yield_runtime();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let ctx = with_yield_observer(
        &with_yielder(&Context::default()),
        record_yield_observations(observations.clone()),
    );

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&ctx, &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");
    resumer.cancel();

    assert_eq!(
        vec![(YieldEvent::Yielded, 1, 1)],
        *observations.lock().expect("yield observations poisoned")
    );
}

#[test]
fn yield_resume_validation_error_does_not_emit_resumed_event() {
    let (_runtime, guest) = setup_yield_runtime();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let initial_ctx = with_yield_observer(
        &with_yielder(&Context::default()),
        record_yield_observations(observations.clone()),
    );

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
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
        vec![(YieldEvent::Yielded, 1, 1)],
        *observations.lock().expect("yield observations poisoned")
    );
    assert_eq!(
        vec![142],
        resumer
            .resume(&with_yielder(&Context::default()), &[42])
            .unwrap()
    );
    assert_eq!(
        vec![(YieldEvent::Yielded, 1, 1)],
        *observations.lock().expect("yield observations poisoned")
    );
}

#[test]
fn fuel_observer_yielded_call_finishes_observation_before_resume() {
    let (_runtime, guest) = setup_yield_runtime();
    let initial_observations = Arc::new(Mutex::new(Vec::new()));
    let resume_observations = Arc::new(Mutex::new(Vec::new()));
    let initial_ctx = with_yielder(&with_fuel_observer(
        &with_fuel_controller(
            &Context::default(),
            TrackingFuelController {
                budget: 64,
                consumed: Arc::new(AtomicI64::new(0)),
            },
        ),
        record_fuel_observations(initial_observations.clone()),
    ));

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");

    assert_eq!(
        vec![FuelEvent::Budgeted, FuelEvent::Consumed],
        *initial_observations
            .lock()
            .expect("initial fuel observations poisoned")
    );

    let resume_ctx = with_yielder(&with_fuel_observer(
        &Context::default(),
        record_fuel_observations(resume_observations.clone()),
    ));
    assert_eq!(vec![142], resumer.resume(&resume_ctx, &[42]).unwrap());

    assert_eq!(
        vec![FuelEvent::Budgeted, FuelEvent::Consumed],
        *initial_observations
            .lock()
            .expect("initial fuel observations poisoned")
    );
    assert_eq!(
        Vec::<FuelEvent>::new(),
        *resume_observations
            .lock()
            .expect("resume fuel observations poisoned")
    );
}

#[test]
fn fuel_observer_resume_without_new_observer_emits_no_additional_callbacks() {
    let (_runtime, guest) = setup_yield_runtime();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let ctx = with_yielder(&with_fuel_observer(
        &with_fuel_controller(
            &Context::default(),
            TrackingFuelController {
                budget: 64,
                consumed: Arc::new(AtomicI64::new(0)),
            },
        ),
        record_fuel_observations(observations.clone()),
    ));

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&ctx, &[])
        .unwrap_err();
    let resumer = yielded(err).resumer().expect("resumer should be present");

    assert_eq!(
        vec![142],
        resumer
            .resume(&with_yielder(&Context::default()), &[42])
            .unwrap()
    );
    assert_eq!(
        vec![FuelEvent::Budgeted, FuelEvent::Consumed],
        *observations.lock().expect("fuel observations poisoned")
    );
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
fn interpreter_invalid_conversion_trap_notifies_observer() {
    let runtime = Runtime::with_config(RuntimeConfig::new_interpreter());
    let observations = Arc::new(Mutex::new(Vec::new()));
    let guest = runtime
        .instantiate_binary(INVALID_CONVERSION_WASM, ModuleConfig::new())
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

    assert_eq!(
        Some(TrapCause::InvalidConversionToInteger),
        trap_cause_of(&err)
    );
    assert_eq!(
        vec![(
            TrapCause::InvalidConversionToInteger,
            "invalid conversion to integer".to_string()
        )],
        *observations.lock().expect("trap observations poisoned")
    );
}

#[test]
fn interpreter_invalid_table_access_trap_notifies_observer() {
    let runtime = Runtime::with_config(RuntimeConfig::new_interpreter());
    let observations = Arc::new(Mutex::new(Vec::new()));
    let guest = runtime
        .instantiate_binary(INVALID_TABLE_ACCESS_WASM, ModuleConfig::new())
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

    assert_eq!(Some(TrapCause::InvalidTableAccess), trap_cause_of(&err));
    assert_eq!(
        vec![(
            TrapCause::InvalidTableAccess,
            "invalid table access".to_string()
        )],
        *observations.lock().expect("trap observations poisoned")
    );
}

#[test]
fn interpreter_indirect_call_type_mismatch_trap_notifies_observer() {
    let runtime = Runtime::with_config(RuntimeConfig::new_interpreter());
    let observations = Arc::new(Mutex::new(Vec::new()));
    let guest = runtime
        .instantiate_binary(INDIRECT_CALL_TYPE_MISMATCH_WASM, ModuleConfig::new())
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

    assert_eq!(
        Some(TrapCause::IndirectCallTypeMismatch),
        trap_cause_of(&err)
    );
    assert_eq!(
        vec![(
            TrapCause::IndirectCallTypeMismatch,
            "indirect call type mismatch".to_string()
        )],
        *observations.lock().expect("trap observations poisoned")
    );
}

#[test]
fn interpreter_unreachable_trap_notifies_observer() {
    let runtime = Runtime::with_config(RuntimeConfig::new_interpreter());
    let observations = Arc::new(Mutex::new(Vec::new()));
    let guest = runtime
        .instantiate_binary(UNREACHABLE_WASM, ModuleConfig::new())
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

    assert_eq!(Some(TrapCause::Unreachable), trap_cause_of(&err));
    assert_eq!(
        vec![(TrapCause::Unreachable, "unreachable".to_string())],
        *observations.lock().expect("trap observations poisoned")
    );
}

#[test]
fn interpreter_unaligned_atomic_trap_notifies_observer() {
    let runtime = Runtime::with_config(
        RuntimeConfig::new_interpreter()
            .with_core_features(CoreFeatures::V2 | CORE_FEATURES_THREADS),
    );
    let observations = Arc::new(Mutex::new(Vec::new()));
    let guest = runtime
        .instantiate_binary(UNALIGNED_ATOMIC_STORE_WASM, ModuleConfig::new())
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

    assert_eq!(Some(TrapCause::UnalignedAtomic), trap_cause_of(&err));
    assert_eq!(
        vec![(TrapCause::UnalignedAtomic, "unaligned atomic".to_string())],
        *observations.lock().expect("trap observations poisoned")
    );
}

#[test]
fn compiled_oob_trap_notifies_observer() {
    if !razero_platform::compiler_supported()
        || !cfg!(target_os = "linux")
        || !cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
    {
        return;
    }

    let runtime = Runtime::with_config(RuntimeConfig::new_compiler().with_secure_mode(true));
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

    assert_eq!(Some(TrapCause::MemoryFault), trap_cause_of(&err));
    assert_eq!(
        vec![(TrapCause::MemoryFault, "memory fault".to_string())],
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportObserverRecord {
    import_module: String,
    event: ImportResolverEvent,
    resolved: bool,
}

#[test]
fn import_resolver_observer_reports_acl_allow_then_store_fallback() {
    let runtime = Runtime::new();
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_ctx, _module, params| Ok(vec![params[0] + 1]),
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .with_name("inc")
        .export("inc")
        .instantiate(&Context::default())
        .unwrap();

    let events = Arc::new(Mutex::new(Vec::new()));
    let ctx = with_import_resolver_observer(
        &with_import_resolver_config(
            &Context::default(),
            ImportResolverConfig {
                acl: Some(ImportACL::new().allow_modules(["env"])),
                ..ImportResolverConfig::default()
            },
        ),
        {
            let events = events.clone();
            move |_ctx: &Context, observation: ImportResolverObservation| {
                events
                    .lock()
                    .expect("import observer events poisoned")
                    .push(ImportObserverRecord {
                        import_module: observation.import_module,
                        event: observation.event,
                        resolved: observation.resolved_module.is_some(),
                    });
            }
        },
    );

    let guest = runtime
        .instantiate_with_context(
            &ctx,
            &runtime.compile(GUEST_IMPORT_INC_WASM).unwrap(),
            ModuleConfig::new(),
        )
        .unwrap();
    assert_eq!(
        vec![42],
        guest.exported_function("run").unwrap().call(&[41]).unwrap()
    );
    assert_eq!(
        vec![
            ImportObserverRecord {
                import_module: "env".to_string(),
                event: ImportResolverEvent::AclAllowed,
                resolved: false,
            },
            ImportObserverRecord {
                import_module: "env".to_string(),
                event: ImportResolverEvent::StoreFallback,
                resolved: false,
            },
        ],
        *events.lock().expect("import observer events poisoned")
    );
}

#[test]
fn import_resolver_observer_reports_acl_denied() {
    let runtime = Runtime::new();
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_ctx, _module, params| Ok(vec![params[0] + 1]),
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .with_name("inc")
        .export("inc")
        .instantiate(&Context::default())
        .unwrap();

    let events = Arc::new(Mutex::new(Vec::new()));
    let ctx = with_import_resolver_observer(
        &with_import_resolver_config(
            &Context::default(),
            ImportResolverConfig {
                acl: Some(ImportACL::new().deny_modules(["env"])),
                ..ImportResolverConfig::default()
            },
        ),
        {
            let events = events.clone();
            move |_ctx: &Context, observation: ImportResolverObservation| {
                events
                    .lock()
                    .expect("import observer events poisoned")
                    .push(ImportObserverRecord {
                        import_module: observation.import_module,
                        event: observation.event,
                        resolved: observation.resolved_module.is_some(),
                    });
            }
        },
    );

    let err = runtime
        .instantiate_with_context(
            &ctx,
            &runtime.compile(GUEST_IMPORT_INC_WASM).unwrap(),
            ModuleConfig::new(),
        )
        .err()
        .expect("ACL-denied import instantiation should fail");
    assert!(err.to_string().contains("denied by import ACL"));
    assert_eq!(
        vec![ImportObserverRecord {
            import_module: "env".to_string(),
            event: ImportResolverEvent::AclDenied,
            resolved: false,
        }],
        *events.lock().expect("import observer events poisoned")
    );
}

#[test]
fn import_resolver_observer_reports_resolver_attempt_before_resolution() {
    let runtime = Runtime::new();
    let compiled_host = runtime
        .new_host_module_builder("env0")
        .new_function_builder()
        .with_func(
            |_ctx, _module, params| Ok(vec![params[0] + 1]),
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .with_name("inc")
        .export("inc")
        .compile(&Context::default())
        .unwrap();
    let anonymous_import = runtime
        .instantiate_with_context(
            &Context::default(),
            &compiled_host,
            ModuleConfig::new().with_name(""),
        )
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let ctx = with_import_resolver_observer(
        &with_import_resolver_config(
            &Context::default(),
            ImportResolverConfig {
                resolver: Some(Arc::new(move |name| {
                    (name == "env").then_some(anonymous_import.clone())
                })),
                ..ImportResolverConfig::default()
            },
        ),
        {
            let events = events.clone();
            move |_ctx: &Context, observation: ImportResolverObservation| {
                events
                    .lock()
                    .expect("import observer events poisoned")
                    .push(ImportObserverRecord {
                        import_module: observation.import_module,
                        event: observation.event,
                        resolved: observation.resolved_module.is_some(),
                    });
            }
        },
    );

    let guest = runtime
        .instantiate_with_context(
            &ctx,
            &runtime.compile(GUEST_IMPORT_INC_WASM).unwrap(),
            ModuleConfig::new(),
        )
        .unwrap();
    assert_eq!(
        vec![42],
        guest.exported_function("run").unwrap().call(&[41]).unwrap()
    );
    assert_eq!(
        vec![
            ImportObserverRecord {
                import_module: "env".to_string(),
                event: ImportResolverEvent::ResolverAttempted,
                resolved: false,
            },
            ImportObserverRecord {
                import_module: "env".to_string(),
                event: ImportResolverEvent::ResolverResolved,
                resolved: true,
            },
        ],
        *events.lock().expect("import observer events poisoned")
    );
}

#[test]
fn import_resolver_observer_reports_fail_closed_denial() {
    let runtime = Runtime::new();
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_ctx, _module, params| Ok(vec![params[0] + 1]),
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .with_name("inc")
        .export("inc")
        .instantiate(&Context::default())
        .unwrap();

    let events = Arc::new(Mutex::new(Vec::new()));
    let ctx = with_import_resolver_observer(
        &with_import_resolver_config(
            &Context::default(),
            ImportResolverConfig {
                acl: Some(ImportACL::new().allow_modules(["env"])),
                fail_closed: true,
                ..ImportResolverConfig::default()
            },
        ),
        {
            let events = events.clone();
            move |_ctx: &Context, observation: ImportResolverObservation| {
                events
                    .lock()
                    .expect("import observer events poisoned")
                    .push(ImportObserverRecord {
                        import_module: observation.import_module,
                        event: observation.event,
                        resolved: observation.resolved_module.is_some(),
                    });
            }
        },
    );

    let err = runtime
        .instantiate_with_context(
            &ctx,
            &runtime.compile(GUEST_IMPORT_INC_WASM).unwrap(),
            ModuleConfig::new(),
        )
        .err()
        .expect("fail-closed import instantiation should fail");
    assert!(err.to_string().contains("unresolved by import resolver"));
    assert_eq!(
        vec![
            ImportObserverRecord {
                import_module: "env".to_string(),
                event: ImportResolverEvent::AclAllowed,
                resolved: false,
            },
            ImportObserverRecord {
                import_module: "env".to_string(),
                event: ImportResolverEvent::FailClosedDenied,
                resolved: false,
            },
        ],
        *events.lock().expect("import observer events poisoned")
    );
}

#[test]
fn import_resolver_observer_reports_resolver_attempt_before_store_fallback() {
    let runtime = Runtime::new();
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_ctx, _module, params| Ok(vec![params[0] + 1]),
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .with_name("inc")
        .export("inc")
        .instantiate(&Context::default())
        .unwrap();

    let events = Arc::new(Mutex::new(Vec::new()));
    let ctx = with_import_resolver_observer(
        &with_import_resolver_config(
            &Context::default(),
            ImportResolverConfig {
                resolver: Some(Arc::new(|_| None)),
                ..ImportResolverConfig::default()
            },
        ),
        {
            let events = events.clone();
            move |_ctx: &Context, observation: ImportResolverObservation| {
                events
                    .lock()
                    .expect("import observer events poisoned")
                    .push(ImportObserverRecord {
                        import_module: observation.import_module,
                        event: observation.event,
                        resolved: observation.resolved_module.is_some(),
                    });
            }
        },
    );

    let guest = runtime
        .instantiate_with_context(
            &ctx,
            &runtime.compile(GUEST_IMPORT_INC_WASM).unwrap(),
            ModuleConfig::new(),
        )
        .unwrap();
    assert_eq!(
        vec![42],
        guest.exported_function("run").unwrap().call(&[41]).unwrap()
    );
    assert_eq!(
        vec![
            ImportObserverRecord {
                import_module: "env".to_string(),
                event: ImportResolverEvent::ResolverAttempted,
                resolved: false,
            },
            ImportObserverRecord {
                import_module: "env".to_string(),
                event: ImportResolverEvent::StoreFallback,
                resolved: false,
            },
        ],
        *events.lock().expect("import observer events poisoned")
    );
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

#[test]
fn denied_host_import_aborts_only_caller_listener() {
    let runtime = Runtime::new();
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_ctx, _module, params| Ok(vec![params[0] + 1]),
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .with_name("inc")
        .export("inc")
        .instantiate(&Context::default())
        .unwrap();
    let guest = runtime
        .instantiate_binary(
            GUEST_IMPORT_INC_WASM,
            ModuleConfig::new().with_name("guest"),
        )
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let call_ctx = with_function_listener_factory(
        &Context::default(),
        AbortRecordingFactory {
            events: events.clone(),
        },
    );

    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&with_host_call_policy(&call_ctx, deny_inc_host_call), &[41])
        .unwrap_err();

    assert_eq!("policy denied: host call", err.to_string());
    assert_eq!(
        vec![
            "before:run".to_string(),
            "abort:run:policy denied: host call".to_string()
        ],
        *events.lock().expect("events poisoned")
    );
}
