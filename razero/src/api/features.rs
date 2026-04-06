pub use razero_features::{CoreFeatures, CORE_FEATURES_V1, CORE_FEATURES_V2};

#[cfg(test)]
mod tests {
    use super::{CoreFeatures, CORE_FEATURES_V2};

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
}
