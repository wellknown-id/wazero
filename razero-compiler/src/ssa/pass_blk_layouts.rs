use crate::ssa::basic_block::{BasicBlockId, BasicBlockPredecessorInfo};
use crate::ssa::builder::Builder;
use crate::ssa::instructions::{Instruction, InstructionId, Opcode};

pub(crate) fn pass_layout_blocks(builder: &mut Builder) {
    for index in 0..builder.block_id_max().0 {
        builder.block_mut(BasicBlockId(index)).visited = 0;
    }

    let mut non_split_blocks = Vec::new();
    let original_order = builder.reverse_post_ordered_blocks.clone();
    for (index, block) in original_order.iter().copied().enumerate() {
        if !builder.block(block).valid() {
            continue;
        }
        non_split_blocks.push(block);
        if index + 1 < original_order.len() {
            let _ = maybe_invert_branches(builder, block, original_order[index + 1]);
        }
    }

    builder.reverse_post_ordered_blocks.clear();
    let mut uninserted_trampolines = Vec::new();
    for block in non_split_blocks {
        let preds = builder.block(block).preds.clone();
        for pred in preds {
            if !builder.block(pred.block).valid() || builder.block(pred.block).visited == 1 {
                continue;
            }
            if builder.block(pred.block).reverse_post_order
                < builder.block(block).reverse_post_order
            {
                builder.reverse_post_ordered_blocks.push(pred.block);
                builder.block_mut(pred.block).visited = 1;
            }
        }

        builder.reverse_post_ordered_blocks.push(block);
        builder.block_mut(block).visited = 1;

        let tail = builder.block(block).tail_instr;
        if builder.block(block).succs.len() < 2
            || tail.is_some_and(|instr| builder.instruction(instr).opcode == Opcode::BrTable)
        {
            continue;
        }

        let succs = builder.block(block).succs.clone();
        for (succ_index, succ) in succs.into_iter().enumerate() {
            if !succ.is_return() && builder.block(succ).preds.len() < 2 {
                continue;
            }
            let pred_info_index = builder
                .block(succ)
                .preds
                .iter()
                .position(|pred| pred.block == block);
            let pred_info_index =
                pred_info_index.expect("BUG: predecessor info not found while successor exists");
            let trampoline = split_critical_edge(builder, block, succ, pred_info_index);
            builder.block_mut(block).succs[succ_index] = trampoline;

            let fallthrough_branch = builder
                .block(block)
                .tail_instr
                .expect("critical edge needs tail");
            let fallthrough_target =
                BasicBlockId(builder.instruction(fallthrough_branch).r_value.0 as u32);
            if builder.instruction(fallthrough_branch).opcode == Opcode::Jump
                && fallthrough_target == trampoline
            {
                builder.reverse_post_ordered_blocks.push(trampoline);
                builder.block_mut(trampoline).visited = 1;
            } else {
                uninserted_trampolines.push(trampoline);
            }
        }

        for trampoline in &uninserted_trampolines {
            let target = builder.block(*trampoline).succs[0];
            if builder.block(target).reverse_post_order
                <= builder.block(*trampoline).reverse_post_order
            {
                builder.reverse_post_ordered_blocks.push(*trampoline);
                builder.block_mut(*trampoline).visited = 1;
            }
        }
        uninserted_trampolines.clear();
    }
}

pub(crate) fn mark_fallthrough_jumps(_builder: &mut Builder) {}

pub(crate) fn maybe_invert_branches(
    builder: &mut Builder,
    now: BasicBlockId,
    next_in_rpo: BasicBlockId,
) -> bool {
    let fallthrough_branch = match builder.block(now).tail_instr {
        Some(instr) if builder.instruction(instr).opcode != Opcode::BrTable => instr,
        _ => return false,
    };
    let cond_branch = match builder.instruction(fallthrough_branch).prev {
        Some(instr)
            if matches!(
                builder.instruction(instr).opcode,
                Opcode::Brz | Opcode::Brnz
            ) =>
        {
            instr
        }
        _ => return false,
    };

    if !builder.instruction(fallthrough_branch).vs.is_empty()
        || !builder.instruction(cond_branch).vs.is_empty()
    {
        return false;
    }

    let fallthrough_target = BasicBlockId(builder.instruction(fallthrough_branch).r_value.0 as u32);
    let cond_target = BasicBlockId(builder.instruction(cond_branch).r_value.0 as u32);

    let should_invert = if builder.block(fallthrough_target).loop_header {
        false
    } else if builder.block(cond_target).loop_header {
        true
    } else if fallthrough_target == next_in_rpo {
        false
    } else {
        cond_target == next_in_rpo
    };
    if !should_invert {
        return false;
    }

    if let Some(pred) = builder
        .block_mut(fallthrough_target)
        .preds
        .iter_mut()
        .find(|pred| pred.branch == fallthrough_branch)
    {
        pred.branch = cond_branch;
    }
    if let Some(pred) = builder
        .block_mut(cond_target)
        .preds
        .iter_mut()
        .find(|pred| pred.branch == cond_branch)
    {
        pred.branch = fallthrough_branch;
    }

    builder.instruction_mut(cond_branch).invert_brx();
    builder.instruction_mut(cond_branch).r_value =
        crate::ssa::vs::Value(fallthrough_target.0 as u64);
    builder.instruction_mut(fallthrough_branch).r_value =
        crate::ssa::vs::Value(cond_target.0 as u64);
    true
}

pub(crate) fn split_critical_edge(
    builder: &mut Builder,
    pred: BasicBlockId,
    succ: BasicBlockId,
    pred_info_index: usize,
) -> BasicBlockId {
    let trampoline = builder.allocate_basic_block();
    if builder.dominators.len() <= trampoline.0 as usize {
        builder.dominators.resize(trampoline.0 as usize + 1, None);
    }
    builder.dominators[trampoline.0 as usize] = Some(pred);

    let original_branch = builder.block(succ).preds[pred_info_index].branch;
    let original_opcode = builder.instruction(original_branch).opcode;
    let original_cond = builder.instruction(original_branch).v;

    let mut new_branch = Instruction::new();
    new_branch.id = Some(InstructionId(builder.instructions.len() as u32));
    new_branch.opcode = original_opcode;
    new_branch.r_value = crate::ssa::vs::Value(trampoline.0 as u64);
    if matches!(original_opcode, Opcode::Brz | Opcode::Brnz) {
        new_branch.v = original_cond;
        builder.instruction_mut(original_branch).opcode = Opcode::Jump;
        builder.instruction_mut(original_branch).v = crate::ssa::vs::Value::INVALID;
    }
    let new_branch_id = new_branch.id.expect("new branch id must exist");
    builder.instructions.push(new_branch);
    swap_instruction(builder, pred, original_branch, new_branch_id);

    {
        let pred_rpo = builder.block(pred).reverse_post_order;
        let trampoline_block = builder.block_mut(trampoline);
        trampoline_block.root_instr = Some(original_branch);
        trampoline_block.tail_instr = Some(original_branch);
        trampoline_block.succs.push(succ);
        trampoline_block.preds.push(BasicBlockPredecessorInfo {
            block: pred,
            branch: new_branch_id,
        });
        trampoline_block.reverse_post_order = pred_rpo;
    }
    builder.seal(trampoline);

    {
        let succ_block = builder.block_mut(succ);
        succ_block.preds[pred_info_index].block = trampoline;
        succ_block.preds[pred_info_index].branch = original_branch;
    }
    trampoline
}

pub(crate) fn swap_instruction(
    builder: &mut Builder,
    block: BasicBlockId,
    old: InstructionId,
    new_instr: InstructionId,
) {
    let old_prev = builder.instruction(old).prev;
    let old_next = builder.instruction(old).next;
    {
        let new_inst = builder.instruction_mut(new_instr);
        new_inst.prev = old_prev;
        new_inst.next = old_next;
    }

    if builder.block(block).root_instr == Some(old) {
        builder.block_mut(block).root_instr = Some(new_instr);
    }
    if builder.block(block).tail_instr == Some(old) {
        builder.block_mut(block).tail_instr = Some(new_instr);
    }
    if let Some(prev) = old_prev {
        builder.instruction_mut(prev).next = Some(new_instr);
    }
    if let Some(next) = old_next {
        builder.instruction_mut(next).prev = Some(new_instr);
    }
    builder.instruction_mut(old).prev = None;
    builder.instruction_mut(old).next = None;
}

#[cfg(test)]
mod tests {
    use crate::ssa::basic_block::BasicBlockId;
    use crate::ssa::builder::Builder;
    use crate::ssa::instructions::{Instruction, Opcode};
    use crate::ssa::signature::{Signature, SignatureId};
    use crate::ssa::vs::{Value, Values};

    use super::{maybe_invert_branches, pass_layout_blocks, split_critical_edge, swap_instruction};

    fn insert_jump(builder: &mut Builder, src: BasicBlockId, dst: BasicBlockId, args: Values) {
        builder.set_current_block(src);
        builder.insert_instruction(builder.allocate_instruction().as_jump(args, dst));
    }

    fn insert_brz(
        builder: &mut Builder,
        src: BasicBlockId,
        dst: BasicBlockId,
        cond: Value,
        args: Values,
    ) {
        builder.set_current_block(src);
        builder.insert_instruction(builder.allocate_instruction().as_brz(cond, args, dst));
    }

    #[test]
    fn inverts_branch_when_conditional_target_is_next() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let now = builder.allocate_basic_block();
        let next = builder.allocate_basic_block();
        let other = builder.allocate_basic_block();
        builder.set_current_block(now);
        let cond = builder.insert_instruction(builder.allocate_instruction().as_iconst32(0));
        let cond = builder.instruction(cond).return_();
        insert_brz(&mut builder, now, next, cond, Values::new());
        insert_jump(&mut builder, now, other, Values::new());
        assert!(maybe_invert_branches(&mut builder, now, next));
        let tail = builder.block(now).tail_instr.expect("tail");
        let cond_branch = builder.instruction(tail).prev.expect("conditional branch");
        assert_eq!(builder.instruction(cond_branch).opcode, Opcode::Brnz);
        assert_eq!(
            BasicBlockId(builder.instruction(tail).r_value.0 as u32),
            next
        );
        assert_eq!(
            BasicBlockId(builder.instruction(cond_branch).r_value.0 as u32),
            other
        );
    }

    #[test]
    fn split_critical_edge_inserts_trampoline() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let pred = builder.allocate_basic_block();
        let succ = builder.allocate_basic_block();
        let other = builder.allocate_basic_block();
        builder.set_current_block(pred);
        let cond = builder.insert_instruction(builder.allocate_instruction().as_iconst32(1));
        let cond = builder.instruction(cond).return_();
        insert_brz(&mut builder, pred, succ, cond, Values::new());
        insert_jump(&mut builder, pred, other, Values::new());
        builder.block_mut(pred).reverse_post_order = 7;
        let trampoline = split_critical_edge(&mut builder, pred, succ, 0);
        assert_eq!(builder.block(trampoline).reverse_post_order, 7);
        assert_eq!(builder.block(trampoline).succs, vec![succ]);
        let replaced = builder
            .instruction(builder.block(pred).root_instr.unwrap())
            .next
            .unwrap();
        assert_eq!(builder.instruction(replaced).opcode, Opcode::Brz);
        assert_eq!(
            BasicBlockId(builder.instruction(replaced).r_value.0 as u32),
            trampoline
        );
        assert_eq!(builder.block(succ).preds[0].block, trampoline);
    }

    #[test]
    fn swap_instruction_updates_root_tail_and_links() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let block = builder.allocate_basic_block();
        builder.set_current_block(block);
        let first = builder.insert_instruction(builder.allocate_instruction().as_iconst32(1));
        let second = builder.insert_instruction(builder.allocate_instruction().as_iconst32(2));
        let mut replacement = Instruction::new().as_iconst32(99);
        replacement.id = Some(crate::ssa::instructions::InstructionId(
            builder.instructions.len() as u32,
        ));
        let replacement_id = replacement.id.unwrap();
        builder.instructions.push(replacement);
        swap_instruction(&mut builder, block, second, replacement_id);
        assert_eq!(builder.block(block).tail_instr, Some(replacement_id));
        assert_eq!(builder.instruction(first).next, Some(replacement_id));
        assert_eq!(builder.instruction(replacement_id).prev, Some(first));
    }

    #[test]
    fn layout_blocks_places_loop_header_trampoline_in_hot_path() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let b0 = builder.allocate_basic_block();
        let b1 = builder.allocate_basic_block();
        let b2 = builder.allocate_basic_block();
        let b3 = builder.allocate_basic_block();
        insert_jump(&mut builder, b0, b1, Values::new());
        insert_jump(&mut builder, b1, b2, Values::new());
        builder.set_current_block(b2);
        let cond = builder.insert_instruction(builder.allocate_instruction().as_iconst32(0));
        let cond = builder.instruction(cond).return_();
        insert_brz(&mut builder, b2, b3, cond, Values::new());
        insert_jump(&mut builder, b2, b1, Values::new());
        builder.seal(b0);
        builder.seal(b1);
        builder.seal(b2);
        builder.seal(b3);
        builder.reverse_post_ordered_blocks = vec![b0, b1, b2, b3];
        builder.block_mut(b0).reverse_post_order = 0;
        builder.block_mut(b1).reverse_post_order = 1;
        builder.block_mut(b2).reverse_post_order = 2;
        builder.block_mut(b3).reverse_post_order = 3;
        builder.block_mut(b1).loop_header = true;
        pass_layout_blocks(&mut builder);
        assert_eq!(
            builder.reverse_post_ordered_blocks,
            vec![b0, b1, b2, BasicBlockId(4), b3]
        );
    }
}
