#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuFeature {
    Amd64Sse41,
    Amd64Bmi1,
    Amd64Abm,
    Arm64Atomic,
}

const AMD64_SSE41: u64 = 1 << 0;
const AMD64_BMI1: u64 = 1 << 1;
const AMD64_ABM: u64 = 1 << 2;
const ARM64_ATOMIC: u64 = 1 << 0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuFeatureSet(u64);

impl CpuFeatureSet {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    pub const fn has(self, feature: CpuFeature) -> bool {
        self.contains(feature)
    }

    pub const fn contains(self, feature: CpuFeature) -> bool {
        self.0 & feature_mask(feature) != 0
    }
}

pub fn detected_cpu_features() -> CpuFeatureSet {
    let mut raw = 0_u64;

    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("sse4.1") {
            raw |= AMD64_SSE41;
        }
        if std::arch::is_x86_feature_detected!("bmi1") {
            raw |= AMD64_BMI1;
        }
        if std::arch::is_x86_feature_detected!("bmi1")
            && std::arch::is_x86_feature_detected!("bmi2")
            && std::arch::is_x86_feature_detected!("popcnt")
        {
            raw |= AMD64_ABM;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        #[cfg(target_os = "macos")]
        {
            raw |= ARM64_ATOMIC;
        }
        #[cfg(not(target_os = "macos"))]
        {
            if std::arch::is_aarch64_feature_detected!("lse") {
                raw |= ARM64_ATOMIC;
            }
        }
    }

    CpuFeatureSet::from_raw(raw)
}

pub fn compiler_supported() -> bool {
    compiler_platform_supports(false) && executable_mmap_supported()
}

fn compiler_platform_supports(threads_enabled: bool) -> bool {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux" | "macos" | "freebsd" | "netbsd" | "windows", "aarch64") => {
            !threads_enabled || detected_cpu_features().has(CpuFeature::Arm64Atomic)
        }
        (
            "linux" | "macos" | "freebsd" | "netbsd" | "windows" | "dragonfly" | "solaris"
            | "illumos",
            "x86_64",
        ) => detected_cpu_features().has(CpuFeature::Amd64Sse41),
        _ => false,
    }
}

fn executable_mmap_supported() -> bool {
    let mut segment = match crate::mmap::map_code_segment(1) {
        Ok(segment) => segment,
        Err(_) => return false,
    };
    let protected = crate::mmap::protect_code_segment(&mut segment).is_ok();
    let _ = crate::mmap::unmap_code_segment(&mut segment);
    protected
}

const fn feature_mask(feature: CpuFeature) -> u64 {
    match feature {
        CpuFeature::Amd64Sse41 => AMD64_SSE41,
        CpuFeature::Amd64Bmi1 => AMD64_BMI1,
        CpuFeature::Amd64Abm => AMD64_ABM,
        CpuFeature::Arm64Atomic => ARM64_ATOMIC,
    }
}

#[cfg(test)]
mod tests {
    use super::{compiler_supported, CpuFeature, CpuFeatureSet};

    #[test]
    fn raw_roundtrip_preserves_flags() {
        let flags = CpuFeatureSet::from_raw(0b111);
        assert_eq!(flags.raw(), 0b111);
        assert!(flags.has(CpuFeature::Amd64Sse41));
        assert!(flags.has(CpuFeature::Amd64Bmi1));
        assert!(flags.has(CpuFeature::Amd64Abm));
    }

    #[test]
    fn compiler_support_check_is_consistent_on_supported_targets() {
        if cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
            && cfg!(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "windows",
                target_os = "dragonfly",
                target_os = "solaris",
                target_os = "illumos"
            ))
        {
            let _ = compiler_supported();
        } else {
            assert!(!compiler_supported());
        }
    }
}
