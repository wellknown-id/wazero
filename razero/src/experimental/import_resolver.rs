use std::sync::Arc;

use crate::{api::wasm::Module, ctx_keys::Context};

pub type ImportResolver = dyn Fn(&str) -> Option<Module> + Send + Sync;

pub fn with_import_resolver(
    ctx: &Context,
    resolver: impl Fn(&str) -> Option<Module> + Send + Sync + 'static,
) -> Context {
    let mut cloned = ctx.clone();
    cloned.import_resolver = Some(Arc::new(resolver));
    cloned
}

pub fn get_import_resolver(ctx: &Context) -> Option<Arc<ImportResolver>> {
    ctx.import_resolver.clone()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    };

    use super::{get_import_resolver, with_import_resolver};
    use crate::{ctx_keys::Context, ModuleConfig, Runtime};

    #[test]
    fn stores_import_resolver_in_context() {
        let ctx = with_import_resolver(&Context::default(), |_name| None);
        let resolver = get_import_resolver(&ctx).expect("resolver should be present");
        assert!(resolver("env").is_none());
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
}
