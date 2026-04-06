use std::fs;
use std::ptr::NonNull;
use std::sync::OnceLock;

use crate::mmap::{CodeSegment, MmapError, PlatformKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HugePageConfig {
    pub(crate) size: usize,
    pub(crate) flag: libc::c_int,
}

static HUGE_PAGE_CONFIGS: OnceLock<Vec<HugePageConfig>> = OnceLock::new();

pub(crate) fn map_code_segment_impl(len: usize) -> Result<CodeSegment, MmapError> {
    if len == 0 {
        return Err(MmapError::ZeroLength);
    }

    let base_flags = libc::MAP_ANONYMOUS | libc::MAP_PRIVATE;
    for config in huge_page_configs() {
        if len & (config.size - 1) == 0 {
            match anonymous_mmap(
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                base_flags | config.flag,
            ) {
                Ok(base) => {
                    return Ok(unsafe {
                        CodeSegment::from_raw_parts(base.as_ptr(), len, PlatformKind::Linux)
                    });
                }
                Err(_) => continue,
            }
        }
    }

    let base = anonymous_mmap(len, libc::PROT_READ | libc::PROT_WRITE, base_flags)?;
    Ok(unsafe { CodeSegment::from_raw_parts(base.as_ptr(), len, PlatformKind::Linux) })
}

pub(crate) fn protect_code_segment_impl(segment: &mut CodeSegment) -> Result<(), MmapError> {
    let base = segment.base()?;
    let rc = unsafe {
        libc::mprotect(
            base.as_ptr().cast(),
            segment.len(),
            libc::PROT_READ | libc::PROT_EXEC,
        )
    };
    if rc != 0 {
        return Err(errno_error("mprotect"));
    }

    segment.set_executable(true);
    Ok(())
}

pub(crate) fn unmap_code_segment_impl(segment: &mut CodeSegment) -> Result<(), MmapError> {
    let base = segment.base()?;
    let rc = unsafe { libc::munmap(base.as_ptr().cast(), segment.len()) };
    if rc != 0 {
        return Err(errno_error("munmap"));
    }

    segment.clear();
    Ok(())
}

pub(crate) fn huge_page_configs() -> &'static [HugePageConfig] {
    HUGE_PAGE_CONFIGS
        .get_or_init(load_huge_page_configs)
        .as_slice()
}

fn load_huge_page_configs() -> Vec<HugePageConfig> {
    let Ok(entries) = fs::read_dir("/sys/kernel/mm/hugepages/") else {
        return Vec::new();
    };

    let mut configs = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(size_kb) = name
            .strip_prefix("hugepages-")
            .and_then(|value| value.strip_suffix("kB"))
        else {
            continue;
        };
        let Ok(size_kb) = size_kb.parse::<usize>() else {
            continue;
        };
        let Some(size) = size_kb.checked_mul(1024) else {
            continue;
        };
        if !size.is_power_of_two() {
            continue;
        }

        configs.push(HugePageConfig {
            size,
            flag: ((size.trailing_zeros() as libc::c_int) << libc::MAP_HUGE_SHIFT)
                | libc::MAP_HUGETLB,
        });
    }

    configs.sort_by(|left, right| right.size.cmp(&left.size));
    configs
}

fn anonymous_mmap(
    len: usize,
    prot: libc::c_int,
    flags: libc::c_int,
) -> Result<NonNull<u8>, MmapError> {
    let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, prot, flags, -1, 0) };
    if ptr == libc::MAP_FAILED {
        return Err(errno_error("mmap"));
    }

    NonNull::new(ptr.cast()).ok_or(errno_error("mmap"))
}

fn errno_error(operation: &'static str) -> MmapError {
    MmapError::Syscall {
        operation,
        errno: std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EINVAL),
    }
}

#[cfg(test)]
mod tests {
    use super::huge_page_configs;

    #[test]
    fn huge_page_configs_are_sorted_and_well_formed() {
        let configs = huge_page_configs();
        for window in configs.windows(2) {
            assert!(window[0].size > window[1].size);
        }
        for config in configs {
            assert!(config.size.is_power_of_two());
            assert_ne!(config.flag, 0);
        }
    }
}
