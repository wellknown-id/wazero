use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc, Mutex,
};

use razero::{
    benchmark_function_listener, get_close_notifier, get_compilation_workers, get_fuel_controller,
    get_fuel_observer, get_function_listener_factory, get_host_call_policy,
    get_host_call_policy_observer, get_import_resolver, get_import_resolver_config,
    get_import_resolver_observer, get_memory_allocator, get_snapshotter, get_trap_observer,
    get_yield_policy, get_yield_policy_observer, get_yielder, new_stack_iterator, trap_cause_of,
    with_close_notifier, with_compilation_workers, with_fuel_controller, with_fuel_observer,
    with_function_listener_factory, with_host_call_policy, with_host_call_policy_observer,
    with_import_resolver, with_import_resolver_acl, with_import_resolver_config,
    with_import_resolver_observer, with_memory_allocator, with_snapshotter, with_trap_observer,
    with_yield_observer, with_yield_policy, with_yield_policy_observer, with_yielder,
    AggregatingFuelController, Context, FuelEvent, FuelObservation, FunctionDefinition,
    FunctionListenerFn, HostCallPolicyDecision, HostCallPolicyObservation, HostCallPolicyRequest,
    ImportACL, ImportResolverConfig, ImportResolverEvent, ImportResolverObservation, LinearMemory,
    MemoryDefinition, ModuleConfig, Runtime, RuntimeConfig, RuntimeError, SimpleFuelController,
    StackFrame, StackIterator, TrapCause, TrapObservation, ValueType, YieldEvent, YieldObservation,
    YieldPolicyDecision, YieldPolicyObservation,
};

const SIMPLE_EXPORT_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, 0x03,
    0x02, 0x01, 0x00, 0x07, 0x05, 0x01, 0x01, b'f', 0x00, 0x00, 0x0a, 0x06, 0x01, 0x04, 0x00, 0x41,
    0x2a, 0x0b,
];
const YIELD_WASM: &[u8] = include_bytes!("../../experimental/testdata/yield.wasm");

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
fn close_notifier_round_trips_through_public_surface() {
    let exit_code = Arc::new(AtomicU32::new(0));
    let ctx = with_close_notifier(&Context::default(), {
        let exit_code = exit_code.clone();
        move |_ctx: &Context, code: u32| {
            exit_code.store(code, Ordering::SeqCst);
        }
    });
    let notifier = get_close_notifier(&ctx).expect("notifier should be present");
    notifier.close_notify(&ctx, 42);

    assert_eq!(42, exit_code.load(Ordering::SeqCst));
}

#[test]
fn snapshotter_public_surface_enables_runtime_injection() {
    let runtime = Runtime::new();
    let module = runtime
        .new_host_module_builder("example")
        .new_function_builder()
        .with_callback(
            move |ctx, _module, _params| {
                assert!(get_snapshotter(&ctx).is_some());
                Ok(vec![7])
            },
            &[],
            &[ValueType::I32],
        )
        .export("snapshot")
        .instantiate(&Context::default())
        .unwrap();

    assert!(get_snapshotter(&Context::default()).is_none());
    assert!(get_snapshotter(&with_snapshotter(&Context::default())).is_none());
    assert_eq!(
        vec![7],
        module
            .exported_function("snapshot")
            .unwrap()
            .call_with_context(&with_snapshotter(&Context::default()), &[])
            .unwrap()
    );
}

#[test]
fn fuel_controller_round_trips_through_public_surface() {
    let ctx = with_fuel_controller(&Context::default(), SimpleFuelController::new(42));
    let controller = get_fuel_controller(&ctx).expect("controller should be present");

    assert_eq!(42, controller.budget());
}

#[test]
fn simple_fuel_controller_total_consumed_round_trips_through_public_surface() {
    let controller = SimpleFuelController::new(1_000);
    assert_eq!(0, controller.total_consumed());

    razero::FuelController::consumed(&controller, 50);
    assert_eq!(50, controller.total_consumed());

    razero::FuelController::consumed(&controller, 75);
    assert_eq!(125, controller.total_consumed());

    razero::FuelController::consumed(&controller, 25);
    assert_eq!(150, controller.total_consumed());
}

#[test]
fn aggregating_fuel_controller_total_consumed_round_trips_through_public_surface() {
    let controller = AggregatingFuelController::new(None, 1_000);
    assert_eq!(0, controller.total_consumed());

    razero::FuelController::consumed(&controller, 100);
    assert_eq!(100, controller.total_consumed());
}

#[test]
fn fuel_observer_public_surface_emits_budgeted_notifications() {
    let runtime = Runtime::new();
    let module = runtime
        .new_host_module_builder("example")
        .new_function_builder()
        .with_func(|_ctx, _module, _params| Ok(vec![7]), &[], &[ValueType::I32])
        .with_name("work")
        .export("work")
        .instantiate(&Context::default())
        .unwrap();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let ctx = with_fuel_observer(
        &with_fuel_controller(&Context::default(), SimpleFuelController::new(5)),
        {
            let observations = observations.clone();
            move |_ctx: &Context, observation: FuelObservation| {
                observations
                    .lock()
                    .expect("fuel observations poisoned")
                    .push((
                        observation.module.name().map(str::to_string),
                        observation.event,
                        observation.budget,
                        observation.consumed,
                        observation.remaining,
                        observation.delta,
                    ));
            }
        },
    );

    assert!(get_fuel_observer(&Context::default()).is_none());
    assert!(get_fuel_observer(&ctx).is_some());
    assert_eq!(
        vec![7],
        module
            .exported_function("work")
            .unwrap()
            .call_with_context(&ctx, &[])
            .unwrap()
    );
    assert_eq!(
        vec![(Some("example".to_string()), FuelEvent::Budgeted, 5, 0, 5, 0)],
        *observations.lock().expect("fuel observations poisoned")
    );
}

#[test]
fn yielder_public_surface_enables_runtime_injection() {
    let runtime = Runtime::new();
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

    assert!(get_yielder(&Context::default()).is_none());
    assert!(get_yielder(&with_yielder(&Context::default())).is_none());
    assert!(guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&with_yielder(&Context::default()), &[])
        .unwrap_err()
        .to_string()
        .contains("yielded"));
}

#[test]
fn yield_error_resumer_round_trips_through_public_surface() {
    struct TestResumer;

    impl razero::Resumer for TestResumer {
        fn resume(&self, _ctx: &Context, host_results: &[u64]) -> razero::Result<Vec<u64>> {
            Ok(host_results.to_vec())
        }

        fn cancel(&self) {}
    }

    let err_none = razero::YieldError::new(None);
    assert!(err_none.resumer().is_none());

    let resumer: Arc<dyn razero::Resumer> = Arc::new(TestResumer);
    let err = razero::YieldError::new(Some(resumer.clone()));
    assert!(Arc::ptr_eq(
        &resumer,
        &err.resumer().expect("resumer should be present")
    ));
}

#[test]
fn memory_allocator_round_trips_through_public_surface() {
    let ctx = with_memory_allocator(&Context::default(), |cap, max| {
        Some(LinearMemory::new(cap, max))
    });
    let allocator = get_memory_allocator(&ctx).expect("allocator should be present");
    let memory = allocator
        .allocate(8, 16)
        .expect("allocation should succeed");

    assert_eq!(8, memory.len());
}

#[test]
fn linear_memory_is_guard_page_backed_reflects_allocation_type() {
    let plain = LinearMemory::new(1024, 2048);
    assert!(!plain.is_guard_page_backed());

    #[cfg(target_os = "linux")]
    {
        use razero_secmem::GuardPageAllocator;

        let guarded_allocation = GuardPageAllocator
            .allocate_zeroed(1024)
            .expect("guard page allocation should succeed");
        let guarded = LinearMemory::from_guarded(guarded_allocation, 512, 1024);
        assert!(guarded.is_guard_page_backed());
    }
}

#[test]
fn memory_is_guard_page_backed_reflects_instance_memory_backing() {
    let definition = MemoryDefinition::new(1, Some(2));

    let plain = razero::Memory::new(definition.clone(), LinearMemory::new(1024, 2048));
    assert!(!plain.is_guard_page_backed());

    #[cfg(target_os = "linux")]
    {
        use razero_secmem::GuardPageAllocator;

        let guarded_allocation = GuardPageAllocator
            .allocate_zeroed(1024)
            .expect("guard page allocation should succeed");
        let guarded = LinearMemory::from_guarded(guarded_allocation, 512, 1024);
        let guarded = razero::Memory::new(definition, guarded);
        assert!(guarded.is_guard_page_backed());
    }
}

#[test]
fn linear_memory_is_empty_tracks_length() {
    let mut memory = LinearMemory::new(8, 16);
    assert!(!memory.is_empty());

    memory.free();
    assert!(memory.is_empty());
    assert_eq!(0, memory.len());
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
fn host_call_policy_request_builders_round_trip_through_public_surface() {
    let function = FunctionDefinition::new("test_fn")
        .with_module_name(Some("guest".to_string()))
        .with_export_name("test")
        .with_signature(vec![ValueType::I32], vec![ValueType::I64])
        .with_import("env", "call");
    let memory = MemoryDefinition::new(1, Some(2))
        .with_module_name(Some("guest".to_string()))
        .with_export_name("memory")
        .with_import("env", "memory");
    let request = HostCallPolicyRequest::new()
        .with_function(function)
        .with_memory(memory.clone())
        .with_caller_module_name("caller");

    assert_eq!(Some("caller"), request.caller_module_name());
    assert_eq!(Some(&memory), request.memory());
    assert_eq!(Some("test_fn"), request.name());
    assert_eq!(Some("guest"), request.module_name());
    assert_eq!(Some(("env", "call")), request.import());
    assert_eq!(1, request.param_count());
    assert_eq!(1, request.result_count());
}

#[test]
fn host_call_policy_request_param_and_result_types_round_trip_through_public_surface() {
    let function = FunctionDefinition::new("test")
        .with_signature(vec![ValueType::I32, ValueType::I64], vec![ValueType::F64]);
    let request = HostCallPolicyRequest::new().with_function(function);

    assert_eq!(
        Some(&[ValueType::I32, ValueType::I64][..]),
        request.param_types()
    );
    assert_eq!(Some(&[ValueType::F64][..]), request.result_types());

    let empty_request = HostCallPolicyRequest::new();
    assert_eq!(None, empty_request.param_types());
    assert_eq!(None, empty_request.result_types());
}

#[test]
fn host_call_policy_request_param_and_result_names_round_trip_through_public_surface() {
    let function = FunctionDefinition::new("test")
        .with_parameter_names(vec!["a".to_string(), "b".to_string()])
        .with_result_names(vec!["result".to_string()]);
    let request = HostCallPolicyRequest::new().with_function(function);

    assert_eq!(
        Some(&["a".to_string(), "b".to_string()][..]),
        request.param_names()
    );
    assert_eq!(Some(&["result".to_string()][..]), request.result_names());

    let empty_request = HostCallPolicyRequest::new();
    assert_eq!(None, empty_request.param_names());
    assert_eq!(None, empty_request.result_names());
}

#[test]
fn host_call_policy_request_export_names_round_trip_through_public_surface() {
    let function = FunctionDefinition::new("test")
        .with_export_name("export1")
        .with_export_name("export2");
    let request = HostCallPolicyRequest::new().with_function(function);

    assert_eq!(
        &["export1".to_string(), "export2".to_string()][..],
        request.export_names()
    );

    let empty_request = HostCallPolicyRequest::new();
    assert!(empty_request.export_names().is_empty());
}

#[test]
fn function_listener_factory_round_trips_through_public_surface() {
    let ctx = with_function_listener_factory(
        &Context::default(),
        |_definition: &razero::FunctionDefinition| {
            Some(Arc::new(
                |_ctx: &Context,
                 _module: &razero::Module,
                 _definition: &razero::FunctionDefinition,
                 _params: &[u64],
                 _stack: &mut dyn razero::StackIterator| {},
            ) as Arc<dyn razero::FunctionListener>)
        },
    );
    let factory = get_function_listener_factory(&ctx).expect("factory should be present");

    assert!(factory
        .new_listener(&razero::FunctionDefinition::new("demo"))
        .is_some());
}

#[test]
fn benchmark_function_listener_runs_through_public_surface() {
    let runtime = Runtime::new();
    let module = runtime
        .instantiate_binary(SIMPLE_EXPORT_WASM, ModuleConfig::new())
        .unwrap();
    let calls = Arc::new(AtomicU32::new(0));
    let listener = FunctionListenerFn::new({
        let calls = calls.clone();
        move |_ctx: &Context,
              _module: &razero::Module,
              definition: &FunctionDefinition,
              _params: &[u64],
              stack: &mut dyn razero::StackIterator| {
            assert_eq!("f", definition.name());
            while stack.next() {}
            calls.fetch_add(1, Ordering::SeqCst);
        }
    });
    let stack = [StackFrame::new(
        FunctionDefinition::new("f"),
        vec![1],
        vec![2],
        3,
        5,
    )];

    benchmark_function_listener(2, &module, &stack, &listener);

    assert_eq!(2, calls.load(Ordering::SeqCst));
}

#[test]
fn new_stack_iterator_round_trips_through_public_surface() {
    let frame = StackFrame::new(
        FunctionDefinition::new("test_fn"),
        vec![1, 2, 3],
        vec![4, 5],
        42,
        99,
    );
    let mut iterator = new_stack_iterator(std::slice::from_ref(&frame));

    assert_eq!("test_fn", frame.definition().name());
    assert_eq!(&[1, 2, 3], frame.params());
    assert_eq!(&[4, 5], frame.results());
    assert_eq!(42, frame.program_counter());
    assert_eq!(99, frame.source_offset());
    assert!(iterator.next());
    assert!(!iterator.next());
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
fn yield_observer_round_trips_through_public_surface() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let runtime = Runtime::new();
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
        .instantiate_binary(YIELD_WASM, ModuleConfig::new().with_name("guest"))
        .unwrap();

    let initial_ctx = with_yield_observer(&with_yielder(&Context::default()), {
        let events = events.clone();
        move |_ctx: &Context, observation: YieldObservation| {
            events.lock().expect("events poisoned").push((
                observation.module.name().map(str::to_string),
                observation.event,
                observation.yield_count,
                observation.expected_host_results,
            ));
        }
    });
    let err = guest
        .exported_function("run")
        .unwrap()
        .call_with_context(&initial_ctx, &[])
        .unwrap_err();
    let yield_error = match err {
        RuntimeError::Yield(yield_error) => yield_error,
        other => panic!("expected yield error, got {other}"),
    };

    let resume_ctx = with_yield_observer(&with_yielder(&Context::default()), {
        let events = events.clone();
        move |_ctx: &Context, observation: YieldObservation| {
            events.lock().expect("events poisoned").push((
                observation.module.name().map(str::to_string),
                observation.event,
                observation.yield_count,
                observation.expected_host_results,
            ));
        }
    });
    let results = yield_error
        .resumer()
        .expect("resumer should be present")
        .resume(&resume_ctx, &[42])
        .unwrap();

    assert_eq!(vec![142], results);
    assert_eq!(
        vec![
            (Some("guest".to_string()), YieldEvent::Yielded, 1, 1),
            (Some("guest".to_string()), YieldEvent::Resumed, 1, 1),
        ],
        *events.lock().expect("events poisoned")
    );
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
fn trap_cause_of_round_trips_through_public_surface() {
    assert_eq!(
        Some(TrapCause::OutOfBoundsMemoryAccess),
        trap_cause_of(&razero::RuntimeError::new("out of bounds memory access"))
    );
    assert_eq!(
        Some(TrapCause::FuelExhausted),
        trap_cause_of(&razero::RuntimeError::new("fuel exhausted"))
    );
    assert_eq!(
        Some(TrapCause::PolicyDenied),
        trap_cause_of(&razero::RuntimeError::new("policy denied"))
    );
    assert_eq!(
        None,
        trap_cause_of(&razero::RuntimeError::new("some other error"))
    );
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

#[test]
fn import_resolver_config_round_trips_through_public_surface() {
    let acl = ImportACL::new().deny_modules(["env"]);
    let ctx = with_import_resolver_config(
        &Context::default(),
        ImportResolverConfig {
            acl: Some(acl.clone()),
            ..ImportResolverConfig::default()
        },
    );
    let cfg = get_import_resolver_config(&ctx).expect("config should be present");

    assert_eq!(Some(acl), cfg.acl);
    assert!(cfg.resolver.is_none());
}

#[test]
fn with_import_resolver_acl_preserves_existing_resolver() {
    let ctx = with_import_resolver(&Context::default(), |_name| None);
    let ctx = with_import_resolver_acl(&ctx, ImportACL::new().allow_modules(["env"]));
    let cfg = get_import_resolver_config(&ctx).expect("config should be present");

    assert!(cfg.resolver.is_some());
    assert_eq!(Some(ImportACL::new().allow_modules(["env"])), cfg.acl);
}
