#![doc = "Interpreter/runtime call signatures."]

use crate::compiler::{FunctionType, ValueType};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signature {
    pub params: Vec<ValueType>,
    pub results: Vec<ValueType>,
    pub param_slots: usize,
    pub result_slots: usize,
}

impl Signature {
    pub fn new(params: Vec<ValueType>, results: Vec<ValueType>) -> Self {
        let param_slots = slot_count(&params);
        let result_slots = slot_count(&results);
        Self {
            params,
            results,
            param_slots,
            result_slots,
        }
    }

    pub fn from_function_type(ty: &FunctionType) -> Self {
        Self::new(ty.params.clone(), ty.results.clone())
    }

    pub fn stack_window_len(&self) -> usize {
        self.param_slots.max(self.result_slots)
    }

    pub fn matches_function_type(&self, ty: &FunctionType) -> bool {
        self.params == ty.params && self.results == ty.results
    }
}

impl From<&FunctionType> for Signature {
    fn from(value: &FunctionType) -> Self {
        Self::from_function_type(value)
    }
}

impl From<FunctionType> for Signature {
    fn from(value: FunctionType) -> Self {
        Self::from_function_type(&value)
    }
}

fn slot_count(types: &[ValueType]) -> usize {
    types
        .iter()
        .map(|ty| usize::from(matches!(ty, ValueType::V128)) + 1)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::Signature;
    use crate::compiler::{FunctionType, ValueType};

    #[test]
    fn counts_scalar_and_vector_slots() {
        let signature = Signature::new(
            vec![ValueType::I32, ValueType::V128],
            vec![ValueType::V128, ValueType::I64],
        );

        assert_eq!(3, signature.param_slots);
        assert_eq!(3, signature.result_slots);
        assert_eq!(3, signature.stack_window_len());
    }

    #[test]
    fn converts_from_function_type() {
        let ty = FunctionType::new(vec![ValueType::I64], vec![ValueType::I32]);
        let signature = Signature::from(&ty);

        assert!(signature.matches_function_type(&ty));
        assert_eq!(1, signature.param_slots);
        assert_eq!(1, signature.result_slots);
    }
}
