use crate::ssa::Signature;

pub fn go_function_call_required_stack_size(sig: &Signature, arg_begin: usize) -> (u64, u64) {
    let param_needed = sig
        .params
        .iter()
        .skip(arg_begin)
        .map(|ty| u64::from(ty.size().max(8)))
        .sum::<u64>();
    let result_needed = sig
        .results
        .iter()
        .map(|ty| u64::from(ty.size().max(8)))
        .sum::<u64>();

    let unaligned = param_needed.max(result_needed);
    (((unaligned + 15) & !15), unaligned)
}

#[cfg(test)]
mod tests {
    use super::go_function_call_required_stack_size;
    use crate::ssa::{Signature, SignatureId, Type};

    #[test]
    fn go_function_stack_size_matches_largest_side_and_alignment() {
        let sig = Signature::new(
            SignatureId(0),
            vec![Type::I64, Type::V128, Type::I32],
            vec![Type::I64, Type::F64],
        );
        assert_eq!(go_function_call_required_stack_size(&sig, 0), (32, 32));
        assert_eq!(go_function_call_required_stack_size(&sig, 2), (16, 16));
    }
}
