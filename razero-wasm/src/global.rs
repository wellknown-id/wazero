#![doc = "Runtime-side Wasm globals."]

use std::fmt;
use std::sync::{Arc, RwLock};

use crate::module::{GlobalType, ValueType};

#[derive(Debug, Default)]
struct GlobalValue {
    lo: u64,
    hi: u64,
}

#[derive(Debug, Clone, Default)]
pub struct GlobalInstance {
    pub ty: GlobalType,
    pub mutable: bool,
    value: Arc<RwLock<GlobalValue>>,
}

impl GlobalInstance {
    pub fn new(ty: GlobalType, value: u64) -> Self {
        Self {
            ty,
            mutable: ty.mutable,
            value: Arc::new(RwLock::new(GlobalValue { lo: value, hi: 0 })),
        }
    }

    pub fn with_value_hi(ty: GlobalType, value: u64, value_hi: u64) -> Self {
        Self {
            ty,
            mutable: ty.mutable,
            value: Arc::new(RwLock::new(GlobalValue {
                lo: value,
                hi: value_hi,
            })),
        }
    }

    pub fn value(&self) -> (u64, u64) {
        let value = self.value.read().expect("global read lock");
        (value.lo, value.hi)
    }

    pub fn set_value(&mut self, lo: u64, hi: u64) {
        let mut value = self.value.write().expect("global write lock");
        value.lo = lo;
        value.hi = hi;
    }
}

impl PartialEq for GlobalInstance {
    fn eq(&self, other: &Self) -> bool {
        self.ty == other.ty && self.mutable == other.mutable && self.value() == other.value()
    }
}

impl Eq for GlobalInstance {}

impl fmt::Display for GlobalInstance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (value, _) = self.value();
        match self.ty.val_type {
            ValueType::I32 | ValueType::I64 => write!(f, "global({value})"),
            ValueType::F32 => write!(f, "global({})", f32::from_bits(value as u32)),
            ValueType::F64 => write!(f, "global({})", f64::from_bits(value)),
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
