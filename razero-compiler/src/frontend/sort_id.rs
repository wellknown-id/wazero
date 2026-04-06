use crate::ssa::ValueId;

pub fn sort_ssa_value_ids(ids: &mut [ValueId]) {
    ids.sort_unstable();
}
