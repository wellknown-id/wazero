use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
};

#[cfg(feature = "filecache")]
use razero::FileCompilationCache;
use razero::{api::wasm::ValueType, Context, ModuleConfig, Runtime, RuntimeConfig};

const CONCURRENT_ADD_WASM: &[u8] =
    include_bytes!("../../examples/concurrent-instantiation/testdata/add.wasm");
const AGE_CALCULATOR_WASM: &[u8] =
    include_bytes!("../../examples/import-go/testdata/age_calculator.wasm");
const HELLO_WORLD_WASM: &[u8] =
    include_bytes!("../../examples/hello-host/testdata/hello_world.wasm");
const RESULT_OFFSET_WASM: &[u8] =
    include_bytes!("../../examples/multiple-results/testdata/result_offset.wasm");
const MULTI_VALUE_WASM: &[u8] =
    include_bytes!("../../examples/multiple-results/testdata/multi_value.wasm");
const MULTI_VALUE_IMPORTED_WASM: &[u8] =
    include_bytes!("../../examples/multiple-results/testdata/multi_value_imported.wasm");
const COUNTER_WASM: &[u8] =
    include_bytes!("../../examples/multiple-runtimes/testdata/counter.wasm");
const RUST_GREET_WASM: &[u8] = include_bytes!("../../examples/allocation/rust/testdata/greet.wasm");
const ZIG_GREET_WASM: &[u8] = include_bytes!("../../examples/allocation/zig/testdata/greet.wasm");
const FAC_WASM: &[u8] = include_bytes!("../../testdata/fac.wasm");

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn concurrent_instantiation_smoke_matches_go_example() {
    let runtime = Runtime::new();
    let compiled = runtime.compile(CONCURRENT_ADD_WASM).unwrap();

    let mut handles = Vec::new();
    for i in 0..50_u64 {
        let runtime = runtime.clone();
        let compiled = compiled.clone();
        handles.push(thread::spawn(move || {
            let module = runtime
                .instantiate(&compiled, ModuleConfig::new().with_name(""))
                .unwrap();
            let result = module
                .exported_function("add")
                .unwrap()
                .call(&[i, i])
                .unwrap();
            assert_eq!(vec![i * 2], result);
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn import_go_age_calculator_smoke_matches_go_example() {
    let runtime = Runtime::new();
    let lines = Arc::new(Mutex::new(Vec::new()));

    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            {
                let lines = lines.clone();
                move |_ctx, _module, params| {
                    lines
                        .lock()
                        .unwrap()
                        .push(format!("log_i32 >> {}", params[0] as u32));
                    Ok(Vec::new())
                }
            },
            &[ValueType::I32],
            &[],
        )
        .export("log_i32")
        .new_function_builder()
        .with_func(
            |_ctx, _module, _params| Ok(vec![2021]),
            &[],
            &[ValueType::I32],
        )
        .export("current_year")
        .instantiate(&Context::default())
        .unwrap();

    let module = runtime
        .instantiate_binary(AGE_CALCULATOR_WASM, ModuleConfig::new())
        .unwrap();

    let age = module
        .exported_function("get_age")
        .unwrap()
        .call(&[2000])
        .unwrap();
    assert_eq!(vec![21], age);
    lines
        .lock()
        .unwrap()
        .insert(0, format!("println >> {}", age[0] as u32));

    module
        .exported_function("log_age")
        .unwrap()
        .call(&[2000])
        .unwrap();

    assert_eq!(
        vec!["println >> 21".to_string(), "log_i32 >> 21".to_string()],
        *lines.lock().unwrap()
    );
}

#[test]
fn hello_host_smoke_matches_rust_example() {
    let runtime = Runtime::new();
    let lines = Arc::new(Mutex::new(Vec::new()));

    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            {
                let lines = lines.clone();
                move |_ctx, module, params| {
                    let memory = module.memory().expect("guest should export memory");
                    let message = String::from_utf8(
                        memory
                            .read(params[0] as usize, params[1] as usize)
                            .expect("guest string should be in bounds"),
                    )
                    .expect("guest string should be utf-8");
                    lines.lock().unwrap().push(message);
                    Ok(Vec::new())
                }
            },
            &[ValueType::I32, ValueType::I32],
            &[],
        )
        .export("print")
        .instantiate(&Context::default())
        .unwrap();

    let module = runtime
        .instantiate_binary(HELLO_WORLD_WASM, ModuleConfig::new())
        .unwrap();
    module.exported_function("run").unwrap().call(&[]).unwrap();

    assert_eq!(
        vec!["hello world from guest".to_string()],
        *lines.lock().unwrap()
    );
}

#[test]
fn multiple_results_smoke_matches_go_example() {
    let runtime = Runtime::new();
    let result_offset = runtime
        .instantiate_binary(RESULT_OFFSET_WASM, ModuleConfig::new())
        .unwrap();
    let multi_value = runtime
        .instantiate_binary(MULTI_VALUE_WASM, ModuleConfig::new())
        .unwrap();

    runtime
        .new_host_module_builder("multi-value/host")
        .new_function_builder()
        .with_func(
            |_ctx, _module, _params| Ok(vec![37, 0]),
            &[],
            &[ValueType::I64, ValueType::I32],
        )
        .export("get_age")
        .instantiate(&Context::default())
        .unwrap();
    let imported_host = runtime
        .instantiate_binary(MULTI_VALUE_IMPORTED_WASM, ModuleConfig::new())
        .unwrap();

    let outputs = vec![
        (
            result_offset.name().unwrap(),
            result_offset
                .exported_function("call_get_age")
                .unwrap()
                .call(&[])
                .unwrap()[0],
        ),
        (
            multi_value.name().unwrap(),
            multi_value
                .exported_function("call_get_age")
                .unwrap()
                .call(&[])
                .unwrap()[0],
        ),
        (
            imported_host.name().unwrap(),
            imported_host
                .exported_function("call_get_age")
                .unwrap()
                .call(&[])
                .unwrap()[0],
        ),
    ];

    assert_eq!(
        vec![
            ("result-offset/wasm", 37),
            ("multi-value/wasm", 37),
            ("multi-value/imported_host", 37),
        ],
        outputs
    );
}

#[cfg(feature = "filecache")]
#[test]
fn multiple_runtimes_smoke_matches_go_example() {
    let scratch = ScratchDir::new("example-multiple-runtimes");
    let cache = Arc::new(FileCompilationCache::new(scratch.path()));
    let config = RuntimeConfig::new().with_compilation_cache(cache);
    let runtime_a = Runtime::with_config(config.clone());
    let runtime_b = Runtime::with_config(config);

    let module_a = instantiate_counter_module(&runtime_a);
    let module_b = instantiate_counter_module(&runtime_b);

    let observed = vec![
        counter_value(&module_a),
        counter_value(&module_b),
        counter_value(&module_a),
        counter_value(&module_b),
    ];
    assert_eq!(vec![0, 0, 1, 1], observed);
    assert!(
        std::fs::read_dir(scratch.path())
            .unwrap()
            .any(|entry| entry.unwrap().path().is_file()),
        "shared compilation cache should materialize on disk"
    );
}

#[test]
fn allocation_rust_smoke_matches_go_example() {
    assert_eq!(
        vec![
            "wasm >> Hello, wazero!".to_string(),
            "go >> Hello, wazero!".to_string(),
        ],
        run_greet_example(RUST_GREET_WASM, "allocate", Some("deallocate"), true)
    );
}

#[test]
fn allocation_zig_smoke_matches_go_example() {
    assert_eq!(
        vec![
            "wasm >> Hello, wazero!".to_string(),
            "go >> Hello, wazero!".to_string(),
        ],
        run_greet_example(ZIG_GREET_WASM, "malloc", Some("free"), false)
    );
}

#[test]
fn basic_addition_smoke_covers_the_unverified_basic_example_surface() {
    let runtime = Runtime::new();
    let module = runtime
        .instantiate_binary(CONCURRENT_ADD_WASM, ModuleConfig::new().with_name(""))
        .unwrap();
    let result = module
        .exported_function("add")
        .unwrap()
        .call(&[7, 9])
        .unwrap();
    assert_eq!(vec![16], result);
}

#[test]
fn fac_ssa_executes_for_secbench_workload() {
    for secure_mode in [false, true] {
        let runtime = Runtime::with_config(RuntimeConfig::new().with_secure_mode(secure_mode));
        let module = runtime
            .instantiate_binary(FAC_WASM, ModuleConfig::new())
            .unwrap();
        let result = module
            .exported_function("fac-ssa")
            .unwrap()
            .call(&[20])
            .unwrap();
        assert_eq!(vec![2_432_902_008_176_640_000], result);
    }
}

fn instantiate_counter_module(runtime: &Runtime) -> razero::Module {
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            {
                let counter = Arc::new(Mutex::new(0_u32));
                move |_ctx, _module, _params| {
                    let mut counter = counter.lock().unwrap();
                    let value = *counter;
                    *counter += 1;
                    Ok(vec![value as u64])
                }
            },
            &[],
            &[ValueType::I32],
        )
        .export("next_i32")
        .instantiate(&Context::default())
        .unwrap();

    runtime
        .instantiate_binary(COUNTER_WASM, ModuleConfig::new().with_name(""))
        .unwrap()
}

fn counter_value(module: &razero::Module) -> u64 {
    module.exported_function("get").unwrap().call(&[]).unwrap()[0]
}

fn run_greet_example(
    wasm: &[u8],
    allocate_export: &str,
    deallocate_export: Option<&str>,
    free_greeting: bool,
) -> Vec<String> {
    let runtime = Runtime::new();
    let lines = Arc::new(Mutex::new(Vec::new()));
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            {
                let lines = lines.clone();
                move |_ctx, module, params| {
                    let memory = module.memory().expect("guest memory should exist");
                    let message = String::from_utf8(
                        memory
                            .read(params[0] as usize, params[1] as usize)
                            .expect("host log reads guest memory"),
                    )
                    .unwrap();
                    lines.lock().unwrap().push(message);
                    Ok(Vec::new())
                }
            },
            &[ValueType::I32, ValueType::I32],
            &[],
        )
        .export("log")
        .instantiate(&Context::default())
        .unwrap();

    let module = runtime
        .instantiate_binary(wasm, ModuleConfig::new())
        .unwrap();
    let memory = module.exported_memory("memory").unwrap();
    let input = b"wazero";
    let name_ptr = module
        .exported_function(allocate_export)
        .unwrap()
        .call(&[input.len() as u64])
        .unwrap()[0];
    assert!(memory.write(name_ptr as usize, input));

    module
        .exported_function("greet")
        .unwrap()
        .call(&[name_ptr, input.len() as u64])
        .unwrap();

    let ptr_size = module
        .exported_function("greeting")
        .unwrap()
        .call(&[name_ptr, input.len() as u64])
        .unwrap()[0];
    let greeting_ptr = (ptr_size >> 32) as usize;
    let greeting_len = (ptr_size as u32) as usize;
    let greeting = String::from_utf8(memory.read(greeting_ptr, greeting_len).unwrap()).unwrap();
    lines.lock().unwrap().push(format!("go >> {greeting}"));

    if let Some(deallocate_export) = deallocate_export {
        module
            .exported_function(deallocate_export)
            .unwrap()
            .call(&[name_ptr, input.len() as u64])
            .unwrap();
        if free_greeting {
            module
                .exported_function(deallocate_export)
                .unwrap()
                .call(&[greeting_ptr as u64, greeting_len as u64])
                .unwrap();
        }
    }

    let output = lines.lock().unwrap().clone();
    output
}

#[test]
fn allocation_rust_greeting_returns_in_bounds_pointer_len() {
    let runtime = Runtime::new();
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_, _, _| Ok(Vec::new()),
            &[ValueType::I32, ValueType::I32],
            &[],
        )
        .export("log")
        .instantiate(&Context::default())
        .unwrap();

    let module = runtime
        .instantiate_binary(RUST_GREET_WASM, ModuleConfig::new())
        .unwrap();
    let memory = module.exported_memory("memory").unwrap();
    let input = b"wazero";
    let name_ptr = module
        .exported_function("allocate")
        .unwrap()
        .call(&[input.len() as u64])
        .unwrap()[0];
    assert!(memory.write(name_ptr as usize, input));

    let ptr_size = module
        .exported_function("greeting")
        .unwrap()
        .call(&[name_ptr, input.len() as u64])
        .unwrap()[0];
    let greeting_ptr = (ptr_size >> 32) as usize;
    let greeting_len = (ptr_size as u32) as usize;
    let memory_size = memory.size() as usize;

    assert!(
        greeting_ptr <= memory_size
            && greeting_len <= memory_size
            && greeting_ptr + greeting_len <= memory_size,
        "ptr_size=0x{ptr_size:016x} ptr={greeting_ptr} len={greeting_len} memory={memory_size}"
    );
    assert_eq!(
        b"Hello, wazero!",
        &memory.read(greeting_ptr, greeting_len).unwrap()[..]
    );
}

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(name: &str) -> Self {
        let mut path = std::env::current_dir().unwrap();
        path.push("target");
        path.push("test-scratch");
        path.push(format!(
            "{name}-{}-{}",
            std::process::id(),
            SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
