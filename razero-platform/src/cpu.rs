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
    match std::env::consts::ARCH {
        "x86_64" => detected_cpu_features().contains(CpuFeature::Amd64Sse41),
        "aarch64" => true,
        _ => false,
    }
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
    use super::{CpuFeature, CpuFeatureSet};

    #[test]
    fn raw_roundtrip_preserves_flags() {
        let flags = CpuFeatureSet::from_raw(0b111);
        assert_eq!(flags.raw(), 0b111);
        assert!(flags.has(CpuFeature::Amd64Sse41));
        assert!(flags.has(CpuFeature::Amd64Bmi1));
        assert!(flags.has(CpuFeature::Amd64Abm));
    }
}
