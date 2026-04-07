use crate::{builder::HostModuleBuilder, logging::LogScopes, FunctionDefinition};

pub const MODULE_NAME: &str = "assemblyscript";
pub const ABORT_NAME: &str = "abort";
pub const TRACE_NAME: &str = "trace";
pub const SEED_NAME: &str = "seed";

pub fn host_module_builder() -> HostModuleBuilder {
    HostModuleBuilder::new(MODULE_NAME)
}

pub fn is_in_log_scope(function: &FunctionDefinition, scopes: LogScopes) -> bool {
    if scopes.is_enabled(LogScopes::PROC) && is_proc_function(function) {
        return true;
    }
    if scopes.is_enabled(LogScopes::RANDOM) && is_random_function(function) {
        return true;
    }
    scopes == LogScopes::ALL
}

fn is_proc_function(function: &FunctionDefinition) -> bool {
    function.export_names().first().map(String::as_str) == Some(ABORT_NAME)
}

fn is_random_function(function: &FunctionDefinition) -> bool {
    function.export_names().first().map(String::as_str) == Some(SEED_NAME)
}

#[cfg(test)]
mod tests {
    use super::{is_in_log_scope, FunctionDefinition, ABORT_NAME, SEED_NAME};
    use crate::logging::LogScopes;

    #[test]
    fn assemblyscript_log_scope_matches_go() {
        let abort = FunctionDefinition::new(ABORT_NAME).with_export_name(ABORT_NAME);
        let seed = FunctionDefinition::new(SEED_NAME).with_export_name(SEED_NAME);

        assert!(is_in_log_scope(&abort, LogScopes::PROC));
        assert!(!is_in_log_scope(&abort, LogScopes::FILESYSTEM));
        assert!(is_in_log_scope(
            &abort,
            LogScopes::PROC | LogScopes::FILESYSTEM,
        ));
        assert!(is_in_log_scope(&abort, LogScopes::ALL));
        assert!(!is_in_log_scope(&abort, LogScopes::NONE));

        assert!(!is_in_log_scope(&seed, LogScopes::FILESYSTEM));
        assert!(is_in_log_scope(
            &seed,
            LogScopes::RANDOM | LogScopes::FILESYSTEM,
        ));
        assert!(is_in_log_scope(&seed, LogScopes::ALL));
        assert!(!is_in_log_scope(&seed, LogScopes::NONE));
    }
}
