use std::{env, hint::black_box, process::Command, sync::OnceLock};

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use criterion::Criterion;
use razero::{
    with_fuel_controller, AggregatingFuelController, Context, CoreFeatures, LinearMemory,
    ModuleConfig, Runtime, RuntimeConfig, SimpleFuelController, CORE_FEATURES_TAIL_CALL,
    CORE_FEATURES_THREADS,
};
use razero_secmem::GuardPageAllocator;

const FAC_WASM: &[u8] = include_bytes!("../../testdata/fac.wasm");
const MEM_GROW_WASM: &[u8] = include_bytes!("../../testdata/mem_grow.wasm");
const OOB_LOAD_WASM: &[u8] = include_bytes!("../../testdata/oob_load.wasm");
const MEMORY_PAGE_SIZE: usize = 65_536;
const FAC_BENCH_ARG: u64 = 20;
const OOB_TRAP_TEXT: &str = "out of bounds memory access";
const GROUP_COMPILE_TIME: &str = "secbench/compile_time";
const GROUP_EXECUTION_BASELINE: &str = "secbench/execution_baseline";
const GROUP_TRAP_OVERHEAD: &str = "secbench/trap_overhead";
const GROUP_MEMORY_GROW: &str = "secbench/memory_grow";
const GROUP_MEMORY_ALLOCATE: &str = "secbench/memory_allocate";
const GROUP_GUARD_PAGE_ALLOCATOR_GROW: &str = "secbench/guard_page_allocator_grow";
const GROUP_FUEL_OVERHEAD: &str = "secbench/fuel_overhead";
const GROUP_FUEL_CONTROLLER_OVERHEAD: &str = "secbench/fuel_controller_overhead";
const GROUP_FUEL_COMPILE_OVERHEAD: &str = "secbench/fuel_compile_overhead";
const GROUP_FUEL_ACCOUNTING: &str = "secbench/fuel_accounting";

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

fn skip_fac_dependent_group(group_name: &str) {
    eprintln!(
        "skipping {group_name}: requires fac-ssa to execute successfully in the current runtime"
    );
}

fn benchmark_compile_time(c: &mut Criterion) {
    let mut group = c.benchmark_group(GROUP_COMPILE_TIME);

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
        skip_fac_dependent_group(GROUP_EXECUTION_BASELINE);
        return;
    }

    let mut group = c.benchmark_group(GROUP_EXECUTION_BASELINE);

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

fn benchmark_trap_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group(GROUP_TRAP_OVERHEAD);

    for (name, secure_mode) in [("standard", false), ("secure", true)] {
        let ctx = Context::default();
        let runtime = Runtime::with_config(runtime_config().with_secure_mode(secure_mode));
        let module = runtime
            .instantiate_binary(OOB_LOAD_WASM, ModuleConfig::new())
            .expect("oob module should instantiate");
        let oob = module
            .exported_function("oob")
            .expect("oob export should exist");

        let err = oob.call(&[]).expect_err("oob should trap");
        assert!(
            err.to_string().contains(OOB_TRAP_TEXT),
            "unexpected trap: {err}"
        );

        group.bench_function(name, |b| {
            b.iter(|| {
                let err = oob.call(&[]).expect_err("oob should trap");
                black_box(err);
            });
        });

        module.close(&ctx).expect("module close should succeed");
        runtime.close(&ctx).expect("runtime close should succeed");
    }

    group.finish();
}

fn benchmark_memory_allocate(c: &mut Criterion) {
    let mut group = c.benchmark_group(GROUP_MEMORY_ALLOCATE);
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
    let mut group = c.benchmark_group(GROUP_MEMORY_GROW);

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
    let mut group = c.benchmark_group(GROUP_GUARD_PAGE_ALLOCATOR_GROW);
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
        skip_fac_dependent_group(GROUP_FUEL_OVERHEAD);
        return;
    }

    let mut group = c.benchmark_group(GROUP_FUEL_OVERHEAD);

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
        skip_fac_dependent_group(GROUP_FUEL_CONTROLLER_OVERHEAD);
        return;
    }

    let mut group = c.benchmark_group(GROUP_FUEL_CONTROLLER_OVERHEAD);
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

fn benchmark_fuel_compile_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group(GROUP_FUEL_COMPILE_OVERHEAD);

    for (name, fuel) in [("no_fuel", 0), ("fuel_enabled", 1_000_000)] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let ctx = Context::default();
                let runtime = Runtime::with_config(runtime_config().with_fuel(fuel));
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

struct AccountingFuelController {
    budget: i64,
    consumed: Arc<AtomicI64>,
}

impl AccountingFuelController {
    fn new(budget: i64, consumed: Arc<AtomicI64>) -> Self {
        consumed.store(0, Ordering::SeqCst);
        Self { budget, consumed }
    }
}

impl razero::FuelController for AccountingFuelController {
    fn budget(&self) -> i64 {
        self.budget
    }

    fn consumed(&self, amount: i64) {
        self.consumed.fetch_add(amount, Ordering::SeqCst);
    }
}

fn benchmark_fuel_accounting(c: &mut Criterion) {
    if !fac_execution_available() {
        skip_fac_dependent_group(GROUP_FUEL_ACCOUNTING);
        return;
    }

    {
        let ctx = Context::default();
        let runtime = Runtime::with_config(runtime_config().with_fuel(1));
        let module = runtime
            .instantiate_binary(FAC_WASM, ModuleConfig::new())
            .expect("factorial module should instantiate");
        let fac = module
            .exported_function("fac-ssa")
            .expect("fac-ssa export should exist");

        let consumed = Arc::new(AtomicI64::new(0));
        let controller = AccountingFuelController::new(10_000_000, consumed.clone());
        let call_ctx = with_fuel_controller(&Context::default(), controller);
        let results = fac
            .call_with_context(&call_ctx, &[FAC_BENCH_ARG])
            .expect("fac-ssa should execute");
        black_box(results);
        eprintln!(
            "secbench/fuel_accounting: fac(20) consumed {} fuel units",
            consumed.load(Ordering::SeqCst)
        );

        drop(call_ctx);
        module.close(&ctx).expect("module close should succeed");
        runtime.close(&ctx).expect("runtime close should succeed");
    }

    let mut group = c.benchmark_group(GROUP_FUEL_ACCOUNTING);

    group.bench_function("fuel_exhaustion_path", |b| {
        b.iter(|| {
            let ctx = Context::default();
            let runtime = Runtime::with_config(runtime_config().with_fuel(1));
            let module = runtime
                .instantiate_binary(FAC_WASM, ModuleConfig::new())
                .expect("factorial module should instantiate");
            let fac = module
                .exported_function("fac-ssa")
                .expect("fac-ssa export should exist");

            let call_ctx = with_fuel_controller(&Context::default(), SimpleFuelController::new(1));
            let err = fac
                .call_with_context(&call_ctx, &[FAC_BENCH_ARG])
                .expect_err("should exhaust fuel");
            black_box(err);

            drop(call_ctx);
            module.close(&ctx).expect("module close should succeed");
            runtime.close(&ctx).expect("runtime close should succeed");
        });
    });

    group.finish();
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

    // These groups map directly to the Workstream 1 benchmark baseline item in
    // SE-ROADMAP.md.
    benchmark_compile_time(&mut criterion);
    benchmark_execution_baseline(&mut criterion);
    benchmark_trap_overhead(&mut criterion);
    benchmark_memory_grow(&mut criterion);

    // These groups remain useful diagnostic measurements, but they are not the
    // canonical roadmap baseline set.
    benchmark_memory_allocate(&mut criterion);
    benchmark_guard_page_allocator_grow(&mut criterion);
    benchmark_fuel_overhead(&mut criterion);
    benchmark_fuel_controller_overhead(&mut criterion);
    benchmark_fuel_compile_overhead(&mut criterion);
    benchmark_fuel_accounting(&mut criterion);
    criterion.final_summary();
}
