#![doc = "Runtime-side Wasm globals."]

use std::fmt;

use crate::module::{GlobalType, ValueType};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalInstance {
    pub ty: GlobalType,
    pub value: u64,
    pub value_hi: u64,
    pub mutable: bool,
}

impl GlobalInstance {
    pub fn new(ty: GlobalType, value: u64) -> Self {
        Self {
            ty,
            value,
            value_hi: 0,
            mutable: ty.mutable,
        }
    }

    pub fn with_value_hi(ty: GlobalType, value: u64, value_hi: u64) -> Self {
        Self {
            ty,
            value,
            value_hi,
            mutable: ty.mutable,
        }
    }

    pub fn value(&self) -> (u64, u64) {
        (self.value, self.value_hi)
    }

    pub fn set_value(&mut self, lo: u64, hi: u64) {
        self.value = lo;
        self.value_hi = hi;
    }
}

impl fmt::Display for GlobalInstance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ty.val_type {
            ValueType::I32 | ValueType::I64 => write!(f, "global({})", self.value),
            ValueType::F32 => write!(f, "global({})", f32::from_bits(self.value as u32)),
            ValueType::F64 => write!(f, "global({})", f64::from_bits(self.value)),
            other => panic!("BUG: unknown value type {:X}", other.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::GlobalInstance;
    use crate::module::{GlobalType, ValueType};

    #[test]
    fn value_round_trips_for_scalars_and_vectors() {
        let mut scalar = GlobalInstance::new(
            GlobalType {
                val_type: ValueType::I64,
                mutable: true,
            },
            1,
        );
        assert_eq!(scalar.value(), (1, 0));
        scalar.set_value(2, 0);
        assert_eq!(scalar.value(), (2, 0));

        let mut vector = GlobalInstance::with_value_hi(
            GlobalType {
                val_type: ValueType::V128,
                mutable: true,
            },
            10,
            20,
        );
        assert_eq!(vector.value(), (10, 20));
        vector.set_value(30, 40);
        assert_eq!(vector.value(), (30, 40));
    }

    #[test]
    fn display_matches_numeric_and_float_globals() {
        assert_eq!(
            GlobalInstance::new(
                GlobalType {
                    val_type: ValueType::I32,
                    mutable: false,
                },
                7,
            )
            .to_string(),
            "global(7)"
        );

        assert_eq!(
            GlobalInstance::new(
                GlobalType {
                    val_type: ValueType::F32,
                    mutable: false,
                },
                u64::from(f32::to_bits(1.25)),
            )
            .to_string(),
            "global(1.25)"
        );

        assert_eq!(
            GlobalInstance::new(
                GlobalType {
                    val_type: ValueType::F64,
                    mutable: false,
                },
                f64::to_bits(2.5),
            )
            .to_string(),
            "global(2.5)"
        );
    }
}
