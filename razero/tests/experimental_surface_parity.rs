use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

use razero::{
    get_compilation_workers, get_import_resolver, with_compilation_workers, with_import_resolver,
    Context, ModuleConfig, Runtime,
};

#[test]
fn compilation_workers_getter_clamps_zero_to_one() {
    let ctx = with_compilation_workers(&Context::default(), 0);
    assert_eq!(1, get_compilation_workers(&ctx));
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
