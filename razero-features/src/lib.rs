use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct CoreFeatures(u64);

impl CoreFeatures {
    pub const BULK_MEMORY_OPERATIONS: Self = Self(1 << 0);
    pub const MULTI_VALUE: Self = Self(1 << 1);
    pub const MUTABLE_GLOBAL: Self = Self(1 << 2);
    pub const NON_TRAPPING_FLOAT_TO_INT_CONVERSION: Self = Self(1 << 3);
    pub const REFERENCE_TYPES: Self = Self(1 << 4);
    pub const SIGN_EXTENSION_OPS: Self = Self(1 << 5);
    pub const SIMD: Self = Self(1 << 6);
    pub const THREADS: Self = Self(1 << 7);
    pub const TAIL_CALL: Self = Self(1 << 8);
    pub const EXTENDED_CONST: Self = Self(1 << 9);

    pub const MUTABLE_GLOBALS: Self = Self::MUTABLE_GLOBAL;
    pub const BULK_MEMORY: Self = Self::BULK_MEMORY_OPERATIONS;

    pub const V1: Self = Self::MUTABLE_GLOBAL;
    pub const V2: Self = Self(
        Self::V1.0
            | Self::BULK_MEMORY_OPERATIONS.0
            | Self::MULTI_VALUE.0
            | Self::NON_TRAPPING_FLOAT_TO_INT_CONVERSION.0
            | Self::REFERENCE_TYPES.0
            | Self::SIGN_EXTENSION_OPS.0
            | Self::SIMD.0,
    );

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn all() -> Self {
        Self(
            Self::BULK_MEMORY_OPERATIONS.0
                | Self::MULTI_VALUE.0
                | Self::MUTABLE_GLOBAL.0
                | Self::NON_TRAPPING_FLOAT_TO_INT_CONVERSION.0
                | Self::REFERENCE_TYPES.0
                | Self::SIGN_EXTENSION_OPS.0
                | Self::SIMD.0
                | Self::THREADS.0
                | Self::TAIL_CALL.0
                | Self::EXTENDED_CONST.0,
        )
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn set_enabled(self, feature: Self, enabled: bool) -> Self {
        if enabled {
            self | feature
        } else {
            self & Self(!feature.0)
        }
    }

    pub fn is_enabled(self, feature: Self) -> bool {
        feature.0 != 0 && self.contains(feature)
    }

    pub fn require_enabled(self, feature: Self) -> Result<(), FeatureError> {
        if self.contains(feature) {
            Ok(())
        } else {
            Err(FeatureError {
                feature_name: feature_name(feature).unwrap_or("<unknown>"),
            })
        }
    }
}

impl Display for CoreFeatures {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for bit in 0..64 {
            let feature = Self(1_u64 << bit);
            if !self.is_enabled(feature) {
                continue;
            }
            let Some(name) = feature_name(feature) else {
                continue;
            };
            if !first {
                f.write_str("|")?;
            }
            first = false;
            f.write_str(name)?;
        }
        Ok(())
    }
}

impl BitOr for CoreFeatures {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for CoreFeatures {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for CoreFeatures {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for CoreFeatures {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

pub const CORE_FEATURES_V1: CoreFeatures = CoreFeatures::V1;
pub const CORE_FEATURES_V2: CoreFeatures = CoreFeatures::V2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeatureError {
    feature_name: &'static str,
}

impl Display for FeatureError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "feature {:?} is disabled", self.feature_name)
    }
}

impl Error for FeatureError {}

fn feature_name(feature: CoreFeatures) -> Option<&'static str> {
    match feature {
        CoreFeatures::BULK_MEMORY_OPERATIONS => Some("bulk-memory-operations"),
        CoreFeatures::MULTI_VALUE => Some("multi-value"),
        CoreFeatures::MUTABLE_GLOBAL => Some("mutable-global"),
        CoreFeatures::NON_TRAPPING_FLOAT_TO_INT_CONVERSION => {
            Some("nontrapping-float-to-int-conversion")
        }
        CoreFeatures::REFERENCE_TYPES => Some("reference-types"),
        CoreFeatures::SIGN_EXTENSION_OPS => Some("sign-extension-ops"),
        CoreFeatures::SIMD => Some("simd"),
        CoreFeatures::THREADS => Some("threads"),
        CoreFeatures::TAIL_CALL => Some("tail-call"),
        CoreFeatures::EXTENDED_CONST => Some("extended-const"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{CoreFeatures, CORE_FEATURES_V2};

    #[test]
    fn zero_is_not_a_valid_feature_flag() {
        let features = CoreFeatures::empty().set_enabled(CoreFeatures::empty(), true);
        assert!(!features.is_enabled(CoreFeatures::empty()));
    }

    #[test]
    fn set_enabled_round_trips() {
        let features = CoreFeatures::empty()
            .set_enabled(CoreFeatures::MUTABLE_GLOBAL, true)
            .set_enabled(CoreFeatures::SIMD, true)
            .set_enabled(CoreFeatures::SIMD, false);
        assert!(features.is_enabled(CoreFeatures::MUTABLE_GLOBAL));
        assert!(!features.is_enabled(CoreFeatures::SIMD));
    }

    #[test]
    fn display_matches_feature_order() {
        assert_eq!(
            "bulk-memory-operations|multi-value|mutable-global|nontrapping-float-to-int-conversion|reference-types|sign-extension-ops|simd",
            CORE_FEATURES_V2.to_string()
        );
    }

    #[test]
    fn require_enabled_matches_go_error_text() {
        let err = CoreFeatures::empty()
            .require_enabled(CoreFeatures::MUTABLE_GLOBAL)
            .expect_err("mutable-global should be reported as disabled");
        assert_eq!("feature \"mutable-global\" is disabled", err.to_string());

        assert!(CoreFeatures::MUTABLE_GLOBAL
            .require_enabled(CoreFeatures::MUTABLE_GLOBAL)
            .is_ok());
    }
}
