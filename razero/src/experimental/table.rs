use crate::api::wasm::{Function, Module, ValueType};
use razero_wasm::module_instance_lookup::LookupError;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Table {
    minimum: u32,
    maximum: Option<u32>,
}

impl Table {
    pub fn new(minimum: u32, maximum: Option<u32>) -> Self {
        Self { minimum, maximum }
    }

    pub fn minimum(&self) -> u32 {
        self.minimum
    }

    pub fn maximum(&self) -> Option<u32> {
        self.maximum
    }
}

pub fn lookup_function(
    module: &Module,
    table_index: u32,
    table_offset: u32,
    expected_param_types: &[ValueType],
    expected_result_types: &[ValueType],
) -> Function {
    match module.lookup_table_function(
        table_index,
        table_offset,
        expected_param_types,
        expected_result_types,
    ) {
        Ok(function) => function,
        Err(LookupError::TableIndexOutOfBounds(_)) => panic!("table index out of range"),
        Err(LookupError::TableElementOutOfBounds { .. })
        | Err(LookupError::UninitializedTableElement { .. }) => panic!("invalid table access"),
        Err(LookupError::TypeMismatch { .. }) => panic!("indirect call type mismatch"),
        Err(err) => panic!("{err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::lookup_function;
    use crate::{api::wasm::ValueType, config::ModuleConfig, runtime::Runtime};

    fn instantiate_lookup_module() -> crate::api::wasm::Module {
        let runtime = Runtime::new();
        runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x0c, 0x02, 0x60, 0x00,
                    0x01, 0x7f, 0x60, 0x02, 0x7f, 0x7f, 0x02, 0x7f, 0x7f, 0x03, 0x03, 0x02, 0x00,
                    0x01, 0x04, 0x04, 0x01, 0x70, 0x00, 0x64, 0x09, 0x08, 0x01, 0x00, 0x41, 0x00,
                    0x0b, 0x02, 0x00, 0x01, 0x0a, 0x0d, 0x02, 0x04, 0x00, 0x41, 0x01, 0x0b, 0x06,
                    0x00, 0x20, 0x01, 0x20, 0x00, 0x0b,
                ],
                ModuleConfig::new(),
            )
            .unwrap()
    }

    #[test]
    fn lookup_function_reads_guest_table_entries() {
        let module = instantiate_lookup_module();

        let first = lookup_function(&module, 0, 0, &[], &[ValueType::I32]);
        assert_eq!(vec![1], first.call(&[]).unwrap());

        let second = lookup_function(
            &module,
            0,
            1,
            &[ValueType::I32, ValueType::I32],
            &[ValueType::I32, ValueType::I32],
        );
        assert_eq!(vec![200, 100], second.call(&[100, 200]).unwrap());
    }

    #[test]
    #[should_panic(expected = "invalid table access")]
    fn lookup_function_panics_on_out_of_bounds_element() {
        let module = instantiate_lookup_module();
        let _ = lookup_function(&module, 0, 2_000, &[], &[ValueType::I32]);
    }

    #[test]
    #[should_panic(expected = "table index out of range")]
    fn lookup_function_panics_on_out_of_bounds_table() {
        let module = instantiate_lookup_module();
        let _ = lookup_function(&module, 1_000, 0, &[], &[ValueType::I32]);
    }

    #[test]
    #[should_panic(expected = "indirect call type mismatch")]
    fn lookup_function_panics_on_mismatched_result_types() {
        let module = instantiate_lookup_module();
        let _ = lookup_function(&module, 0, 0, &[], &[ValueType::F32]);
    }

    #[test]
    #[should_panic(expected = "indirect call type mismatch")]
    fn lookup_function_panics_on_mismatched_param_arity() {
        let module = instantiate_lookup_module();
        let _ = lookup_function(&module, 0, 0, &[ValueType::I32], &[]);
    }

    #[test]
    #[should_panic(expected = "indirect call type mismatch")]
    fn lookup_function_panics_on_mismatched_result_arity() {
        let module = instantiate_lookup_module();
        let _ = lookup_function(&module, 0, 1, &[ValueType::I32, ValueType::I32], &[]);
    }

    #[test]
    #[should_panic(expected = "indirect call type mismatch")]
    fn lookup_function_panics_on_mismatched_result_type() {
        let module = instantiate_lookup_module();
        let _ = lookup_function(
            &module,
            0,
            1,
            &[ValueType::I32, ValueType::I32],
            &[ValueType::I32, ValueType::F32],
        );
    }
}
