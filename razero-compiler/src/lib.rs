#![doc = "Scaffold for the razero optimizing compiler crate."]

pub mod aot;
pub mod backend;
pub mod call_engine;
pub mod engine;
pub mod engine_cache;
pub mod entrypoint_amd64;
pub mod entrypoint_arm64;
pub mod entrypoint_other;
pub mod frontend;
pub mod hostmodule;
pub mod isa_amd64;
pub mod isa_arm64;
pub mod isa_other;
pub mod linker;
pub mod memmove;
pub mod module_engine;
pub mod runtime_support;
pub mod sighandler_linux;
pub mod sighandler_linux_amd64;
pub mod sighandler_linux_arm64;
pub mod sighandler_stub;
pub mod ssa;
pub mod wazevoapi;
