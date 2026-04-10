use std::{collections::BTreeSet, sync::Arc};

use crate::{api::wasm::Module, ctx_keys::Context};

pub type ImportResolver = dyn Fn(&str) -> Option<Module> + Send + Sync;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportACL {
    allow_modules: BTreeSet<String>,
    allow_prefixes: Vec<String>,
    deny_modules: BTreeSet<String>,
    deny_prefixes: Vec<String>,
}

#[derive(Clone, Default)]
pub struct ImportResolverConfig {
    pub resolver: Option<Arc<ImportResolver>>,
    pub acl: Option<ImportACL>,
    pub fail_closed: bool,
}

impl ImportACL {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow_modules<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for name in names {
            let name = name.into();
            if !name.is_empty() {
                self.allow_modules.insert(name);
            }
        }
        self
    }

    pub fn allow_module_prefixes<I, S>(mut self, prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for prefix in prefixes {
            let prefix = prefix.into();
            if !prefix.is_empty() {
                self.allow_prefixes.push(prefix);
            }
        }
        self
    }

    pub fn deny_modules<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for name in names {
            let name = name.into();
            if !name.is_empty() {
                self.deny_modules.insert(name);
            }
        }
        self
    }

    pub fn deny_module_prefixes<I, S>(mut self, prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for prefix in prefixes {
            let prefix = prefix.into();
            if !prefix.is_empty() {
                self.deny_prefixes.push(prefix);
            }
        }
        self
    }

    pub fn check_import(&self, module_name: &str) -> crate::Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        if self.matches_module(&self.deny_modules, &self.deny_prefixes, module_name) {
            return Err(crate::RuntimeError::new(format!(
                "module[{module_name}] denied by import ACL"
            )));
        }
        if self.has_allow_rules()
            && !self.matches_module(&self.allow_modules, &self.allow_prefixes, module_name)
        {
            return Err(crate::RuntimeError::new(format!(
                "module[{module_name}] not allowed by import ACL"
            )));
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.allow_modules.is_empty()
            && self.allow_prefixes.is_empty()
            && self.deny_modules.is_empty()
            && self.deny_prefixes.is_empty()
    }

    fn has_allow_rules(&self) -> bool {
        !self.allow_modules.is_empty() || !self.allow_prefixes.is_empty()
    }

    fn matches_module(
        &self,
        exact: &BTreeSet<String>,
        prefixes: &[String],
        module_name: &str,
    ) -> bool {
        exact.contains(module_name)
            || prefixes
                .iter()
                .any(|prefix| module_name.starts_with(prefix))
    }
}

impl ImportResolverConfig {
    fn is_empty(&self) -> bool {
        self.resolver.is_none() && self.acl.as_ref().is_none_or(ImportACL::is_empty)
    }
}

pub fn with_import_resolver(
    ctx: &Context,
    resolver: impl Fn(&str) -> Option<Module> + Send + Sync + 'static,
) -> Context {
    let mut cloned = ctx.clone();
    let mut cfg = cloned.import_resolver.unwrap_or_default();
    cfg.resolver = Some(Arc::new(resolver));
    cloned.import_resolver = Some(cfg);
    cloned
}

pub fn get_import_resolver(ctx: &Context) -> Option<Arc<ImportResolver>> {
    ctx.import_resolver
        .as_ref()
        .and_then(|cfg| cfg.resolver.clone())
}

pub fn with_import_resolver_acl(ctx: &Context, acl: ImportACL) -> Context {
    if acl.is_empty() {
        return ctx.clone();
    }
    let mut cloned = ctx.clone();
    let mut cfg = cloned.import_resolver.unwrap_or_default();
    cfg.acl = Some(acl);
    cloned.import_resolver = Some(cfg);
    cloned
}

pub fn with_import_resolver_config(ctx: &Context, cfg: ImportResolverConfig) -> Context {
    if cfg.is_empty() {
        return ctx.clone();
    }
    let mut cloned = ctx.clone();
    cloned.import_resolver = Some(cfg);
    cloned
}

pub fn get_import_resolver_config(ctx: &Context) -> Option<ImportResolverConfig> {
    ctx.import_resolver.clone()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    };

    use super::{
        get_import_resolver, get_import_resolver_config, with_import_resolver,
        with_import_resolver_acl, ImportACL, ImportResolverConfig,
    };
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

    #[test]
    fn import_resolver_config_acl_only_round_trip() {
        let acl = ImportACL::new().deny_modules(["env"]);
        let ctx = super::with_import_resolver_config(
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
        assert_eq!(Some(ImportACL::new().allow_modules(["env"])), cfg.acl,);
    }

    #[test]
    fn import_acl_allow_prefixes_match_module_names() {
        let acl = ImportACL::new().allow_module_prefixes(["wasi_", "env."]);

        assert!(acl.check_import("wasi_snapshot_preview1").is_ok());
        assert!(acl.check_import("env.clock").is_ok());
        assert!(acl
            .check_import("other")
            .expect_err("unexpected module should be rejected")
            .to_string()
            .contains("module[other] not allowed by import ACL"));
    }

    #[test]
    fn import_acl_deny_prefixes_block_module_names() {
        let acl = ImportACL::new().deny_module_prefixes(["__internal_", "private."]);

        assert!(acl
            .check_import("__internal_clock")
            .expect_err("internal module should be denied")
            .to_string()
            .contains("module[__internal_clock] denied by import ACL"));
        assert!(acl
            .check_import("private.env")
            .expect_err("private module should be denied")
            .to_string()
            .contains("module[private.env] denied by import ACL"));
        assert!(acl.check_import("env").is_ok());
    }

    #[test]
    fn import_acl_deny_prefixes_take_precedence_over_allow_rules() {
        let acl = ImportACL::new()
            .allow_module_prefixes(["env."])
            .allow_modules(["wasi_snapshot_preview1"])
            .deny_module_prefixes(["env.internal."]);

        assert!(acl.check_import("env.public.clock").is_ok());
        assert!(acl.check_import("wasi_snapshot_preview1").is_ok());
        assert!(acl
            .check_import("env.internal.clock")
            .expect_err("deny prefix should win")
            .to_string()
            .contains("module[env.internal.clock] denied by import ACL"));
    }
}
