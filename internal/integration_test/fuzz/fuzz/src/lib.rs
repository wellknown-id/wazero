use std::sync::{Arc, Mutex};

use arbitrary::Arbitrary;
use libfuzzer_sys::arbitrary::{Result, Unstructured};
use razero::{
    logging, trap_cause_of, with_function_listener_factory, with_host_call_policy,
    with_host_call_policy_observer, with_trap_observer, with_yield_observer, with_yield_policy,
    with_yielder, Context, CoreFeatures, FunctionDefinition, FunctionListener,
    FunctionListenerFactory, HostCallPolicyDecision, HostCallPolicyObservation, Module,
    ModuleConfig, Runtime, RuntimeConfig, RuntimeError, TrapCause, TrapObservation, ValueType,
    YieldEvent, YieldObservation, YieldPolicyRequest, CORE_FEATURES_TAIL_CALL,
    CORE_FEATURES_THREADS,
};
use wasm_smith::Config;

const YIELD_WASM: &[u8] = include_bytes!("../../../../../experimental/testdata/yield.wasm");
const OOB_LOAD_WASM: &[u8] = include_bytes!("../../../../../testdata/oob_load.wasm");
const GUEST_HOST_CALL_DENY_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f, 0x01,
    0x7f, 0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04, b'h', b'o', b'o', b'k', 0x00, 0x00,
    0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01, 0x0a, 0x08,
    0x01, 0x06, 0x00, 0x41, 0x2a, 0x10, 0x00, 0x0b,
];
const DIV_BY_ZERO_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
    0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x09,
    0x01, 0x07, 0x00, 0x41, 0x05, 0x41, 0x00, 0x6d, 0x0b,
];
const DIV_OVERFLOW_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
    0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x0d,
    0x01, 0x0b, 0x00, 0x41, 0x80, 0x80, 0x80, 0x80, 0x78, 0x41, 0x7f, 0x6d, 0x0b,
];
const INVALID_CONVERSION_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
    0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x0a,
    0x01, 0x08, 0x00, 0x43, 0x00, 0x00, 0xc0, 0x7f, 0xa8, 0x0b,
];
const INVALID_TABLE_ACCESS_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03,
    0x02, 0x01, 0x00, 0x04, 0x04, 0x01, 0x70, 0x00, 0x01, 0x07, 0x07, 0x01, 0x03, b'r', b'u',
    b'n', 0x00, 0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x41, 0x01, 0x11, 0x00, 0x00, 0x0b,
];
const INDIRECT_CALL_TYPE_MISMATCH_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x60, 0x00, 0x01, 0x7f,
    0x60, 0x00, 0x00, 0x03, 0x03, 0x02, 0x01, 0x00, 0x04, 0x04, 0x01, 0x70, 0x00, 0x01, 0x07,
    0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01, 0x09, 0x07, 0x01, 0x00, 0x41, 0x00, 0x0b,
    0x01, 0x00, 0x0a, 0x0c, 0x02, 0x02, 0x00, 0x0b, 0x07, 0x00, 0x41, 0x00, 0x11, 0x00, 0x00,
    0x0b,
];
const UNREACHABLE_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03,
    0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x05, 0x01,
    0x03, 0x00, 0x00, 0x0b,
];
const UNALIGNED_ATOMIC_STORE_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03,
    0x02, 0x01, 0x00, 0x05, 0x04, 0x01, 0x03, 0x01, 0x01, 0x07, 0x07, 0x01, 0x03, b'r', b'u',
    b'n', 0x00, 0x00, 0x0a, 0x0c, 0x01, 0x0a, 0x00, 0x41, 0x01, 0x41, 0x2a, 0xfe, 0x17, 0x02,
    0x00, 0x0b,
];

#[derive(Clone, Copy)]
pub struct ParityOptions {
    pub check_memory: bool,
    pub check_logging: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExecutionSnapshot {
    CompileError(String),
    InstantiateError(String),
    SetupError(String),
    Executed {
        functions: Vec<FunctionSnapshot>,
        memory: Option<Vec<u8>>,
        logs: Option<Vec<String>>,
        trap_observations: Option<Vec<TrapSnapshot>>,
        yield_observations: Option<Vec<YieldSnapshot>>,
        host_call_policy_observations: Option<Vec<HostCallPolicySnapshot>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionSnapshot {
    name: String,
    outcome: FunctionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FunctionOutcome {
    Returned(Vec<u64>),
    Yielded {
        message: String,
    },
    Trapped {
        message: String,
        cause: Option<TrapCause>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrapSnapshot {
    module_name: Option<String>,
    cause: TrapCause,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct YieldSnapshot {
    module_name: Option<String>,
    event: YieldEvent,
    yield_count: u64,
    expected_host_results: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostCallPolicySnapshot {
    module_name: Option<String>,
    function_name: Option<String>,
    caller_module_name: Option<String>,
    decision: HostCallPolicyDecision,
}

#[derive(Arbitrary, Clone, Copy, Debug)]
enum PolicyTrapScenario {
    InitialDeny,
    ResumeDeny,
    HostCallDeny,
}

#[derive(Arbitrary, Clone, Copy, Debug)]
struct PolicyTrapInput {
    scenario: PolicyTrapScenario,
    resume_value: u64,
}

#[derive(Arbitrary, Clone, Copy, Debug)]
enum FixedTrapScenario {
    OutOfBounds,
    DivideByZero,
    DivideOverflow,
    InvalidConversion,
    InvalidTableAccess,
    IndirectCallTypeMismatch,
    Unreachable,
    UnalignedAtomic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResumeSnapshot {
    initial: FunctionOutcome,
    resumed: FunctionOutcome,
    trap_observations: Vec<TrapSnapshot>,
    yield_observations: Vec<YieldSnapshot>,
}

#[derive(Debug, Clone, Default)]
struct CaptureOptions {
    check_memory: bool,
    check_logging: bool,
    capture_traps: bool,
    capture_yield_observations: bool,
    capture_host_call_policy_observations: bool,
    attach_yielder: bool,
    deny_yields: bool,
    deny_host_calls: bool,
    export_name: Option<String>,
    call_params: Option<Vec<u64>>,
}

fn fuzz_features() -> CoreFeatures {
    CoreFeatures::V2 | CORE_FEATURES_THREADS | CORE_FEATURES_TAIL_CALL
}

fn runtime_config(secure_mode: bool) -> RuntimeConfig {
    RuntimeConfig::new()
        .with_core_features(fuzz_features())
        .with_secure_mode(secure_mode)
}

pub fn replay_native_parity(wasm: &[u8], options: ParityOptions) {
    let options = CaptureOptions::from(options);
    let standard = capture_execution(wasm, false, &options);
    let secure = capture_execution(wasm, true, &options);
    assert_eq!(
        standard, secure,
        "native parity mismatch between standard and secure mode"
    );
}

pub fn replay_native_trap_parity(wasm: &[u8], export_name: &str) {
    let options = CaptureOptions {
        capture_traps: true,
        export_name: Some(export_name.to_string()),
        ..CaptureOptions::default()
    };
    let standard = capture_execution(wasm, false, &options);
    let secure = capture_execution(wasm, true, &options);
    assert_eq!(
        standard, secure,
        "native trap parity mismatch between standard and secure mode"
    );
}

pub fn replay_initial_policy_trap_parity() {
    replay_policy_trap_parity(PolicyTrapInput {
        scenario: PolicyTrapScenario::InitialDeny,
        resume_value: 0,
    });
}

pub fn replay_resume_policy_trap_parity(resume_value: u64) {
    replay_policy_trap_parity(PolicyTrapInput {
        scenario: PolicyTrapScenario::ResumeDeny,
        resume_value,
    });
}

pub fn replay_host_call_policy_trap_parity() {
    replay_policy_trap_parity(PolicyTrapInput {
        scenario: PolicyTrapScenario::HostCallDeny,
        resume_value: 0,
    });
}

pub fn run_policy_trap_parity(data: &[u8]) -> Result<()> {
    let mut u = Unstructured::new(data);
    let input = PolicyTrapInput::arbitrary(&mut u)?;
    replay_policy_trap_parity(input);
    Ok(())
}

pub fn run_fixed_trap_parity(data: &[u8]) -> Result<()> {
    let mut u = Unstructured::new(data);
    let scenario = FixedTrapScenario::arbitrary(&mut u)?;
    replay_fixed_trap_parity(scenario);
    Ok(())
}

pub fn replay_all_fixed_trap_fixtures() {
    for scenario in [
        FixedTrapScenario::OutOfBounds,
        FixedTrapScenario::DivideByZero,
        FixedTrapScenario::DivideOverflow,
        FixedTrapScenario::InvalidConversion,
        FixedTrapScenario::InvalidTableAccess,
        FixedTrapScenario::IndirectCallTypeMismatch,
        FixedTrapScenario::Unreachable,
        FixedTrapScenario::UnalignedAtomic,
    ] {
        replay_fixed_trap_parity(scenario);
    }
}

pub fn replay_validation(wasm: &[u8]) {
    let ctx = Context::default();
    let runtime = Runtime::with_config(runtime_config(false));
    let _ = runtime.compile(wasm);
    runtime.close(&ctx).expect("runtime close should succeed");
}

pub fn run_native_parity(data: &[u8], check_memory: bool, check_logging: bool) -> Result<()> {
    let wasm = generate_execution_module(data, check_logging)?;
    replay_native_parity(
        &wasm,
        ParityOptions {
            check_memory,
            check_logging,
        },
    );
    Ok(())
}

pub fn run_validation(data: &[u8]) -> Result<()> {
    let mut u = Unstructured::new(data);
    let mut config = Config::arbitrary(&mut u)?;
    config.threads_enabled = true;
    config.tail_call_enabled = true;
    config.allow_invalid_funcs = true;

    let module = wasm_smith::Module::new(config, &mut u)?;
    replay_validation(&module.to_bytes());
    Ok(())
}

fn generate_execution_module(data: &[u8], check_logging: bool) -> Result<Vec<u8>> {
    let mut u = Unstructured::new(data);
    let mut config = Config::arbitrary(&mut u)?;

    config.memory64_enabled = false;
    config.max_memories = 1;
    config.min_memories = 1;
    config.max_memory32_pages = 10;
    config.memory_max_size_required = true;
    config.max_tables = 2;
    config.max_table_elements = 1_000;
    config.table_max_size_required = true;
    config.max_instructions = 5_000;
    config.canonicalize_nans = true;
    config.export_everything = true;
    config.min_funcs = 1;
    config.max_funcs = config.max_funcs.max(1);
    config.threads_enabled = true;

    if check_logging {
        config.reference_types_enabled = false;
    } else {
        config.tail_call_enabled = true;
    }

    let mut module = wasm_smith::Module::new(config, &mut u)?;
    module
        .ensure_termination(1_000)
        .expect("termination instrumentation should succeed");
    Ok(module.to_bytes())
}

fn capture_execution(wasm: &[u8], secure_mode: bool, options: &CaptureOptions) -> ExecutionSnapshot {
    let ctx = Context::default();
    let runtime = Runtime::with_config(runtime_config(secure_mode));
    let outcome = capture_execution_with_runtime(&runtime, &ctx, wasm, options);
    runtime.close(&ctx).expect("runtime close should succeed");
    outcome
}

fn capture_execution_with_runtime(
    runtime: &Runtime,
    _ctx: &Context,
    wasm: &[u8],
    options: &CaptureOptions,
) -> ExecutionSnapshot {
    let compiled = match runtime.compile(wasm) {
        Ok(compiled) => compiled,
        Err(err) => {
            return ExecutionSnapshot::CompileError(err.to_string());
        }
    };

    let module = match runtime.instantiate(&compiled, ModuleConfig::new()) {
        Ok(module) => module,
        Err(err) => {
            return ExecutionSnapshot::InstantiateError(err.to_string());
        }
    };

    let logs = options
        .check_logging
        .then(|| Arc::new(Mutex::new(Vec::new())));
    let trap_observations = options
        .capture_traps
        .then(|| Arc::new(Mutex::new(Vec::new())));
    let yield_observations = options
        .capture_yield_observations
        .then(|| Arc::new(Mutex::new(Vec::new())));
    let host_call_policy_observations = options
        .capture_host_call_policy_observations
        .then(|| Arc::new(Mutex::new(Vec::new())));
    let call_ctx = build_call_context(
        options,
        logs.clone(),
        trap_observations.clone(),
        yield_observations.clone(),
        host_call_policy_observations.clone(),
    );

    let exports = match selected_exports(&module, options) {
        Ok(exports) => exports,
        Err(err) => return ExecutionSnapshot::SetupError(err),
    };

    let mut functions = Vec::new();
    for name in exports {
        let function = module
            .exported_function(&name)
            .expect("exported function should exist");
        let params = options.call_params.as_deref().unwrap_or(&[]);
        let outcome = match function.call_with_context(&call_ctx, params) {
            Ok(results) => FunctionOutcome::Returned(results),
            Err(err) => runtime_error_to_outcome(err),
        };
        functions.push(FunctionSnapshot { name, outcome });
    }

    let memory = options.check_memory.then(|| {
        module
            .memory()
            .and_then(|memory| memory.read(0, memory.size() as usize))
            .unwrap_or_default()
    });
    let logs = logs.map(|events| events.lock().expect("log buffer poisoned").clone());
    let trap_observations = trap_observations
        .map(|events| events.lock().expect("trap observations poisoned").clone());
    let yield_observations = yield_observations
        .map(|events| events.lock().expect("yield observations poisoned").clone());
    let host_call_policy_observations = host_call_policy_observations.map(|events| {
        events
            .lock()
            .expect("host-call policy observations poisoned")
            .clone()
    });

    let outcome = ExecutionSnapshot::Executed {
        functions,
        memory,
        logs,
        trap_observations,
        yield_observations,
        host_call_policy_observations,
    };
    outcome
}

fn replay_policy_trap_parity(input: PolicyTrapInput) {
    match input.scenario {
        PolicyTrapScenario::InitialDeny => {
            let options = CaptureOptions {
                capture_traps: true,
                capture_yield_observations: true,
                attach_yielder: true,
                deny_yields: true,
                export_name: Some("run".to_string()),
                ..CaptureOptions::default()
            };
            let standard = capture_execution(YIELD_WASM, false, &options);
            let secure = capture_execution(YIELD_WASM, true, &options);
            assert_execution_has_no_yield_events(&standard);
            assert_execution_has_no_yield_events(&secure);
            assert_eq!(
                standard, secure,
                "policy denial mismatch between standard and secure mode"
            );
        }
        PolicyTrapScenario::ResumeDeny => {
            let standard = capture_resume_policy_denial(false, input.resume_value);
            let secure = capture_resume_policy_denial(true, input.resume_value);
            assert_resume_snapshot_expectations(&standard);
            assert_resume_snapshot_expectations(&secure);
            assert_eq!(
                standard, secure,
                "resume-path policy denial mismatch between standard and secure mode"
            );
        }
        PolicyTrapScenario::HostCallDeny => {
            let standard = capture_host_call_policy_denial(false);
            let secure = capture_host_call_policy_denial(true);
            assert_host_call_policy_snapshot(&standard);
            assert_host_call_policy_snapshot(&secure);
            assert_eq!(
                standard, secure,
                "host-call policy denial mismatch between standard and secure mode"
            );
        }
    }
}

fn replay_fixed_trap_parity(scenario: FixedTrapScenario) {
    match scenario {
        FixedTrapScenario::OutOfBounds => replay_native_trap_parity(OOB_LOAD_WASM, "oob"),
        FixedTrapScenario::DivideByZero => replay_native_trap_parity(DIV_BY_ZERO_WASM, "run"),
        FixedTrapScenario::DivideOverflow => replay_native_trap_parity(DIV_OVERFLOW_WASM, "run"),
        FixedTrapScenario::InvalidConversion => {
            replay_native_trap_parity(INVALID_CONVERSION_WASM, "run")
        }
        FixedTrapScenario::InvalidTableAccess => {
            replay_native_trap_parity(INVALID_TABLE_ACCESS_WASM, "run")
        }
        FixedTrapScenario::IndirectCallTypeMismatch => {
            replay_native_trap_parity(INDIRECT_CALL_TYPE_MISMATCH_WASM, "run")
        }
        FixedTrapScenario::Unreachable => replay_native_trap_parity(UNREACHABLE_WASM, "run"),
        FixedTrapScenario::UnalignedAtomic => {
            replay_native_trap_parity(UNALIGNED_ATOMIC_STORE_WASM, "run")
        }
    }
}

fn capture_resume_policy_denial(secure_mode: bool, resume_value: u64) -> ResumeSnapshot {
    let ctx = Context::default();
    let runtime = Runtime::with_config(runtime_config(secure_mode));
    install_yield_host(&runtime);
    let guest = runtime
        .instantiate_binary(YIELD_WASM, ModuleConfig::new())
        .expect("yield guest should instantiate");
    let trap_observations = Arc::new(Mutex::new(Vec::new()));
    let yield_observations = Arc::new(Mutex::new(Vec::new()));

    let initial_ctx = build_call_context(
        &CaptureOptions {
            attach_yielder: true,
            capture_traps: true,
            capture_yield_observations: true,
            ..CaptureOptions::default()
        },
        None,
        Some(trap_observations.clone()),
        Some(yield_observations.clone()),
        None,
    );
    let initial_err = guest
        .exported_function("run_twice")
        .expect("run_twice export should exist")
        .call_with_context(&initial_ctx, &[])
        .expect_err("run_twice should yield before resume");
    let initial = runtime_error_to_outcome(initial_err.clone());
    let resumer = match initial_err {
        RuntimeError::Yield(yield_error) => yield_error.resumer().expect("resumer should exist"),
        other => panic!("expected yielded error, got {}", other),
    };

    let resumed_ctx = build_call_context(
        &CaptureOptions {
            attach_yielder: true,
            capture_traps: true,
            capture_yield_observations: true,
            deny_yields: true,
            ..CaptureOptions::default()
        },
        None,
        Some(trap_observations.clone()),
        Some(yield_observations.clone()),
        None,
    );
    let resumed = match resumer.resume(&resumed_ctx, &[resume_value]) {
        Ok(results) => FunctionOutcome::Returned(results),
        Err(err) => runtime_error_to_outcome(err),
    };

    let snapshot = ResumeSnapshot {
        initial,
        resumed,
        trap_observations: trap_observations
            .lock()
            .expect("trap observations poisoned")
            .clone(),
        yield_observations: yield_observations
            .lock()
            .expect("yield observations poisoned")
            .clone(),
    };
    runtime.close(&ctx).expect("runtime close should succeed");
    snapshot
}

fn capture_host_call_policy_denial(secure_mode: bool) -> ExecutionSnapshot {
    let ctx = Context::default();
    let runtime = Runtime::with_config(runtime_config(secure_mode));
    install_host_call_deny_host(&runtime);
    let options = CaptureOptions {
        capture_traps: true,
        capture_host_call_policy_observations: true,
        deny_host_calls: true,
        export_name: Some("run".to_string()),
        call_params: Some(vec![0]),
        ..CaptureOptions::default()
    };
    let snapshot =
        capture_execution_with_runtime(&runtime, &ctx, GUEST_HOST_CALL_DENY_WASM, &options);
    runtime.close(&ctx).expect("runtime close should succeed");
    snapshot
}

fn build_call_context(
    options: &CaptureOptions,
    logs: Option<Arc<Mutex<Vec<String>>>>,
    trap_observations: Option<Arc<Mutex<Vec<TrapSnapshot>>>>,
    yield_observations: Option<Arc<Mutex<Vec<YieldSnapshot>>>>,
    host_call_policy_observations: Option<Arc<Mutex<Vec<HostCallPolicySnapshot>>>>,
) -> Context {
    let mut ctx = Context::default();

    if options.attach_yielder {
        ctx = with_yielder(&ctx);
    }
    if options.deny_yields {
        ctx = with_yield_policy(&ctx, deny_all_yields);
    }
    if options.deny_host_calls {
        ctx = with_host_call_policy(&ctx, deny_all_host_calls);
    }
    if let Some(events) = logs {
        ctx = with_function_listener_factory(
            &ctx,
            RecordingFactory {
                events: events.clone(),
            },
        );
    }
    if let Some(observations) = trap_observations {
        ctx = with_trap_observer(&ctx, move |_ctx: &Context, observation: TrapObservation| {
            observations
                .lock()
                .expect("trap observations poisoned")
                .push(TrapSnapshot {
                    module_name: observation.module.name().map(str::to_string),
                    cause: observation.cause,
                    message: observation.err.to_string(),
                });
        });
    }
    if let Some(observations) = yield_observations {
        ctx = with_yield_observer(&ctx, move |_ctx: &Context, observation: YieldObservation| {
            observations
                .lock()
                .expect("yield observations poisoned")
                .push(YieldSnapshot {
                    module_name: observation.module.name().map(str::to_string),
                    event: observation.event,
                    yield_count: observation.yield_count,
                    expected_host_results: observation.expected_host_results,
                });
        });
    }
    if let Some(observations) = host_call_policy_observations {
        ctx = with_host_call_policy_observer(
            &ctx,
            move |_ctx: &Context, observation: HostCallPolicyObservation| {
                observations
                    .lock()
                    .expect("host-call policy observations poisoned")
                    .push(HostCallPolicySnapshot {
                        module_name: observation.module.name().map(str::to_string),
                        function_name: observation.request.name().map(str::to_string),
                        caller_module_name: observation
                            .request
                            .caller_module_name()
                            .map(str::to_string),
                        decision: observation.decision,
                    });
            },
        );
    }

    ctx
}

fn install_yield_host(runtime: &Runtime) {
    runtime
        .new_host_module_builder("example")
        .new_function_builder()
        .with_func(
            |ctx, _module, _params| {
                razero::get_yielder(&ctx)
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
        .expect("yield host should instantiate");
}

fn install_host_call_deny_host(runtime: &Runtime) {
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(|_ctx, _module, _params| Ok(vec![7]), &[ValueType::I32], &[ValueType::I32])
        .with_name("hook_impl")
        .export("hook")
        .instantiate(&Context::default())
        .expect("host-call denial fixture should instantiate");
}

fn selected_exports(module: &Module, options: &CaptureOptions) -> std::result::Result<Vec<String>, String> {
    if let Some(export_name) = &options.export_name {
        let definitions = module.exported_function_definitions();
        let Some(definition) = definitions.get(export_name) else {
            return Err(format!("missing exported function: {export_name}"));
        };
        let actual_param_count = definition.param_types().len();
        let provided_param_count = options.call_params.as_ref().map_or(0, Vec::len);
        if actual_param_count != provided_param_count {
            return Err(format!(
                "exported function {export_name} requires {actual_param_count} params, but capture options provided {provided_param_count}"
            ));
        }
        return Ok(vec![export_name.clone()]);
    }

    let mut exports = module
        .exported_function_definitions()
        .into_iter()
        .filter_map(|(name, definition)| definition.param_types().is_empty().then_some(name))
        .collect::<Vec<_>>();
    exports.sort();
    Ok(exports)
}

fn deny_all_yields(_ctx: &Context, _request: &YieldPolicyRequest) -> bool {
    false
}

fn deny_all_host_calls(_ctx: &Context, _request: &razero::HostCallPolicyRequest) -> bool {
    false
}

fn runtime_error_to_outcome(err: RuntimeError) -> FunctionOutcome {
    match err {
        RuntimeError::Yield(err) => FunctionOutcome::Yielded {
            message: err.to_string(),
        },
        other => FunctionOutcome::Trapped {
            cause: trap_cause_of(&other),
            message: other.to_string(),
        },
    }
}

fn assert_execution_has_no_yield_events(snapshot: &ExecutionSnapshot) {
    let ExecutionSnapshot::Executed {
        yield_observations, ..
    } = snapshot
    else {
        return;
    };
    assert_eq!(
        Some(Vec::<YieldSnapshot>::new()),
        yield_observations.clone(),
        "initial policy denial should not emit yield observer events"
    );
}

fn assert_resume_snapshot_expectations(snapshot: &ResumeSnapshot) {
    assert_eq!(
        vec![
            YieldSnapshot {
                module_name: None,
                event: YieldEvent::Yielded,
                yield_count: 1,
                expected_host_results: 1,
            },
            YieldSnapshot {
                module_name: None,
                event: YieldEvent::Resumed,
                yield_count: 1,
                expected_host_results: 1,
            },
        ],
        snapshot.yield_observations
    );
}

fn assert_host_call_policy_snapshot(snapshot: &ExecutionSnapshot) {
    let ExecutionSnapshot::Executed {
        host_call_policy_observations,
        trap_observations,
        ..
    } = snapshot
    else {
        panic!("host-call policy scenario should execute");
    };
    let observations = host_call_policy_observations
        .as_ref()
        .expect("host-call policy observations should exist");
    assert!(
        observations.iter().any(|observation| {
            observation.function_name.as_deref() == Some("hook_impl")
                && observation.decision == HostCallPolicyDecision::Denied
        }),
        "host-call policy denial should record a denied hook_impl observation"
    );
    assert!(
        trap_observations.as_ref().is_some_and(|observations| {
            observations.iter().any(|observation| {
                observation.cause == TrapCause::PolicyDenied
                    && observation.message == "policy denied: host call"
            })
        }),
        "host-call policy denial should record a policy-denied trap observation"
    );
}

impl From<ParityOptions> for CaptureOptions {
    fn from(value: ParityOptions) -> Self {
        Self {
            check_memory: value.check_memory,
            check_logging: value.check_logging,
            ..Self::default()
        }
    }
}

struct RecordingFactory {
    events: Arc<Mutex<Vec<String>>>,
}

impl FunctionListenerFactory for RecordingFactory {
    fn new_listener(&self, _definition: &FunctionDefinition) -> Option<Arc<dyn FunctionListener>> {
        Some(Arc::new(RecordingListener {
            events: self.events.clone(),
        }))
    }
}

struct RecordingListener {
    events: Arc<Mutex<Vec<String>>>,
}

impl FunctionListener for RecordingListener {
    fn before(
        &self,
        _ctx: &Context,
        module: &Module,
        definition: &FunctionDefinition,
        params: &[u64],
        _stack_iterator: &mut dyn razero::StackIterator,
    ) {
        self.push(format!(
            "before:{}({})",
            definition.name(),
            format_params(definition, module, params)
        ));
    }

    fn after(
        &self,
        _ctx: &Context,
        module: &Module,
        definition: &FunctionDefinition,
        results: &[u64],
    ) {
        self.push(format!(
            "after:{}({})",
            definition.name(),
            format_results(definition, module, results)
        ));
    }

    fn abort(
        &self,
        _ctx: &Context,
        _module: &Module,
        definition: &FunctionDefinition,
        error: &RuntimeError,
    ) {
        self.push(format!("abort:{}:{}", definition.name(), error));
    }
}

impl RecordingListener {
    fn push(&self, entry: String) {
        self.events.lock().expect("log buffer poisoned").push(entry);
    }
}

fn format_params(definition: &FunctionDefinition, module: &Module, params: &[u64]) -> String {
    let (loggers, _) = logging::config(definition);
    format_values(&loggers, module, params)
}

fn format_results(definition: &FunctionDefinition, module: &Module, results: &[u64]) -> String {
    let (_, loggers) = logging::config(definition);
    format_values(&loggers, module, results)
}

fn format_values<T>(loggers: &[T], module: &Module, values: &[u64]) -> String
where
    T: ValueLogger,
{
    let memory = module.memory();
    let mut written = Vec::new();
    for (index, logger) in loggers.iter().enumerate() {
        if index > 0 {
            written.extend_from_slice(b", ");
        }
        logger
            .write(memory.as_ref(), &mut written, values)
            .expect("log formatting should succeed");
    }
    String::from_utf8(written).expect("formatted log should be utf-8")
}

trait ValueLogger {
    fn write(
        &self,
        memory: Option<&razero::Memory>,
        writer: &mut Vec<u8>,
        values: &[u64],
    ) -> std::io::Result<()>;
}

impl ValueLogger for logging::ParamLogger {
    fn write(
        &self,
        memory: Option<&razero::Memory>,
        writer: &mut Vec<u8>,
        values: &[u64],
    ) -> std::io::Result<()> {
        self.log(memory, writer, values)
    }
}

impl ValueLogger for logging::ResultLogger {
    fn write(
        &self,
        memory: Option<&razero::Memory>,
        writer: &mut Vec<u8>,
        values: &[u64],
    ) -> std::io::Result<()> {
        self.log(memory, writer, values)
    }
}
