use crate::api::features::CoreFeatures;

pub const CORE_FEATURES_THREADS: CoreFeatures = CoreFeatures::THREADS;
pub const CORE_FEATURES_TAIL_CALL: CoreFeatures = CoreFeatures::TAIL_CALL;
pub const CORE_FEATURES_EXTENDED_CONST: CoreFeatures = CoreFeatures::EXTENDED_CONST;

#[cfg(test)]
mod tests {
    use super::{CORE_FEATURES_EXTENDED_CONST, CORE_FEATURES_TAIL_CALL, CORE_FEATURES_THREADS};
    use crate::api::features::CoreFeatures;

    #[test]
    fn experimental_constants_match_core_features() {
        assert_eq!(CoreFeatures::THREADS, CORE_FEATURES_THREADS);
        assert_eq!(CoreFeatures::TAIL_CALL, CORE_FEATURES_TAIL_CALL);
        assert_eq!(CoreFeatures::EXTENDED_CONST, CORE_FEATURES_EXTENDED_CONST);
    }
}
