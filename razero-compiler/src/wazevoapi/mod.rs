//! Shared support utilities for the optimizing compiler.

pub mod debug_options;
pub mod exitcode;
pub mod offsetdata;
pub mod perfmap;
pub mod pool;
pub mod ptr;
pub mod queue;
pub mod resetmap;

pub use debug_options::{
    check_stack_guard_page, print_enabled_index, CurrentFunction, DeterministicCompilationError,
    DeterministicCompilationVerifier, NEED_FUNCTION_NAME_IN_CONTEXT,
};
pub use exitcode::{go_function_index_from_exit_code, ExitCode, EXIT_CODE_MASK, EXIT_CODE_MAX};
pub use offsetdata::{
    ModuleContextOffsetData, ModuleContextOffsetSource, Offset,
    FUNCTION_INSTANCE_EXECUTABLE_OFFSET, FUNCTION_INSTANCE_MODULE_CONTEXT_OPAQUE_PTR_OFFSET,
    FUNCTION_INSTANCE_SIZE, FUNCTION_INSTANCE_TYPE_ID_OFFSET,
};
pub use perfmap::{Perfmap, PERF_MAP_ENABLED};
pub use pool::{IDedPool, Pool, VarLength, VarLengthPool};
pub use ptr::{non_null_from_usize, ptr_from_usize};
pub use queue::Queue;
pub use resetmap::{reset_map, ClearableMap};
