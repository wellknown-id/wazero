use std::{env, hint::black_box, process::Command, sync::Arc, sync::OnceLock};

use criterion::Criterion;
use razero::{
    with_fuel_controller, AggregatingFuelController, Context, CoreFeatures, LinearMemory,
    ModuleConfig, Runtime, RuntimeConfig, SimpleFuelController, CORE_FEATURES_TAIL_CALL,
    CORE_FEATURES_THREADS,
};
use razero_secmem::GuardPageAllocator;

const FAC_WASM: &[u8] = include_bytes!("../../testdata/fac.wasm");
const MEM_GROW_WASM: &[u8] = include_bytes!("../../testdata/mem_grow.wasm");
const MEMORY_PAGE_SIZE: usize = 65_536;
const FAC_BENCH_ARG: u64 = 20;

static FAC_EXECUTION_AVAILABLE: OnceLock<bool> = OnceLock::new();

fn fuzz_features() -> CoreFeatures {
    CoreFeatures::V2 | CORE_FEATURES_THREADS | CORE_FEATURES_TAIL_CALL
}

fn runtime_config() -> RuntimeConfig {
    RuntimeConfig::new().with_core_features(fuzz_features())
}

fn fac_execution_available() -> bool {
    *FAC_EXECUTION_AVAILABLE.get_or_init(|| {
        let Ok(exe) = env::current_exe() else {
            return false;
        };
        matches!(
            Command::new(exe)
                .env("RAZERO_SECBENCH_PROBE_FAC", "1")
                .status(),
            Ok(status) if status.success()
        )
    })
}

fn benchmark_compile_time(c: &mut Criterion) {
    let mut group = c.benchmark_group("secbench/compile_time");

    for (name, secure_mode) in [("standard", false), ("secure", true)] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let ctx = Context::default();
                let runtime = Runtime::with_config(runtime_config().with_secure_mode(secure_mode));
                let compiled = runtime
                    .compile(FAC_WASM)
                    .expect("factorial module should compile");
                black_box(compiled.bytes().len());
                compiled.close();
                runtime.close(&ctx).expect("runtime close should succeed");
            });
        });
    }

    group.finish();
}

fn benchmark_execution_baseline(c: &mut Criterion) {
    if !fac_execution_available() {
        eprintln!(
            "skipping secbench/execution_baseline: fac-ssa currently aborts in the Rust runtime"
        );
        return;
    }

    let mut group = c.benchmark_group("secbench/execution_baseline");

    for (name, secure_mode) in [("standard", false), ("secure", true)] {
        let ctx = Context::default();
        let runtime = Runtime::with_config(runtime_config().with_secure_mode(secure_mode));
        let module = runtime
            .instantiate_binary(FAC_WASM, ModuleConfig::new())
            .expect("factorial module should instantiate");
        let fac = module
            .exported_function("fac-ssa")
            .expect("fac-ssa export should exist");

        group.bench_function(name, |b| {
            b.iter(|| {
                let results = fac.call(&[FAC_BENCH_ARG]).expect("fac-ssa should execute");
                black_box(results);
            });
        });

        module.close(&ctx).expect("module close should succeed");
        runtime.close(&ctx).expect("runtime close should succeed");
    }

    group.finish();
}

fn benchmark_memory_allocate(c: &mut Criterion) {
    let mut group = c.benchmark_group("secbench/memory_allocate");
    let cap_bytes = MEMORY_PAGE_SIZE;
    let max_bytes = 256 * MEMORY_PAGE_SIZE;

    group.bench_function("go_slice", |b| {
        b.iter(|| {
            let mut bytes = Vec::with_capacity(max_bytes);
            bytes.resize(cap_bytes, 0);
            black_box(bytes);
        });
    });

    group.bench_function("guard_page_mmap", |b| {
        b.iter(|| {
            let allocation = GuardPageAllocator
                .allocate_zeroed(max_bytes)
                .expect("guard-page allocation should succeed");
            let memory = LinearMemory::from_guarded(allocation, cap_bytes, max_bytes);
            black_box(memory);
        });
    });

    group.finish();
}

fn benchmark_memory_grow(c: &mut Criterion) {
    let mut group = c.benchmark_group("secbench/memory_grow");

    for (name, secure_mode) in [("standard", false), ("secure", true)] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let ctx = Context::default();
                let runtime = Runtime::with_config(runtime_config().with_secure_mode(secure_mode));
                let _ = black_box(runtime.instantiate_binary(MEM_GROW_WASM, ModuleConfig::new()));
                runtime.close(&ctx).expect("runtime close should succeed");
            });
        });
    }

    group.finish();
}

fn benchmark_guard_page_allocator_grow(c: &mut Criterion) {
    let mut group = c.benchmark_group("secbench/guard_page_allocator_grow");
    let max_bytes = 1024 * MEMORY_PAGE_SIZE;

    group.bench_function("grow", |b| {
        b.iter(|| {
            let allocation = GuardPageAllocator
                .allocate_zeroed(max_bytes)
                .expect("guard-page allocation should succeed");
            let mut memory = LinearMemory::from_guarded(allocation, MEMORY_PAGE_SIZE, max_bytes);
            for pages in 2..=10 {
                let bytes = memory
                    .reallocate(pages * MEMORY_PAGE_SIZE)
                    .expect("incremental grow should succeed");
                black_box(bytes.len());
            }
            memory.free();
        });
    });

    group.finish();
}

fn benchmark_fuel_overhead(c: &mut Criterion) {
    if !fac_execution_available() {
        eprintln!("skipping secbench/fuel_overhead: fac-ssa currently aborts in the Rust runtime");
        return;
    }

    let mut group = c.benchmark_group("secbench/fuel_overhead");

    for (name, fuel) in [
        ("no_fuel", 0),
        ("fuel_1M", 1_000_000),
        ("fuel_100M", 100_000_000),
    ] {
        let ctx = Context::default();
        let runtime = Runtime::with_config(runtime_config().with_fuel(fuel));
        let module = runtime
            .instantiate_binary(FAC_WASM, ModuleConfig::new())
            .expect("factorial module should instantiate");
        let fac = module
            .exported_function("fac-ssa")
            .expect("fac-ssa export should exist");

        group.bench_function(name, |b| {
            b.iter(|| {
                let results = fac.call(&[FAC_BENCH_ARG]).expect("fac-ssa should execute");
                black_box(results);
            });
        });

        module.close(&ctx).expect("module close should succeed");
        runtime.close(&ctx).expect("runtime close should succeed");
    }

    group.finish();
}

fn benchmark_fuel_controller_overhead(c: &mut Criterion) {
    if !fac_execution_available() {
        eprintln!("skipping secbench/fuel_controller_overhead: fac-ssa currently aborts in the Rust runtime");
        return;
    }

    let mut group = c.benchmark_group("secbench/fuel_controller_overhead");
    let ctx = Context::default();
    let runtime = Runtime::with_config(runtime_config().with_fuel(1));
    let module = runtime
        .instantiate_binary(FAC_WASM, ModuleConfig::new())
        .expect("factorial module should instantiate");
    let fac = module
        .exported_function("fac-ssa")
        .expect("fac-ssa export should exist");

    group.bench_function("simple_1M", |b| {
        b.iter(|| {
            let call_ctx =
                with_fuel_controller(&Context::default(), SimpleFuelController::new(1_000_000));
            let results = fac
                .call_with_context(&call_ctx, &[FAC_BENCH_ARG])
                .expect("fac-ssa should execute");
            black_box(results);
        });
    });

    group.bench_function("aggregating_1M", |b| {
        b.iter(|| {
            let parent: Arc<dyn razero::FuelController> =
                Arc::new(SimpleFuelController::new(10_000_000));
            let call_ctx = with_fuel_controller(
                &Context::default(),
                AggregatingFuelController::new(Some(parent), 1_000_000),
            );
            let results = fac
                .call_with_context(&call_ctx, &[FAC_BENCH_ARG])
                .expect("fac-ssa should execute");
            black_box(results);
        });
    });

    group.finish();
    module.close(&ctx).expect("module close should succeed");
    runtime.close(&ctx).expect("runtime close should succeed");
}

fn run_fac_probe() -> i32 {
    let runtime = Runtime::with_config(runtime_config());
    let module = match runtime.instantiate_binary(FAC_WASM, ModuleConfig::new()) {
        Ok(module) => module,
        Err(_) => return 1,
    };
    let fac = match module.exported_function("fac-ssa") {
        Some(function) => function,
        None => return 1,
    };
    match fac.call(&[FAC_BENCH_ARG]) {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

fn main() {
    if env::var_os("RAZERO_SECBENCH_PROBE_FAC").is_some() {
        std::process::exit(run_fac_probe());
    }

    let mut criterion = Criterion::default().configure_from_args();
    benchmark_compile_time(&mut criterion);
    benchmark_execution_baseline(&mut criterion);
    benchmark_memory_allocate(&mut criterion);
    benchmark_memory_grow(&mut criterion);
    benchmark_guard_page_allocator_grow(&mut criterion);
    benchmark_fuel_overhead(&mut criterion);
    benchmark_fuel_controller_overhead(&mut criterion);
    criterion.final_summary();
}
