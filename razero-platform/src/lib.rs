#![doc = "Platform support scaffold for the Rust port."]

pub mod cpu;
pub mod crypto;
pub mod guard;
pub mod mmap;
pub mod path;
pub mod time;

#[cfg(target_os = "linux")]
mod mmap_linux;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod mmap_other;
#[cfg(target_os = "windows")]
mod mmap_windows;

pub use cpu::{compiler_supported, detected_cpu_features, CpuFeature, CpuFeatureSet};
pub use crypto::{new_fake_rand_source, FakeRandSource};
pub use guard::{
    guard_page_support, supports_guard_pages, GuardPageError, GuardPageSupport, LinearMemory,
    LinearMemoryLayout, GUARD_REGION_SIZE,
};
pub use mmap::{
    map_code_segment, protect_code_segment, unmap_code_segment, CodeSegment, MmapError,
};
pub use path::to_posix_path;
pub use time::{
    nanosleep, nanotime, new_fake_nanotime, new_fake_walltime, walltime, FAKE_EPOCH_NANOS,
};
