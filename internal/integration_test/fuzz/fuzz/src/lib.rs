use std::sync::{Arc, Mutex};

use arbitrary::Arbitrary;
use libfuzzer_sys::arbitrary::{Result, Unstructured};
use razero::{
    logging, trap_cause_of, with_function_listener_factory, with_trap_observer,
    with_yield_policy, with_yielder, Context, CoreFeatures, FunctionDefinition, FunctionListener,
    FunctionListenerFactory, Module, ModuleConfig, Runtime, RuntimeConfig, RuntimeError,
    TrapCause, TrapObservation, YieldPolicyRequest, CORE_FEATURES_TAIL_CALL, CORE_FEATURES_THREADS,
};
use wasm_smith::Config;

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

#[derive(Debug, Clone, Default)]
struct CaptureOptions {
    check_memory: bool,
    check_logging: bool,
    capture_traps: bool,
    attach_yielder: bool,
    deny_yields: bool,
    export_name: Option<String>,
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
    let compiled = match runtime.compile(wasm) {
        Ok(compiled) => compiled,
        Err(err) => {
            let outcome = ExecutionSnapshot::CompileError(err.to_string());
            runtime.close(&ctx).expect("runtime close should succeed");
            return outcome;
        }
    };

    let module = match runtime.instantiate(&compiled, ModuleConfig::new()) {
        Ok(module) => module,
        Err(err) => {
            let outcome = ExecutionSnapshot::InstantiateError(err.to_string());
            runtime.close(&ctx).expect("runtime close should succeed");
            return outcome;
        }
    };

    let logs = options
        .check_logging
        .then(|| Arc::new(Mutex::new(Vec::new())));
    let trap_observations = options
        .capture_traps
        .then(|| Arc::new(Mutex::new(Vec::new())));
    let call_ctx = build_call_context(options, logs.clone(), trap_observations.clone());

    let exports = match selected_exports(&module, options) {
        Ok(exports) => exports,
        Err(err) => {
            let outcome = ExecutionSnapshot::SetupError(err);
            runtime.close(&ctx).expect("runtime close should succeed");
            return outcome;
        }
    };

    let mut functions = Vec::new();
    for name in exports {
        let function = module
            .exported_function(&name)
            .expect("exported function should exist");
        let outcome = match function.call_with_context(&call_ctx, &[]) {
            Ok(results) => FunctionOutcome::Returned(results),
            Err(err) => FunctionOutcome::Trapped {
                cause: trap_cause_of(&err),
                message: err.to_string(),
            },
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

    let outcome = ExecutionSnapshot::Executed {
        functions,
        memory,
        logs,
        trap_observations,
    };
    runtime.close(&ctx).expect("runtime close should succeed");
    outcome
}

fn build_call_context(
    options: &CaptureOptions,
    logs: Option<Arc<Mutex<Vec<String>>>>,
    trap_observations: Option<Arc<Mutex<Vec<TrapSnapshot>>>>,
) -> Context {
    let mut ctx = Context::default();

    if options.attach_yielder {
        ctx = with_yielder(&ctx);
    }
    if options.deny_yields {
        ctx = with_yield_policy(&ctx, deny_all_yields);
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

    ctx
}

fn selected_exports(module: &Module, options: &CaptureOptions) -> std::result::Result<Vec<String>, String> {
    if let Some(export_name) = &options.export_name {
        let definitions = module.exported_function_definitions();
        let Some(definition) = definitions.get(export_name) else {
            return Err(format!("missing exported function: {export_name}"));
        };
        if !definition.param_types().is_empty() {
            return Err(format!(
                "exported function {export_name} requires {} params",
                definition.param_types().len()
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
