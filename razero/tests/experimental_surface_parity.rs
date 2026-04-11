use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc, Mutex,
};

use razero::{
    add_fuel, benchmark_function_listener, get_close_notifier, get_compilation_workers,
    get_fuel_controller, get_fuel_observer, get_function_listener_factory, get_host_call_policy,
    get_host_call_policy_observer, get_import_resolver, get_import_resolver_config,
    get_import_resolver_observer, get_memory_allocator, get_snapshotter, get_time_provider,
    get_trap_observer, get_yield_policy, get_yield_policy_observer, get_yielder,
    new_stack_iterator, remaining_fuel, trap_cause_of, with_close_notifier,
    with_compilation_workers, with_fuel_controller, with_fuel_observer,
    with_function_listener_factory, with_host_call_policy, with_host_call_policy_observer,
    with_import_resolver, with_import_resolver_acl, with_import_resolver_config,
    with_import_resolver_observer, with_memory_allocator, with_snapshotter, with_time_provider,
    with_trap_observer, with_yield_observer, with_yield_policy, with_yield_policy_observer,
    with_yielder, AggregatingFuelController, Context, FuelEvent, FuelObservation,
    FunctionDefinition, FunctionListenerFn, HostCallPolicyDecision, HostCallPolicyObservation,
    HostCallPolicyRequest, ImportACL, ImportResolverConfig, ImportResolverEvent,
    ImportResolverObservation, LinearMemory, MemoryDefinition, ModuleConfig, Runtime,
    RuntimeConfig, RuntimeError, SimpleFuelController, StackFrame, StackIterator, TimeProvider,
    TrapCause, TrapObservation, ValueType, YieldEvent, YieldObservation, YieldPolicyDecision,
    YieldPolicyObservation,
};

const SIMPLE_EXPORT_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, 0x03,
    0x02, 0x01, 0x00, 0x07, 0x05, 0x01, 0x01, b'f', 0x00, 0x00, 0x0a, 0x06, 0x01, 0x04, 0x00, 0x41,
    0x2a, 0x0b,
];
const IMPORTED_GLOBAL_HOST_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x06, 0x06, 0x01, 0x7e, 0x01, 0x42, 0x2a, 0x0b,
    0x07, 0x0b, 0x01, 0x07, b'c', b'o', b'u', b'n', b't', b'e', b'r', 0x03, 0x00,
];
const IMPORTED_GLOBAL_GUEST_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x02, 0x10, 0x01, 0x03, b'e', b'n', b'v', 0x07,
    b'c', b'o', b'u', b'n', b't', b'e', b'r', 0x03, 0x7e, 0x01,
];
const IMPORTED_TABLE_HOST_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x04, 0x05, 0x01, 0x70, 0x01, 0x01, 0x02, 0x07,
    0x09, 0x01, 0x05, b't', b'a', b'b', b'l', b'e', 0x01, 0x00,
];
const IMPORTED_TABLE_GUEST_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x02, 0x10, 0x01, 0x03, b'e', b'n', b'v', 0x05,
    b't', b'a', b'b', b'l', b'e', 0x01, 0x70, 0x01, 0x01, 0x02,
];
const IMPORTED_MEMORY_HOST_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x0a, 0x01,
    0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
];
const IMPORTED_MEMORY_GUEST_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x02, 0x0f,
    0x01, 0x03, b'e', b'n', b'v', 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, 0x01, 0x03,
    0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x04, 0x01, 0x02,
    0x00, 0x0b,
];
const EXPORTED_GLOBAL_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x06, 0x06, 0x01, 0x7e, 0x01, 0x42, 0x2a, 0x0b,
    0x07, 0x13, 0x02, 0x07, b'c', b'o', b'u', b'n', b't', b'e', b'r', 0x03, 0x00, 0x05, b'a', b'l',
    b'i', b'a', b's', 0x03, 0x00,
];
const EXPORTED_MEMORY_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x0a, 0x01,
    0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
];
const EXPORTED_TABLE_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x04, 0x05, 0x01, 0x70, 0x01, 0x01, 0x02, 0x07,
    0x11, 0x02, 0x05, b't', b'a', b'b', b'l', b'e', 0x01, 0x00, 0x05, b'a', b'l', b'i', b'a', b's',
    0x01, 0x00,
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
fn time_provider_round_trips_through_public_surface() {
    struct StubTimeProvider {
        sleeps: Arc<Mutex<Vec<i64>>>,
    }

    impl TimeProvider for StubTimeProvider {
        fn walltime(&self) -> (i64, i32) {
            (1, 2)
        }

        fn nanotime(&self) -> i64 {
            3
        }

        fn nanosleep(&self, ns: i64) {
            self.sleeps.lock().expect("sleep log poisoned").push(ns);
        }
    }

    let sleeps = Arc::new(Mutex::new(Vec::new()));
    let ctx = with_time_provider(
        &Context::default(),
        StubTimeProvider {
            sleeps: sleeps.clone(),
        },
    );
    let provider = get_time_provider(&ctx).expect("time provider should exist");

    assert_eq!((1, 2), provider.walltime());
    assert_eq!(3, provider.nanotime());
    provider.nanosleep(7);
    assert_eq!(vec![7], *sleeps.lock().expect("sleep log poisoned"));
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
fn fuel_observer_public_surface_emits_budgeted_and_consumed_notifications() {
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
        vec![
            (Some("example".to_string()), FuelEvent::Budgeted, 5, 0, 5, 0),
            (Some("example".to_string()), FuelEvent::Consumed, 5, 1, 4, 0),
        ],
        *observations.lock().expect("fuel observations poisoned")
    );
}

#[test]
fn fuel_observer_public_surface_emits_exhausted_notifications() {
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
    let observations = Arc::new(Mutex::new(Vec::new()));
    let ctx = with_fuel_observer(
        &with_fuel_controller(&Context::default(), SimpleFuelController::new(1)),
        {
            let observations = observations.clone();
            move |_ctx: &Context, observation: FuelObservation| {
                observations
                    .lock()
                    .expect("fuel observations poisoned")
                    .push((
                        observation.event,
                        observation.budget,
                        observation.consumed,
                        observation.remaining,
                    ));
            }
        },
    );

    let err = module
        .exported_function("burn")
        .unwrap()
        .call_with_context(&ctx, &[])
        .unwrap_err();

    assert_eq!("fuel exhausted", err.to_string());
    assert_eq!(
        vec![
            (FuelEvent::Budgeted, 1, 0, 1),
            (FuelEvent::Consumed, 1, 3, -2),
            (FuelEvent::Exhausted, 1, 3, -2),
        ],
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
fn module_imported_global_definitions_round_trip_through_public_surface() {
    let runtime = Runtime::new();
    runtime
        .instantiate_binary(
            IMPORTED_GLOBAL_HOST_WASM,
            ModuleConfig::new().with_name("env"),
        )
        .unwrap();

    let guest = runtime
        .instantiate_binary(
            IMPORTED_GLOBAL_GUEST_WASM,
            ModuleConfig::new().with_name("guest"),
        )
        .unwrap();

    let imported = guest.imported_global_definitions();
    assert_eq!(1, imported.len());
    let definition = &imported[0];
    assert_eq!(ValueType::I64, definition.value_type());
    assert!(definition.is_mutable());
    assert_eq!(None, definition.module_name());
    assert_eq!(Some(("env", "counter")), definition.import());
    assert!(definition.export_names().is_empty());
}

#[test]
fn module_imported_table_definitions_round_trip_through_public_surface() {
    let runtime = Runtime::new();
    runtime
        .instantiate_binary(
            IMPORTED_TABLE_HOST_WASM,
            ModuleConfig::new().with_name("env"),
        )
        .unwrap();

    let guest = runtime
        .instantiate_binary(
            IMPORTED_TABLE_GUEST_WASM,
            ModuleConfig::new().with_name("guest"),
        )
        .unwrap();

    let imported = guest.imported_table_definitions();
    assert_eq!(1, imported.len());
    let definition = &imported[0];
    assert_eq!(ValueType::FuncRef, definition.ref_type());
    assert_eq!(1, definition.minimum());
    assert_eq!(Some(2), definition.maximum());
    assert_eq!(None, definition.module_name());
    assert_eq!(Some(("env", "table")), definition.import());
    assert!(definition.export_names().is_empty());
}

#[test]
fn module_exported_global_definitions_round_trip_through_public_surface() {
    let runtime = Runtime::new();
    let guest = runtime
        .instantiate_binary(EXPORTED_GLOBAL_WASM, ModuleConfig::new().with_name("guest"))
        .unwrap();

    let exported = guest.exported_global_definitions();
    assert_eq!(2, exported.len());
    let counter = exported.get("counter").unwrap();
    let alias = exported.get("alias").unwrap();
    assert_eq!(ValueType::I64, counter.value_type());
    assert!(counter.is_mutable());
    assert_eq!(Some("guest"), counter.module_name());
    assert_eq!(None, counter.import());
    assert_eq!(
        &["counter".to_string(), "alias".to_string()],
        counter.export_names()
    );
    assert_eq!(counter, alias);
}

#[test]
fn module_exported_table_definitions_round_trip_through_public_surface() {
    let runtime = Runtime::new();
    let guest = runtime
        .instantiate_binary(EXPORTED_TABLE_WASM, ModuleConfig::new().with_name("guest"))
        .unwrap();

    let exported = guest.exported_table_definitions();
    assert_eq!(2, exported.len());
    let table = exported.get("table").unwrap();
    let alias = exported.get("alias").unwrap();
    assert_eq!(ValueType::FuncRef, table.ref_type());
    assert_eq!(1, table.minimum());
    assert_eq!(Some(2), table.maximum());
    assert_eq!(Some("guest"), table.module_name());
    assert_eq!(None, table.import());
    assert_eq!(
        &["table".to_string(), "alias".to_string()],
        table.export_names()
    );
    assert_eq!(table, alias);
}

#[test]
fn module_imported_function_definitions_round_trip_through_public_surface() {
    let runtime = Runtime::new();
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_ctx, _module, _params| Ok(vec![7]),
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .with_name("hook_impl")
        .with_parameter_names(&["value"])
        .with_result_names(&["result"])
        .export("hook")
        .instantiate(&Context::default())
        .unwrap();

    let guest = runtime
        .instantiate_binary(
            &[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f,
                0x01, 0x7f, 0x02, 0x0c, 0x01, 0x03, b'e', b'n', b'v', 0x04, b'h', b'o', b'o', b'k',
                0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00,
                0x01, 0x0a, 0x08, 0x01, 0x06, 0x00, 0x41, 0x2a, 0x10, 0x00, 0x0b,
            ],
            ModuleConfig::new().with_name("guest"),
        )
        .unwrap();

    let imported = guest.imported_function_definitions();
    assert_eq!(1, imported.len());
    let definition = &imported[0];
    assert_eq!("", definition.name());
    assert_eq!(None, definition.module_name());
    assert_eq!(Some(("env", "hook")), definition.import());
    assert_eq!(&[ValueType::I32], definition.param_types());
    assert_eq!(&[ValueType::I32], definition.result_types());
}

#[test]
fn module_imported_memory_definitions_round_trip_through_public_surface() {
    let runtime = Runtime::new();
    runtime
        .instantiate_binary(
            IMPORTED_MEMORY_HOST_WASM,
            ModuleConfig::new().with_name("env"),
        )
        .unwrap();

    let guest = runtime
        .instantiate_binary(
            IMPORTED_MEMORY_GUEST_WASM,
            ModuleConfig::new().with_name("guest"),
        )
        .unwrap();

    let imported = guest.imported_memory_definitions();
    assert_eq!(1, imported.len());
    let definition = &imported[0];
    assert_eq!(None, definition.module_name());
    assert_eq!(Some(("env", "memory")), definition.import());
    assert_eq!(1, definition.minimum_pages());
    assert_eq!(None, definition.maximum_pages());
}

#[test]
fn module_exported_memory_definitions_round_trip_through_public_surface() {
    let runtime = Runtime::new();
    let guest = runtime
        .instantiate_binary(EXPORTED_MEMORY_WASM, ModuleConfig::new().with_name("guest"))
        .unwrap();

    let exported = guest.exported_memory_definitions();
    assert_eq!(1, exported.len());
    let definition = exported.get("memory").unwrap();
    assert_eq!(None, definition.module_name());
    assert_eq!(1, definition.minimum_pages());
    assert_eq!(None, definition.maximum_pages());
    assert_eq!(None, definition.import());
    assert_eq!(&["memory".to_string()], definition.export_names());
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
fn linear_memory_len_tracks_allocation_size() {
    let memory = LinearMemory::new(42, 100);
    assert_eq!(42, memory.len());

    let empty = LinearMemory::new(0, 100);
    assert_eq!(0, empty.len());
}

#[test]
fn linear_memory_bytes_returns_allocation_view() {
    let memory = LinearMemory::new(16, 64);
    let bytes = memory.bytes();
    assert_eq!(16, bytes.len());
    assert_eq!(0, bytes[0]);

    let empty = LinearMemory::new(0, 64);
    assert!(empty.bytes().is_empty());
}

#[test]
fn linear_memory_bytes_mut_allows_mutation_of_allocation() {
    let mut memory = LinearMemory::new(16, 64);
    let bytes = memory.bytes_mut();
    bytes[0] = 42;

    assert_eq!(42, memory.bytes()[0]);

    let mut empty = LinearMemory::new(0, 64);
    assert!(empty.bytes_mut().is_empty());
}

#[test]
fn linear_memory_free_clears_public_surface_state() {
    let mut memory = LinearMemory::new(16, 64);
    memory.bytes_mut()[0] = 42;
    memory.bytes_mut()[1] = 99;

    memory.free();

    assert!(memory.is_empty());
    assert_eq!(0, memory.len());
    assert!(memory.bytes().is_empty());
}

#[test]
fn linear_memory_reallocate_resizes_within_max_bound() {
    let mut memory = LinearMemory::new(8, 32);
    memory.bytes_mut()[0] = 42;
    memory.bytes_mut()[3] = 99;

    {
        let grown = memory
            .reallocate(16)
            .expect("growth within max should succeed");
        assert_eq!(16, grown.len());
        assert_eq!(42, grown[0]);
        assert_eq!(99, grown[3]);
        assert!(grown[8..].iter().all(|byte| *byte == 0));
    }
    assert_eq!(16, memory.len());

    {
        let shrunk = memory
            .reallocate(4)
            .expect("shrink within max should succeed");
        assert_eq!(4, shrunk.len());
        assert_eq!(42, shrunk[0]);
        assert_eq!(99, shrunk[3]);
    }
    assert_eq!(4, memory.len());

    assert!(memory.reallocate(64).is_none());
    assert_eq!(4, memory.len());
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
fn host_call_policy_request_defaults_to_empty_metadata_through_public_surface() {
    let request = HostCallPolicyRequest::new();

    assert_eq!(None, request.param_types());
    assert_eq!(None, request.result_types());
    assert_eq!(None, request.param_names());
    assert_eq!(None, request.result_names());
    assert_eq!(None, request.caller_module_name());
    assert_eq!(0, request.param_count());
    assert_eq!(0, request.result_count());
    assert_eq!(None, request.import());
    assert_eq!(None, request.memory());
    assert_eq!(None, request.module_name());
    assert_eq!(None, request.name());
    assert!(request.export_names().is_empty());
}

#[test]
fn host_call_policy_request_memory_metadata_round_trip_through_public_surface() {
    let memory = MemoryDefinition::new(1, Some(2))
        .with_module_name(Some("env".to_string()))
        .with_import("env", "memory")
        .with_export_name("memory");
    let request = HostCallPolicyRequest::new().with_memory(memory.clone());

    assert_eq!(Some(&memory), request.memory());
    assert_eq!(
        Some("env"),
        request.memory().and_then(MemoryDefinition::module_name)
    );
    assert_eq!(
        Some(("env", "memory")),
        request.memory().and_then(MemoryDefinition::import)
    );
    assert_eq!(
        Some(1),
        request.memory().map(MemoryDefinition::minimum_pages)
    );
    assert_eq!(
        Some(Some(2)),
        request.memory().map(MemoryDefinition::maximum_pages)
    );
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
fn yield_policy_request_builders_round_trip_through_public_surface() {
    let function = FunctionDefinition::new("test_fn")
        .with_module_name(Some("guest".to_string()))
        .with_export_name("test")
        .with_signature(vec![ValueType::I32], vec![ValueType::I64])
        .with_import("env", "call");
    let memory = MemoryDefinition::new(1, Some(2))
        .with_module_name(Some("guest".to_string()))
        .with_export_name("memory")
        .with_import("env", "memory");
    let request = razero::YieldPolicyRequest::new()
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
fn yield_policy_request_param_and_result_types_round_trip_through_public_surface() {
    let function = FunctionDefinition::new("test")
        .with_signature(vec![ValueType::I32, ValueType::I64], vec![ValueType::F64]);
    let request = razero::YieldPolicyRequest::new().with_function(function);

    assert_eq!(
        Some(&[ValueType::I32, ValueType::I64][..]),
        request.param_types()
    );
    assert_eq!(Some(&[ValueType::F64][..]), request.result_types());

    let empty_request = razero::YieldPolicyRequest::new();
    assert_eq!(None, empty_request.param_types());
    assert_eq!(None, empty_request.result_types());
}

#[test]
fn yield_policy_request_param_and_result_names_round_trip_through_public_surface() {
    let function = FunctionDefinition::new("test")
        .with_parameter_names(vec!["a".to_string(), "b".to_string()])
        .with_result_names(vec!["result".to_string()]);
    let request = razero::YieldPolicyRequest::new().with_function(function);

    assert_eq!(
        Some(&["a".to_string(), "b".to_string()][..]),
        request.param_names()
    );
    assert_eq!(Some(&["result".to_string()][..]), request.result_names());

    let empty_request = razero::YieldPolicyRequest::new();
    assert_eq!(None, empty_request.param_names());
    assert_eq!(None, empty_request.result_names());
}

#[test]
fn yield_policy_request_export_names_round_trip_through_public_surface() {
    let function = FunctionDefinition::new("test")
        .with_export_name("export1")
        .with_export_name("export2");
    let request = razero::YieldPolicyRequest::new().with_function(function);

    assert_eq!(
        &["export1".to_string(), "export2".to_string()][..],
        request.export_names()
    );

    let empty_request = razero::YieldPolicyRequest::new();
    assert!(empty_request.export_names().is_empty());
}

#[test]
fn yield_policy_request_defaults_to_empty_metadata_through_public_surface() {
    let request = razero::YieldPolicyRequest::new();

    assert_eq!(None, request.param_types());
    assert_eq!(None, request.result_types());
    assert_eq!(None, request.param_names());
    assert_eq!(None, request.result_names());
    assert_eq!(None, request.caller_module_name());
    assert_eq!(0, request.param_count());
    assert_eq!(0, request.result_count());
    assert_eq!(None, request.import());
    assert_eq!(None, request.memory());
    assert_eq!(None, request.module_name());
    assert_eq!(None, request.name());
    assert!(request.export_names().is_empty());
}

#[test]
fn yield_policy_request_memory_metadata_round_trip_through_public_surface() {
    let memory = MemoryDefinition::new(1, Some(2))
        .with_module_name(Some("env".to_string()))
        .with_import("env", "memory")
        .with_export_name("memory");
    let request = razero::YieldPolicyRequest::new().with_memory(memory.clone());

    assert_eq!(Some(&memory), request.memory());
    assert_eq!(
        Some("env"),
        request.memory().and_then(MemoryDefinition::module_name)
    );
    assert_eq!(
        Some(("env", "memory")),
        request.memory().and_then(MemoryDefinition::import)
    );
    assert_eq!(
        Some(1),
        request.memory().map(MemoryDefinition::minimum_pages)
    );
    assert_eq!(
        Some(Some(2)),
        request.memory().map(MemoryDefinition::maximum_pages)
    );
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
