use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

use razero::{
    get_compilation_workers, get_function_listener_factory, get_host_call_policy,
    get_host_call_policy_observer, get_import_resolver, get_import_resolver_observer,
    get_trap_observer, get_yield_policy, get_yield_policy_observer, with_compilation_workers,
    with_function_listener_factory, with_host_call_policy, with_host_call_policy_observer,
    with_import_resolver, with_import_resolver_observer, with_trap_observer, with_yield_policy,
    with_yield_policy_observer, Context, HostCallPolicyDecision, HostCallPolicyObservation,
    ImportResolverEvent, ImportResolverObservation, ModuleConfig, Runtime, RuntimeConfig,
    TrapCause, TrapObservation, YieldPolicyDecision, YieldPolicyObservation,
};

const SIMPLE_EXPORT_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, 0x03,
    0x02, 0x01, 0x00, 0x07, 0x05, 0x01, 0x01, b'f', 0x00, 0x00, 0x0a, 0x06, 0x01, 0x04, 0x00, 0x41,
    0x2a, 0x0b,
];

fn allow_host_calls(_ctx: &Context, _request: &razero::HostCallPolicyRequest) -> bool {
    true
}

fn allow_yields(_ctx: &Context, _request: &razero::YieldPolicyRequest) -> bool {
    true
}

#[test]
fn compilation_workers_getter_clamps_zero_to_one() {
    let ctx = with_compilation_workers(&Context::default(), 0);
    assert_eq!(1, get_compilation_workers(&ctx));
}

#[test]
fn compilation_workers_getter_clamps_negative_to_one() {
    let ctx = with_compilation_workers(&Context::default(), -7);
    assert_eq!(1, get_compilation_workers(&ctx));
}

#[test]
fn compilation_workers_round_trip_positive_value() {
    let ctx = with_compilation_workers(&Context::default(), 4);
    assert_eq!(4, get_compilation_workers(&ctx));
}

#[test]
fn compilation_workers_drive_context_aware_compile_paths() {
    let runtime = Runtime::with_config(RuntimeConfig::new_interpreter());
    let ctx = with_compilation_workers(&Context::default(), 4);
    let compiled = runtime
        .compile_with_context(&ctx, SIMPLE_EXPORT_WASM)
        .unwrap();
    let module = runtime
        .instantiate_with_context(&ctx, &compiled, ModuleConfig::new())
        .unwrap();

    let results = module.exported_function("f").unwrap().call(&[]).unwrap();
    assert_eq!(vec![42], results);
}

#[test]
fn close_on_context_done_round_trips_in_runtime_config() {
    let config = RuntimeConfig::new().with_close_on_context_done(true);
    assert!(config.close_on_context_done());
}

#[test]
fn host_call_policy_round_trips_through_public_surface() {
    let ctx = with_host_call_policy(&Context::default(), allow_host_calls);
    let policy = get_host_call_policy(&ctx).expect("policy should be present");

    assert!(policy.allow_host_call(&ctx, &razero::HostCallPolicyRequest::new()));
    assert!(RuntimeConfig::new()
        .with_host_call_policy(allow_host_calls)
        .host_call_policy()
        .is_some());
}

#[test]
fn function_listener_factory_round_trips_through_public_surface() {
    let ctx = with_function_listener_factory(&Context::default(), |_definition: &razero::FunctionDefinition| {
        Some(Arc::new(
            |_ctx: &Context,
             _module: &razero::Module,
             _definition: &razero::FunctionDefinition,
             _params: &[u64],
             _stack: &mut dyn razero::StackIterator| {},
        ) as Arc<dyn razero::FunctionListener>)
    });
    let factory = get_function_listener_factory(&ctx).expect("factory should be present");

    assert!(factory
        .new_listener(&razero::FunctionDefinition::new("demo"))
        .is_some());
}

#[test]
fn host_call_policy_observer_round_trips_through_public_surface() {
    let observed = Arc::new(AtomicU32::new(0));
    let ctx = with_host_call_policy_observer(&Context::default(), {
        let observed = observed.clone();
        move |_ctx: &Context, observation: HostCallPolicyObservation| {
            assert_eq!(HostCallPolicyDecision::Allowed, observation.decision);
            observed.fetch_add(1, Ordering::SeqCst);
        }
    });
    let observer = get_host_call_policy_observer(&ctx).expect("observer should be present");
    let runtime = Runtime::new();
    let compiled = runtime.compile(SIMPLE_EXPORT_WASM).unwrap();
    let module = runtime
        .instantiate(&compiled, ModuleConfig::new().with_name("guest"))
        .unwrap();
    observer.observe_host_call_policy(
        &ctx,
        HostCallPolicyObservation {
            module,
            request: razero::HostCallPolicyRequest::new(),
            decision: HostCallPolicyDecision::Allowed,
        },
    );

    assert_eq!(1, observed.load(Ordering::SeqCst));
}

#[test]
fn yield_policy_round_trips_through_public_surface() {
    let ctx = with_yield_policy(&Context::default(), allow_yields);
    let policy = get_yield_policy(&ctx).expect("policy should be present");

    assert!(policy.allow_yield(&ctx, &razero::YieldPolicyRequest::new()));
    assert!(RuntimeConfig::new()
        .with_yield_policy(allow_yields)
        .yield_policy()
        .is_some());
}

#[test]
fn yield_policy_observer_round_trips_through_public_surface() {
    let observed = Arc::new(AtomicU32::new(0));
    let ctx = with_yield_policy_observer(&Context::default(), {
        let observed = observed.clone();
        move |_ctx: &Context, observation: YieldPolicyObservation| {
            assert_eq!(YieldPolicyDecision::Allowed, observation.decision);
            observed.fetch_add(1, Ordering::SeqCst);
        }
    });
    let observer = get_yield_policy_observer(&ctx).expect("observer should be present");
    let runtime = Runtime::new();
    let compiled = runtime.compile(SIMPLE_EXPORT_WASM).unwrap();
    let module = runtime
        .instantiate(&compiled, ModuleConfig::new().with_name("guest"))
        .unwrap();
    observer.observe_yield_policy(
        &ctx,
        YieldPolicyObservation {
            module,
            request: razero::YieldPolicyRequest::new(),
            decision: YieldPolicyDecision::Allowed,
        },
    );

    assert_eq!(1, observed.load(Ordering::SeqCst));
}

#[test]
fn trap_observer_round_trips_through_public_surface() {
    let observed = Arc::new(AtomicU32::new(0));
    let ctx = with_trap_observer(&Context::default(), {
        let observed = observed.clone();
        move |_ctx: &Context, observation: TrapObservation| {
            assert_eq!(TrapCause::MemoryFault, observation.cause);
            observed.fetch_add(1, Ordering::SeqCst);
        }
    });
    let observer = get_trap_observer(&ctx).expect("observer should be present");
    let runtime = Runtime::new();
    let compiled = runtime.compile(SIMPLE_EXPORT_WASM).unwrap();
    let module = runtime
        .instantiate(&compiled, ModuleConfig::new().with_name("guest"))
        .unwrap();
    observer.observe_trap(
        &ctx,
        TrapObservation {
            module,
            cause: TrapCause::MemoryFault,
            err: razero::RuntimeError::new("memory fault"),
        },
    );

    assert_eq!(1, observed.load(Ordering::SeqCst));
}

#[test]
fn import_resolver_observer_round_trips_through_public_surface() {
    let observed = Arc::new(AtomicU32::new(0));
    let ctx = with_import_resolver_observer(&Context::default(), {
        let observed = observed.clone();
        move |_ctx: &Context, observation: ImportResolverObservation| {
            assert_eq!(ImportResolverEvent::StoreFallback, observation.event);
            observed.fetch_add(1, Ordering::SeqCst);
        }
    });
    let observer = get_import_resolver_observer(&ctx).expect("observer should be present");
    observer.observe_import_resolution(
        &ctx,
        ImportResolverObservation {
            module_name: "guest".to_string(),
            import_module: "env".to_string(),
            resolved_module: None,
            event: ImportResolverEvent::StoreFallback,
        },
    );

    assert_eq!(1, observed.load(Ordering::SeqCst));
}

#[test]
fn import_resolver_can_return_anonymous_module_instances() {
    let runtime = Runtime::new();
    let call_count = Arc::new(AtomicU32::new(0));

    let compiled_host = runtime
        .new_host_module_builder("env0")
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
    let anonymous_import = runtime
        .instantiate_with_context(
            &Context::default(),
            &compiled_host,
            ModuleConfig::new().with_name(""),
        )
        .unwrap();

    let ctx = with_import_resolver(&Context::default(), move |name| {
        (name == "env").then_some(anonymous_import.clone())
    });

    let resolver = get_import_resolver(&ctx).expect("resolver should be present");
    let first = resolver("env").expect("env should resolve");
    let second = resolver("env").expect("env should resolve again");
    assert!(resolver("other").is_none());

    first.exported_function("start").unwrap().call(&[]).unwrap();
    second
        .exported_function("start")
        .unwrap()
        .call(&[])
        .unwrap();
    assert_eq!(2, call_count.load(Ordering::SeqCst));
}
