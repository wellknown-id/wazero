#![doc = "Decoder errors and shared helpers."]

use std::error::Error;
use std::fmt::{Display, Formatter};

use razero::CoreFeatures;

pub const ERR_INVALID_BYTE: &str = "invalid byte";
pub const ERR_INVALID_MAGIC_NUMBER: &str = "invalid magic number";
pub const ERR_INVALID_VERSION: &str = "invalid version header";
pub const ERR_INVALID_SECTION_ID: &str = "invalid section id";
pub const ERR_CUSTOM_SECTION_NOT_FOUND: &str = "custom section not found";

pub type DecodeResult<T> = Result<T, DecodeError>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecodeError {
    pub message: String,
}

impl DecodeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for DecodeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for DecodeError {}

pub fn require_feature(
    enabled_features: CoreFeatures,
    feature: CoreFeatures,
    feature_name: &str,
) -> DecodeResult<()> {
    if enabled_features.contains(feature) {
        Ok(())
    } else {
        Err(DecodeError::new(format!(
            "feature \"{feature_name}\" is disabled"
        )))
    }
}
