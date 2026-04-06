use std::fmt;

use crate::ssa::types::Type;

const VARIABLE_TYPE_SHIFT: u32 = 28;
const VALUE_ID_INVALID: u32 = u32::MAX;
const VALUE_TYPE_SHIFT: u64 = 60;
const VALUE_INSTRUCTION_SHIFT: u64 = 32;
const VALUE_INSTRUCTION_MASK: u64 = 0x0fff_ffff;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Variable(pub u32);

impl Variable {
    pub fn with_type(self, ty: Type) -> Self {
        assert!(
            self.0 < (1 << VARIABLE_TYPE_SHIFT),
            "too large variable: {}",
            self.0
        );
        Self(((ty as u32) << VARIABLE_TYPE_SHIFT) | self.0)
    }

    pub const fn ty(self) -> Type {
        match self.0 >> VARIABLE_TYPE_SHIFT {
            1 => Type::I32,
            2 => Type::I64,
            3 => Type::F32,
            4 => Type::F64,
            5 => Type::V128,
            _ => Type::Invalid,
        }
    }

    pub const fn raw(self) -> u32 {
        self.0 & 0x0fff_ffff
    }
}

impl fmt::Display for Variable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "var{}", self.raw())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueId(pub u32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Value(pub u64);

impl Value {
    pub const INVALID: Self = Self(VALUE_ID_INVALID as u64);

    pub const fn valid(self) -> bool {
        self.id().0 != VALUE_ID_INVALID
    }

    pub const fn id(self) -> ValueId {
        ValueId(self.0 as u32)
    }

    pub const fn ty(self) -> Type {
        match (self.0 >> VALUE_TYPE_SHIFT) as u8 {
            1 => Type::I32,
            2 => Type::I64,
            3 => Type::F32,
            4 => Type::F64,
            5 => Type::V128,
            _ => Type::Invalid,
        }
    }

    pub const fn with_type(self, ty: Type) -> Self {
        Self(self.0 | ((ty as u64) << VALUE_TYPE_SHIFT))
    }

    pub fn with_instruction_id(self, instruction_id: u32) -> Self {
        assert!(
            instruction_id < (1 << 28),
            "too large instruction ID: {instruction_id}"
        );
        Self(self.0 | (((instruction_id as u64) + 1) << VALUE_INSTRUCTION_SHIFT))
    }

    pub const fn instruction_id(self) -> Option<u32> {
        let stored = ((self.0 >> VALUE_INSTRUCTION_SHIFT) & VALUE_INSTRUCTION_MASK) as u32;
        if stored == 0 {
            None
        } else {
            Some(stored - 1)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Values(Vec<Value>);

impl Values {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn from_vec(values: Vec<Value>) -> Self {
        Self(values)
    }

    pub fn as_slice(&self) -> &[Value] {
        &self.0
    }

    pub fn as_mut_slice(&mut self) -> &mut [Value] {
        &mut self.0
    }

    pub fn push(&mut self, value: Value) {
        self.0.push(value);
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = Value> + '_ {
        self.0.iter().copied()
    }
}

impl IntoIterator for Values {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::{Value, ValueId, Variable};
    use crate::ssa::types::Type;

    #[test]
    fn value_instruction_id_encoding_matches_go_layout() {
        let v = Value(1234).with_type(Type::I32).with_instruction_id(5678);
        assert_eq!(v.id(), ValueId(1234));
        assert_eq!(v.instruction_id(), Some(5678));
        assert_eq!(v.ty(), Type::I32);
    }

    #[test]
    fn variable_encodes_type_in_upper_bits() {
        let v = Variable(12).with_type(Type::F64);
        assert_eq!(v.ty(), Type::F64);
        assert_eq!(v.to_string(), "var12");
    }
}
