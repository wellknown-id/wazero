pub mod error;
pub mod features;
pub mod wasm;

pub use error::{ExitError, Result, RuntimeError};
pub use features::CoreFeatures;
pub use wasm::{
    CustomSection,
    ExternType,
    Function,
    FunctionDefinition,
    Global,
    GlobalValue,
    Instance,
    Memory,
    MemoryDefinition,
    Module,
    ValueType,
};
