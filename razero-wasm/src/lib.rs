#![doc = "Runtime-side Wasm data model scaffolding."]

pub mod ieee754;
pub mod leb128;
pub mod moremath;
pub mod u32;
pub mod u64;
pub mod wasmdebug;
pub mod wasmruntime;

pub mod const_expr;
pub mod counts;
pub mod engine;
pub mod func_validation;
pub mod function_definition;
pub mod global;
pub mod host;
pub mod host_func;
pub mod instruction;
pub mod memory;
pub mod memory_definition;
pub mod module;
pub mod module_instance;
pub mod module_instance_lookup;
pub mod store;
pub mod store_module_list;
pub mod table;
