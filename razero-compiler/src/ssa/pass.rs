use crate::ssa::basic_block::BasicBlockId;
use crate::ssa::basic_block_sort::sort_blocks;
use crate::ssa::builder::Builder;
use crate::ssa::instructions::{InstructionGroupId, InstructionId, Opcode, SideEffect};
use crate::ssa::pass_blk_layouts::{mark_fallthrough_jumps, pass_layout_blocks};
use crate::ssa::pass_cfg::{
    pass_build_dominator_tree, pass_build_loop_nesting_forest, pass_calculate_immediate_dominators,
};
use crate::ssa::vs::{Value, Values};

#[derive(Clone, Copy)]
struct RedundantParam {
    index: usize,
    unique_value: Value,
}

pub(crate) fn run_passes(builder: &mut Builder) {
    run_pre_block_layout_passes(builder);
    run_block_layout_pass(builder);
    run_post_block_layout_passes(builder);
    run_finalizing_passes(builder);
}

pub(crate) fn run_pre_block_layout_passes(builder: &mut Builder) {
    pass_sort_successors(builder);
    pass_dead_block_elimination_opt(builder);
    pass_calculate_immediate_dominators(builder);
    pass_redundant_phi_elimination_opt(builder);
    pass_nop_inst_elimination(builder);
    pass_dead_code_elimination_opt(builder);
    builder.done_pre_block_layout_passes = true;
}

pub(crate) fn run_block_layout_pass(builder: &mut Builder) {
    assert!(
        builder.done_pre_block_layout_passes,
        "run_block_layout_pass must follow pre block layout passes"
    );
    pass_layout_blocks(builder);
    builder.done_block_layout = true;
}

pub(crate) fn run_post_block_layout_passes(builder: &mut Builder) {
    assert!(
        builder.done_block_layout,
        "run_post_block_layout_passes must follow block layout"
    );
    builder.done_post_block_layout_passes = true;
}

pub(crate) fn run_finalizing_passes(builder: &mut Builder) {
    assert!(
        builder.done_post_block_layout_passes,
        "run_finalizing_passes must follow post block layout passes"
    );
    pass_build_loop_nesting_forest(builder);
    pass_build_dominator_tree(builder);
    mark_fallthrough_jumps(builder);
}

pub(crate) fn pass_dead_block_elimination_opt(builder: &mut Builder) {
    let entry = builder
        .entry_block()
        .expect("entry block must exist before dead block elimination");
    let mut stack = vec![entry];
    while let Some(block) = stack.pop() {
        if builder.block(block).visited == 1 {
            continue;
        }
        if !builder.block(block).sealed && !block.is_return() {
            panic!("{block} is not sealed");
        }
        builder.block_mut(block).visited = 1;
        let succs = builder.block(block).succs.clone();
        for succ in succs {
            if succ.is_return() || builder.block(succ).visited == 1 {
                continue;
            }
            stack.push(succ);
        }
    }

    for index in 0..builder.block_id_max().0 {
        let block = BasicBlockId(index);
        let reachable = builder.block(block).visited == 1;
        builder.block_mut(block).invalid = !reachable;
        builder.block_mut(block).visited = 0;
    }
}

pub(crate) fn pass_redundant_phi_elimination_opt(builder: &mut Builder) {
    let mut redundant_params = Vec::new();
    loop {
        let mut changed = false;
        let order = builder.reverse_post_ordered_blocks.clone();
        let mut iter = order.into_iter();
        let _ = iter.next();
        for block in iter {
            redundant_params.clear();
            let params = builder.block(block).params.as_slice().to_vec();
            let preds = builder.block(block).preds.clone();
            for (param_index, phi_value) in params.iter().copied().enumerate() {
                let mut redundant = true;
                let mut non_self = Value::INVALID;
                for pred in &preds {
                    builder.resolve_argument_aliases(pred.branch);
                    let pred_value = builder.instruction(pred.branch).vs.as_slice()[param_index];
                    if pred_value == phi_value {
                        continue;
                    }
                    if !non_self.valid() {
                        non_self = pred_value;
                        continue;
                    }
                    if non_self != pred_value {
                        redundant = false;
                        break;
                    }
                }
                if !non_self.valid() {
                    panic!("BUG: params added but only self-referencing");
                }
                if redundant {
                    redundant_params.push(RedundantParam {
                        index: param_index,
                        unique_value: non_self,
                    });
                }
            }
            if redundant_params.is_empty() {
                continue;
            }

            changed = true;
            for pred in &preds {
                let args = builder.instruction(pred.branch).vs.as_slice().to_vec();
                let mut kept =
                    Vec::with_capacity(args.len().saturating_sub(redundant_params.len()));
                for (arg_index, value) in args.into_iter().enumerate() {
                    if redundant_params
                        .iter()
                        .any(|redundant| redundant.index == arg_index)
                    {
                        continue;
                    }
                    kept.push(value);
                }
                builder.instruction_mut(pred.branch).vs = Values::from_vec(kept);
            }

            for redundant in &redundant_params {
                builder.alias(params[redundant.index], redundant.unique_value);
            }

            let filtered_params = params
                .into_iter()
                .enumerate()
                .filter_map(|(param_index, value)| {
                    (!redundant_params
                        .iter()
                        .any(|redundant| redundant.index == param_index))
                    .then_some(value)
                })
                .collect::<Vec<_>>();
            builder.block_mut(block).params = Values::from_vec(filtered_params);
        }
        if !changed {
            break;
        }
    }
}

pub(crate) fn pass_dead_code_elimination_opt(builder: &mut Builder) {
    let value_count = builder.next_value_id as usize;
    if builder.values_info.len() < value_count {
        builder.values_info.resize(value_count, Default::default());
    }
    for info in &mut builder.values_info {
        info.ref_count = 0;
    }
    for index in 0..builder.instructions.len() {
        builder.instructions[index].live = false;
        builder.instructions[index].gid = InstructionGroupId(0);
    }

    let mut live_instructions = Vec::new();
    let mut gid = InstructionGroupId(0);
    for index in 0..builder.block_id_max().0 {
        let block = BasicBlockId(index);
        if !builder.block(block).valid() {
            continue;
        }
        let mut cur = builder.block(block).root_instr;
        while let Some(instr_id) = cur {
            builder.instruction_mut(instr_id).gid = gid;
            match builder.instruction(instr_id).side_effect() {
                SideEffect::Strict => {
                    live_instructions.push(instr_id);
                    gid.0 += 1;
                }
                SideEffect::Traps => live_instructions.push(instr_id),
                SideEffect::None => {}
            }
            cur = builder.instruction(instr_id).next;
        }
    }

    while let Some(live) = live_instructions.pop() {
        if builder.instruction(live).live {
            continue;
        }
        builder.instruction_mut(live).live = true;
        builder.resolve_argument_aliases(live);
        let (v1, v2, v3, vs) = {
            let instr = builder.instruction(live);
            (instr.v, instr.v2, instr.v3, instr.vs.as_slice().to_vec())
        };
        for value in [v1, v2, v3] {
            if value.valid() {
                if let Some(id) = value.instruction_id() {
                    live_instructions.push(InstructionId(id));
                }
            }
        }
        for value in vs {
            if let Some(id) = value.instruction_id() {
                live_instructions.push(InstructionId(id));
            }
        }
    }

    for index in 0..builder.block_id_max().0 {
        let block = BasicBlockId(index);
        if !builder.block(block).valid() {
            continue;
        }
        let mut cur = builder.block(block).root_instr;
        while let Some(instr_id) = cur {
            let next = builder.instruction(instr_id).next;
            if !builder.instruction(instr_id).live {
                let prev = builder.instruction(instr_id).prev;
                if let Some(prev) = prev {
                    builder.instruction_mut(prev).next = next;
                } else {
                    builder.block_mut(block).root_instr = next;
                }
                if let Some(next_id) = next {
                    builder.instruction_mut(next_id).prev = prev;
                } else {
                    builder.block_mut(block).tail_instr = prev;
                }
            } else {
                let (v1, v2, v3, vs) = {
                    let instr = builder.instruction(instr_id);
                    (instr.v, instr.v2, instr.v3, instr.vs.as_slice().to_vec())
                };
                for value in [v1, v2, v3] {
                    if value.valid() {
                        inc_ref_count(builder, value);
                    }
                }
                for value in vs {
                    inc_ref_count(builder, value);
                }
            }
            cur = next;
        }
    }
}

fn inc_ref_count(builder: &mut Builder, value: Value) {
    let index = value.id().0 as usize;
    if index >= builder.values_info.len() {
        builder.values_info.resize(index + 1, Default::default());
    }
    builder.values_info[index].ref_count += 1;
}

pub(crate) fn pass_nop_inst_elimination(builder: &mut Builder) {
    for index in 0..builder.block_id_max().0 {
        let block = BasicBlockId(index);
        if !builder.block(block).valid() {
            continue;
        }
        let mut cur = builder.block(block).root_instr;
        while let Some(instr_id) = cur {
            let next = builder.instruction(instr_id).next;
            let opcode = builder.instruction(instr_id).opcode;
            if matches!(opcode, Opcode::Ishl | Opcode::Sshr | Opcode::Ushr) {
                let (x, amount) = {
                    let instr = builder.instruction(instr_id);
                    (instr.v, instr.v2)
                };
                if let Some(def_inst) = amount.instruction_id().map(InstructionId) {
                    let defining = builder.instruction(def_inst);
                    if defining.opcode == Opcode::Iconst {
                        let mut amount = defining.iconst_data();
                        amount %= if x.ty().bits() == 64 { 64 } else { 32 };
                        if amount == 0 {
                            builder.alias(builder.instruction(instr_id).return_(), x);
                        }
                    }
                }
            }
            cur = next;
        }
    }
}

pub(crate) fn pass_sort_successors(builder: &mut Builder) {
    for index in 0..builder.block_id_max().0 {
        let block = BasicBlockId(index);
        let mut succs = builder.block(block).succs.clone();
        sort_blocks(builder, &mut succs);
        builder.block_mut(block).succs = succs;
    }
}

#[cfg(test)]
mod tests {
    use crate::ssa::builder::Builder;
    use crate::ssa::instructions::Opcode;
    use crate::ssa::signature::{Signature, SignatureId};
    use crate::ssa::types::Type;
    use crate::ssa::vs::Values;

    use super::{
        pass_dead_block_elimination_opt, pass_dead_code_elimination_opt, pass_nop_inst_elimination,
        pass_redundant_phi_elimination_opt, pass_sort_successors,
    };
    use crate::ssa::pass_cfg::pass_calculate_immediate_dominators;

    #[test]
    fn dead_block_elimination_marks_unreachable_block_invalid() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let entry = builder.allocate_basic_block();
        let reachable = builder.allocate_basic_block();
        let unreachable = builder.allocate_basic_block();
        builder.set_current_block(entry);
        builder.insert_instruction(
            builder
                .allocate_instruction()
                .as_jump(Values::new(), reachable),
        );
        builder.set_current_block(unreachable);
        builder.insert_instruction(
            builder
                .allocate_instruction()
                .as_jump(Values::new(), reachable),
        );
        builder.seal(entry);
        builder.seal(reachable);
        builder.seal(unreachable);
        pass_dead_block_elimination_opt(&mut builder);
        assert!(builder.block(entry).valid());
        assert!(builder.block(reachable).valid());
        assert!(!builder.block(unreachable).valid());
    }

    #[test]
    fn redundant_phi_elimination_keeps_single_param_and_aliases_branch_use() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let entry = builder.allocate_basic_block();
        let loop_header = builder.allocate_basic_block();
        let end = builder.allocate_basic_block();
        let var = builder.declare_variable(Type::I32);
        let live_param = builder.allocate_value(Type::I32);
        builder.block_mut(loop_header).add_param(live_param);

        builder.set_current_block(entry);
        let iconst = builder.insert_instruction(builder.allocate_instruction().as_iconst32(0xff));
        let iconst = builder.instruction(iconst).return_();
        builder.define_variable(var, iconst, entry);
        let mut args = Values::new();
        args.push(iconst);
        builder.insert_instruction(builder.allocate_instruction().as_jump(args, loop_header));
        builder.seal(entry);

        builder.set_current_block(loop_header);
        let phi = builder.must_find_value(var);
        let tmp = builder.insert_instruction(builder.allocate_instruction().as_iconst32(0xff));
        let tmp = builder.instruction(tmp).return_();
        let mut loop_args = Values::new();
        loop_args.push(tmp);
        loop_args.push(phi);
        builder.insert_instruction(builder.allocate_instruction().as_brz(
            phi,
            loop_args,
            loop_header,
        ));
        builder.insert_instruction(builder.allocate_instruction().as_jump(Values::new(), end));
        builder.seal(loop_header);

        builder.set_current_block(end);
        builder.insert_instruction(builder.allocate_instruction().as_return(Values::new()));

        pass_calculate_immediate_dominators(&mut builder);
        pass_redundant_phi_elimination_opt(&mut builder);
        assert_eq!(builder.block(loop_header).params_len(), 1);
        assert_eq!(builder.resolve_alias(phi), iconst);
        let branch = builder
            .block(loop_header)
            .preds
            .iter()
            .find(|pred| pred.block == entry)
            .expect("entry predecessor must remain")
            .branch;
        assert_eq!(builder.instruction(branch).vs.as_slice().len(), 1);
    }

    #[test]
    fn dead_code_elimination_removes_unused_instructions_and_counts_refs() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let entry = builder.allocate_basic_block();
        let end = builder.allocate_basic_block();
        builder.set_current_block(entry);
        let live_const = builder.insert_instruction(builder.allocate_instruction().as_iconst32(3));
        let live_const = builder.instruction(live_const).return_();

        let mut store = builder.allocate_instruction();
        store.opcode = Opcode::Store;
        store.v = live_const;
        store.v2 = live_const;
        builder.insert_instruction(store);

        let dead_const = builder.insert_instruction(builder.allocate_instruction().as_iconst32(0));
        let kept_const = builder.insert_instruction(builder.allocate_instruction().as_iconst32(1));
        let kept_const = builder.instruction(kept_const).return_();
        builder.insert_instruction(builder.allocate_instruction().as_jump(Values::new(), end));

        builder.set_current_block(end);
        let add = builder.insert_instruction(
            builder
                .allocate_instruction()
                .as_iadd(kept_const, live_const),
        );
        let add_value = builder.instruction(add).return_();
        let mut ret_args = Values::new();
        ret_args.push(add_value);
        builder.insert_instruction(builder.allocate_instruction().as_return(ret_args));

        pass_dead_code_elimination_opt(&mut builder);
        assert!(!builder.instruction(dead_const).live);
        assert_eq!(
            builder.values_info()[kept_const.id().0 as usize].ref_count,
            1
        );
        assert_eq!(
            builder.values_info()[live_const.id().0 as usize].ref_count,
            3
        );
    }

    #[test]
    fn nop_shift_elimination_aliases_zero_shift_amounts() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let entry = builder.allocate_basic_block();
        builder.set_current_block(entry);
        let x = builder.insert_instruction(builder.allocate_instruction().as_iconst32(1));
        let x = builder.instruction(x).return_();
        let amount = builder.insert_instruction(builder.allocate_instruction().as_iconst32(32 * 3));
        let amount = builder.instruction(amount).return_();
        let mut ishl = builder.allocate_instruction();
        ishl.opcode = Opcode::Ishl;
        ishl.typ = Type::I32;
        ishl.v = x;
        ishl.v2 = amount;
        let ishl = builder.insert_instruction(ishl);
        let ishl_value = builder.instruction(ishl).return_();
        pass_nop_inst_elimination(&mut builder);
        assert_eq!(builder.resolve_alias(ishl_value), x);
    }

    #[test]
    fn successor_sorting_uses_natural_block_order() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let b0 = builder.allocate_basic_block();
        let b1 = builder.allocate_basic_block();
        let b2 = builder.allocate_basic_block();
        builder.block_mut(b0).succs = vec![b1, b2];
        builder.block_mut(b1).root_instr = Some(crate::ssa::InstructionId(9));
        builder.block_mut(b2).root_instr = Some(crate::ssa::InstructionId(3));
        pass_sort_successors(&mut builder);
        assert_eq!(builder.block(b0).succs, vec![b2, b1]);
    }
}
