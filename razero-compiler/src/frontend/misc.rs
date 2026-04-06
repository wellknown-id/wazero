use crate::ssa::FuncRef;

pub fn function_index_to_func_ref(index: u32) -> FuncRef {
    FuncRef(index)
}
