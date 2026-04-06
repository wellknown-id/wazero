use std::cmp::Ordering;

use crate::ssa::basic_block::BasicBlockId;
use crate::ssa::builder::Builder;

pub(crate) fn sort_blocks(builder: &Builder, blocks: &mut [BasicBlockId]) {
    blocks.sort_by(|lhs, rhs| compare_blocks(builder, *lhs, *rhs));
}

fn compare_blocks(builder: &Builder, lhs: BasicBlockId, rhs: BasicBlockId) -> Ordering {
    let lhs_is_return = lhs.is_return();
    let rhs_is_return = rhs.is_return();
    if lhs_is_return && rhs_is_return {
        return Ordering::Equal;
    }
    if rhs_is_return {
        return Ordering::Greater;
    }
    if lhs_is_return {
        return Ordering::Less;
    }

    let lhs_root = builder.block(lhs).root_instr;
    let rhs_root = builder.block(rhs).root_instr;
    match (lhs_root, rhs_root) {
        (None, None) => Ordering::Equal,
        (_, None) => Ordering::Greater,
        (None, _) => Ordering::Less,
        (Some(lhs_root), Some(rhs_root)) => lhs_root.0.cmp(&rhs_root.0),
    }
}

#[cfg(test)]
mod tests {
    use crate::ssa::basic_block::BASIC_BLOCK_ID_RETURN_BLOCK;
    use crate::ssa::builder::Builder;
    use crate::ssa::instructions::InstructionId;
    use crate::ssa::signature::{Signature, SignatureId};

    use super::sort_blocks;

    #[test]
    fn sorts_by_return_then_instruction_order() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let b0 = builder.allocate_basic_block();
        let b1 = builder.allocate_basic_block();
        let b2 = builder.allocate_basic_block();
        builder.block_mut(b0).root_instr = Some(InstructionId(9));
        builder.block_mut(b1).root_instr = Some(InstructionId(2));
        let mut blocks = vec![b0, BASIC_BLOCK_ID_RETURN_BLOCK, b2, b1];
        sort_blocks(&builder, &mut blocks);
        assert_eq!(blocks, vec![BASIC_BLOCK_ID_RETURN_BLOCK, b2, b1, b0]);
    }
}
