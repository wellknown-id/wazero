use crate::builder::HostModuleBuilder;

pub const MODULE_NAME: &str = "assemblyscript";
pub const ABORT_NAME: &str = "abort";
pub const TRACE_NAME: &str = "trace";
pub const SEED_NAME: &str = "seed";

pub fn host_module_builder() -> HostModuleBuilder {
    HostModuleBuilder::new(MODULE_NAME)
}
