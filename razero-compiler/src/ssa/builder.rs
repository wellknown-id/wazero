use std::collections::{BTreeMap, HashMap};

use crate::ssa::basic_block::{
    BasicBlock, BasicBlockData, BasicBlockId, BasicBlockPredecessorInfo, UnknownValue,
    BASIC_BLOCK_ID_RETURN_BLOCK,
};
use crate::ssa::instructions::{Instruction, InstructionId, SideEffect, SourceOffset};
use crate::ssa::pass_cfg::DominatorSparseTree;
use crate::ssa::signature::{Signature, SignatureId};
use crate::ssa::types::Type;
use crate::ssa::vs::{Value, ValueId, Values, Variable};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ValueInfo {
    pub ref_count: u32,
    pub alias: Option<Value>,
}

#[derive(Debug, Default)]
pub struct Builder {
    blocks: Vec<BasicBlockData>,
    pub(crate) instructions: Vec<Instruction>,
    signatures: BTreeMap<SignatureId, Signature>,
    current_signature: Option<Signature>,
    pub(crate) reverse_post_ordered_blocks: Vec<BasicBlockId>,
    current_bb: Option<BasicBlockId>,
    return_block: BasicBlockData,
    pub(crate) next_value_id: u32,
    next_variable: u32,
    value_annotations: HashMap<ValueId, String>,
    pub(crate) values_info: Vec<ValueInfo>,
    pub(crate) dominators: Vec<Option<BasicBlockId>>,
    pub(crate) loop_nesting_forest_roots: Vec<BasicBlockId>,
    pub(crate) sparse_tree: DominatorSparseTree,
    block_iter_cur: usize,
    pub(crate) done_pre_block_layout_passes: bool,
    pub(crate) done_block_layout: bool,
    pub(crate) done_post_block_layout_passes: bool,
    current_source_offset: SourceOffset,
    zeros: [Option<Value>; Type::COUNT],
}

impl Builder {
    pub fn new() -> Self {
        Self {
            return_block: BasicBlockData::new(BASIC_BLOCK_ID_RETURN_BLOCK),
            current_source_offset: SourceOffset::UNKNOWN,
            zeros: [None; Type::COUNT],
            ..Self::default()
        }
    }

    pub fn init(&mut self, signature: Signature) {
        self.blocks.clear();
        self.instructions.clear();
        self.current_signature = Some(signature);
        for sig in self.signatures.values_mut() {
            sig.used = false;
        }
        self.reverse_post_ordered_blocks.clear();
        self.current_bb = None;
        self.next_value_id = 0;
        self.next_variable = 0;
        self.value_annotations.clear();
        self.values_info.clear();
        self.dominators.clear();
        self.loop_nesting_forest_roots.clear();
        self.sparse_tree.clear();
        self.block_iter_cur = 0;
        self.done_pre_block_layout_passes = false;
        self.done_block_layout = false;
        self.done_post_block_layout_passes = false;
        self.current_source_offset = SourceOffset::UNKNOWN;
        self.zeros = [None; Type::COUNT];
        self.return_block = BasicBlockData::new(BASIC_BLOCK_ID_RETURN_BLOCK);
    }

    pub fn signature(&self) -> Option<&Signature> {
        self.current_signature.as_ref()
    }

    pub fn declare_signature(&mut self, signature: Signature) {
        self.signatures.insert(signature.id, signature);
    }

    pub fn signatures(&self) -> Vec<&Signature> {
        self.signatures.values().collect()
    }

    pub fn resolve_signature(&self, id: SignatureId) -> Option<&Signature> {
        self.signatures.get(&id)
    }

    pub fn allocate_basic_block(&mut self) -> BasicBlock {
        let id = BasicBlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlockData::new(id));
        id
    }

    pub fn block_id_max(&self) -> BasicBlockId {
        BasicBlockId(self.blocks.len() as u32)
    }

    pub fn block(&self, id: BasicBlockId) -> &BasicBlockData {
        if id == BASIC_BLOCK_ID_RETURN_BLOCK {
            &self.return_block
        } else {
            &self.blocks[id.0 as usize]
        }
    }

    pub fn block_mut(&mut self, id: BasicBlockId) -> &mut BasicBlockData {
        if id == BASIC_BLOCK_ID_RETURN_BLOCK {
            &mut self.return_block
        } else {
            &mut self.blocks[id.0 as usize]
        }
    }

    pub fn set_current_block(&mut self, bb: BasicBlock) {
        self.current_bb = Some(bb);
    }

    pub fn current_block(&self) -> Option<BasicBlock> {
        self.current_bb
    }

    pub fn entry_block(&self) -> Option<BasicBlock> {
        self.blocks.first().map(|b| b.id)
    }

    pub fn return_block(&self) -> BasicBlock {
        BASIC_BLOCK_ID_RETURN_BLOCK
    }

    pub fn declare_variable(&mut self, ty: Type) -> Variable {
        let v = Variable(self.next_variable).with_type(ty);
        self.next_variable += 1;
        v
    }

    pub fn define_variable(&mut self, variable: Variable, value: Value, block: BasicBlock) {
        self.block_mut(block)
            .last_definitions
            .insert(variable, value);
    }

    pub fn define_variable_in_current_bb(&mut self, variable: Variable, value: Value) {
        let current = self.current_bb.expect("current block must be set");
        self.define_variable(variable, value, current);
    }

    pub fn allocate_instruction(&self) -> Instruction {
        Instruction::new()
    }

    pub fn instruction(&self, id: InstructionId) -> &Instruction {
        &self.instructions[id.0 as usize]
    }

    pub fn instruction_mut(&mut self, id: InstructionId) -> &mut Instruction {
        &mut self.instructions[id.0 as usize]
    }

    pub fn instruction_of_value(&self, value: Value) -> Option<&Instruction> {
        value
            .instruction_id()
            .map(|id| self.instruction(InstructionId(id)))
    }

    pub fn set_current_source_offset(&mut self, offset: SourceOffset) {
        self.current_source_offset = offset;
    }

    pub fn insert_zero_value(&mut self, ty: Type) -> Value {
        if let Some(v) = self.zeros[ty.index()] {
            return v;
        }
        let instr = match ty {
            Type::I32 => self.allocate_instruction().as_iconst32(0),
            Type::I64 => self.allocate_instruction().as_iconst64(0),
            Type::F32 => self.allocate_instruction().as_f32const(0.0),
            Type::F64 => self.allocate_instruction().as_f64const(0.0),
            Type::V128 => self.allocate_instruction().as_vconst(0, 0),
            Type::Invalid => panic!("invalid zero type"),
        };
        let id = self.insert_instruction(instr);
        let zero = self.instruction(id).return_();
        self.zeros[ty.index()] = Some(zero);
        zero
    }

    pub fn insert_undefined(&mut self) -> InstructionId {
        self.insert_instruction(
            self.allocate_instruction()
                .with_opcode(crate::ssa::instructions::Opcode::Undefined),
        )
    }

    pub fn insert_instruction(&mut self, mut instr: Instruction) -> InstructionId {
        let block_id = self.current_bb.expect("current block must be set");
        let instr_id = InstructionId(self.instructions.len() as u32);
        instr.id = Some(instr_id);
        if self.current_source_offset.valid() && !matches!(instr.side_effect(), SideEffect::None) {
            instr.source_offset = self.current_source_offset;
        }
        let (first, rest) = instr.result_types(self);
        if let Some(first_ty) = first {
            instr.r_value = self
                .allocate_value(first_ty)
                .with_instruction_id(instr_id.0);
            if !rest.is_empty() {
                let mut values = Values::new();
                for ty in rest {
                    values.push(self.allocate_value(ty).with_instruction_id(instr_id.0));
                }
                instr.r_values = values;
            }
        }

        let prev = self.block(block_id).tail_instr;
        instr.prev = prev;
        if let Some(prev_id) = prev {
            self.instruction_mut(prev_id).next = Some(instr_id);
        } else {
            self.block_mut(block_id).root_instr = Some(instr_id);
        }
        self.block_mut(block_id).tail_instr = Some(instr_id);

        let opcode = instr.opcode;
        let target = if matches!(
            opcode,
            crate::ssa::instructions::Opcode::Jump
                | crate::ssa::instructions::Opcode::Brz
                | crate::ssa::instructions::Opcode::Brnz
        ) {
            Some(BasicBlockId(instr.r_value.0 as u32))
        } else {
            None
        };
        let br_table_targets = if matches!(opcode, crate::ssa::instructions::Opcode::BrTable) {
            Some(instr.r_values.as_slice().to_vec())
        } else {
            None
        };

        self.instructions.push(instr);

        if let Some(target) = target {
            self.add_pred(target, block_id, instr_id);
        }
        if let Some(targets) = br_table_targets {
            for target in targets {
                self.add_pred(BasicBlockId(target.0 as u32), block_id, instr_id);
            }
        }

        instr_id
    }

    fn add_pred(&mut self, target: BasicBlockId, pred: BasicBlockId, branch: InstructionId) {
        let target_data = self.block_mut(target);
        assert!(
            !target_data.sealed,
            "trying to add predecessor to a sealed block: {target}"
        );
        if target_data
            .preds
            .iter()
            .any(|existing| existing.block == pred && existing.branch != branch)
        {
            panic!("redundant non-BrTable jumps in {target}");
        }
        target_data.preds.push(BasicBlockPredecessorInfo {
            block: pred,
            branch,
        });
        self.block_mut(pred).succs.push(target);
    }

    pub fn allocate_value(&mut self, ty: Type) -> Value {
        let value = Value(self.next_value_id as u64).with_type(ty);
        self.next_value_id += 1;
        value
    }

    pub fn annotate_value(&mut self, value: Value, annotation: impl Into<String>) {
        self.value_annotations.insert(value.id(), annotation.into());
    }

    pub fn find_value_in_linear_path(&self, variable: Variable) -> Value {
        self.find_value_in_linear_path_from(
            variable,
            self.current_bb.expect("current block must be set"),
        )
    }

    fn find_value_in_linear_path_from(&self, variable: Variable, block: BasicBlockId) -> Value {
        let bb = self.block(block);
        if let Some(value) = bb.last_definitions.get(&variable) {
            *value
        } else if !bb.sealed {
            Value::INVALID
        } else if let Some(pred) = bb.single_pred {
            self.find_value_in_linear_path_from(variable, pred)
        } else {
            Value::INVALID
        }
    }

    pub fn must_find_value(&mut self, variable: Variable) -> Value {
        let current = self.current_bb.expect("current block must be set");
        self.find_value(variable.ty(), variable, current)
    }

    fn find_value(&mut self, ty: Type, variable: Variable, block: BasicBlockId) -> Value {
        if let Some(value) = self.block(block).last_definitions.get(&variable).copied() {
            return value;
        }
        if !self.block(block).sealed {
            let value = self.allocate_value(ty);
            let bb = self.block_mut(block);
            bb.last_definitions.insert(variable, value);
            bb.unknown_values.push(UnknownValue { variable, value });
            return value;
        }
        if block.is_entry() {
            return self.zeros[variable.ty().index()]
                .unwrap_or_else(|| self.insert_zero_value(variable.ty()));
        }
        if let Some(pred) = self.block(block).single_pred {
            return self.find_value(ty, variable, pred);
        }
        if self.block(block).preds.is_empty() {
            panic!("value is not defined for {variable}");
        }

        let tmp_value = self.allocate_value(ty);
        self.define_variable(variable, tmp_value, block);

        let pred_blocks = self
            .block(block)
            .preds
            .iter()
            .map(|p| p.block)
            .collect::<Vec<_>>();
        let mut unique_value = None;
        for pred in pred_blocks {
            let pred_value = self.find_value(ty, variable, pred);
            match unique_value {
                None => unique_value = Some(pred_value),
                Some(existing) if existing == pred_value => {}
                _ => {
                    unique_value = None;
                    break;
                }
            }
        }

        if let Some(unique) = unique_value {
            self.alias(tmp_value, unique);
            unique
        } else {
            self.block_mut(block).add_param(tmp_value);
            let preds = self.block(block).preds.clone();
            for pred in preds {
                let value = self.find_value(ty, variable, pred.block);
                self.instruction_mut(pred.branch)
                    .add_argument_branch_inst(value);
            }
            tmp_value
        }
    }

    pub fn seal(&mut self, block: BasicBlock) {
        let single_pred = {
            let bb = self.block(block);
            if bb.preds.len() == 1 {
                Some(bb.preds[0].block)
            } else {
                None
            }
        };
        {
            let bb = self.block_mut(block);
            bb.single_pred = single_pred;
            bb.sealed = true;
        }
        let unknowns = self.block(block).unknown_values.clone();
        for unknown in unknowns {
            self.block_mut(block).add_param(unknown.value);
            let preds = self.block(block).preds.clone();
            for pred in preds {
                let pred_value =
                    self.find_value(unknown.variable.ty(), unknown.variable, pred.block);
                assert!(
                    pred_value.valid(),
                    "value is not defined anywhere in the predecessors"
                );
                self.instruction_mut(pred.branch)
                    .add_argument_branch_inst(pred_value);
            }
        }
    }

    pub fn values_info(&self) -> &[ValueInfo] {
        &self.values_info
    }

    pub fn alias(&mut self, dst: Value, src: Value) {
        let index = dst.id().0 as usize;
        if index >= self.values_info.len() {
            self.values_info.resize(index + 1, ValueInfo::default());
        }
        self.values_info[index].alias = Some(src);
    }

    pub fn resolve_alias(&self, mut value: Value) -> Value {
        loop {
            let index = value.id().0 as usize;
            if index < self.values_info.len() {
                if let Some(alias) = self.values_info[index].alias {
                    value = alias;
                    continue;
                }
            }
            return value;
        }
    }

    pub fn resolve_argument_aliases(&mut self, instruction: InstructionId) {
        let mut args = {
            let instr = self.instruction(instruction);
            (instr.v, instr.v2, instr.v3, instr.vs.as_slice().to_vec())
        };
        if args.0.valid() {
            args.0 = self.resolve_alias(args.0);
        }
        if args.1.valid() {
            args.1 = self.resolve_alias(args.1);
        }
        if args.2.valid() {
            args.2 = self.resolve_alias(args.2);
        }
        for v in &mut args.3 {
            *v = self.resolve_alias(*v);
        }
        let instr = self.instruction_mut(instruction);
        instr.v = args.0;
        instr.v2 = args.1;
        instr.v3 = args.2;
        instr.vs = Values::from_vec(args.3);
    }

    pub fn block_iterator_begin(&mut self) -> Option<BasicBlockId> {
        self.block_iter_cur = 0;
        self.block_iterator_next()
    }

    pub fn block_iterator_next(&mut self) -> Option<BasicBlockId> {
        while self.block_iter_cur < self.blocks.len() {
            let block = self.blocks[self.block_iter_cur].id;
            self.block_iter_cur += 1;
            if self.block(block).valid() {
                return Some(block);
            }
        }
        None
    }

    pub fn block_iterator_reverse_post_order_begin(&mut self) -> Option<BasicBlockId> {
        self.block_iter_cur = 0;
        self.block_iterator_reverse_post_order_next()
    }

    pub fn block_iterator_reverse_post_order_next(&mut self) -> Option<BasicBlockId> {
        let block = self
            .reverse_post_ordered_blocks
            .get(self.block_iter_cur)
            .copied();
        if block.is_some() {
            self.block_iter_cur += 1;
        }
        block
    }

    pub fn loop_nesting_forest_roots(&self) -> &[BasicBlockId] {
        &self.loop_nesting_forest_roots
    }

    pub fn idom(&self, block: BasicBlockId) -> Option<BasicBlockId> {
        self.dominators.get(block.0 as usize).and_then(|v| *v)
    }

    pub fn run_passes(&mut self) {
        crate::ssa::pass::run_passes(self);
    }

    pub fn format(&self) -> String {
        let mut out = String::new();
        let used_signatures = self
            .signatures
            .values()
            .filter(|sig| sig.used)
            .collect::<Vec<_>>();
        if !used_signatures.is_empty() {
            out.push('\n');
            out.push_str("signatures:\n");
            for sig in used_signatures {
                out.push('\t');
                out.push_str(&sig.to_string());
                out.push('\n');
            }
        }
        let blocks: Vec<_> = if self.done_block_layout {
            self.reverse_post_ordered_blocks.clone()
        } else {
            self.blocks.iter().map(|b| b.id).collect()
        };
        for block in blocks {
            let bb = self.block(block);
            if !bb.valid() {
                continue;
            }
            out.push('\n');
            out.push_str(&self.format_block_header(block));
            out.push('\n');
            let mut cur = bb.root_instr;
            while let Some(instr_id) = cur {
                let instr = self.instruction(instr_id);
                out.push('\t');
                out.push_str(&instr.format(self));
                out.push('\n');
                cur = instr.next;
            }
        }
        out
    }

    pub fn format_value(&self, value: Value) -> String {
        self.value_annotations
            .get(&value.id())
            .cloned()
            .unwrap_or_else(|| format!("v{}", value.id().0))
    }

    pub fn format_value_with_type(&self, value: Value) -> String {
        let mut rendered = format!("{}:{}", self.format_value(value), value.ty());
        if self.done_post_block_layout_passes {
            if let Some(info) = self.values_info.get(value.id().0 as usize) {
                rendered.push_str(&format!("(ref={})", info.ref_count));
            }
        }
        rendered
    }

    pub fn format_block_header(&self, block: BasicBlockId) -> String {
        let bb = self.block(block);
        let params = bb
            .params
            .iter()
            .map(|p| self.format_value_with_type(p))
            .collect::<Vec<_>>()
            .join(", ");
        if bb.preds.is_empty() {
            format!("{block}: ({params})")
        } else {
            let preds = bb
                .preds
                .iter()
                .filter_map(|pred| {
                    self.block(pred.block)
                        .valid()
                        .then_some(pred.block.to_string())
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{block}: ({params}) <-- ({preds})")
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ssa::basic_block::BasicBlockId;
    use crate::ssa::instructions::Opcode;
    use crate::ssa::signature::{Signature, SignatureId};

    use super::Builder;
    use crate::ssa::types::Type;
    use crate::ssa::vs::Values;

    #[test]
    fn resolve_alias_follows_chain() {
        let mut b = Builder::new();
        b.init(Signature::new(SignatureId(0), vec![], vec![]));
        let v1 = b.allocate_value(Type::I32);
        let v2 = b.allocate_value(Type::I32);
        let v3 = b.allocate_value(Type::I32);
        let v4 = b.allocate_value(Type::I32);
        let v5 = b.allocate_value(Type::I32);
        b.alias(v1, v2);
        b.alias(v2, v3);
        b.alias(v3, v4);
        b.alias(v4, v5);
        assert_eq!(b.resolve_alias(v1), v5);
        assert_eq!(b.resolve_alias(v5), v5);
    }

    #[test]
    fn sealing_block_materializes_block_argument() {
        let mut b = Builder::new();
        b.init(Signature::new(SignatureId(0), vec![], vec![]));
        let v = b.declare_variable(Type::I32);
        let pred1 = b.allocate_basic_block();
        let pred2 = b.allocate_basic_block();
        let merge = b.allocate_basic_block();

        b.set_current_block(pred1);
        let one = b.insert_instruction(b.allocate_instruction().as_iconst32(1));
        let one = b.instruction(one).return_();
        b.define_variable(v, one, pred1);
        b.insert_instruction(b.allocate_instruction().as_jump(Values::new(), merge));

        b.set_current_block(pred2);
        let two = b.insert_instruction(b.allocate_instruction().as_iconst32(2));
        let two = b.instruction(two).return_();
        b.define_variable(v, two, pred2);
        b.insert_instruction(b.allocate_instruction().as_jump(Values::new(), merge));

        b.set_current_block(merge);
        let phi = b.must_find_value(v);
        assert!(phi.valid());
        assert_eq!(b.block(merge).params_len(), 0);

        b.seal(merge);

        assert_eq!(b.block(merge).params_len(), 1);
        assert_eq!(b.block(merge).param(0), phi);
        let pred_branches = b.block(merge).preds.clone();
        let args_0 = b.instruction(pred_branches[0].branch).vs.as_slice();
        let args_1 = b.instruction(pred_branches[1].branch).vs.as_slice();
        assert_eq!(args_0.len(), 1);
        assert_eq!(args_1.len(), 1);
        assert_ne!(args_0[0], args_1[0]);
    }

    #[test]
    fn linear_path_lookup_requires_sealed_single_predecessor() {
        let mut b = Builder::new();
        b.init(Signature::new(SignatureId(0), vec![], vec![]));
        let v = b.declare_variable(Type::I64);
        let entry = b.allocate_basic_block();
        let child = b.allocate_basic_block();

        b.set_current_block(entry);
        let value = b.insert_instruction(b.allocate_instruction().as_iconst64(9));
        let value = b.instruction(value).return_();
        b.define_variable(v, value, entry);
        b.insert_instruction(b.allocate_instruction().as_jump(Values::new(), child));

        b.set_current_block(child);
        assert!(!b.find_value_in_linear_path(v).valid());
        b.seal(child);
        assert_eq!(b.find_value_in_linear_path(v), value);
    }

    #[test]
    fn inserting_call_uses_signature_results() {
        let mut b = Builder::new();
        b.init(Signature::new(SignatureId(0), vec![], vec![]));
        b.declare_signature(Signature::new(
            SignatureId(3),
            vec![Type::I32],
            vec![Type::I64, Type::I32],
        ));
        let entry = b.allocate_basic_block();
        b.set_current_block(entry);
        let call = b.insert_instruction(b.allocate_instruction().as_call(
            crate::ssa::FuncRef(9),
            SignatureId(3),
            Values::new(),
        ));
        let inst = b.instruction(call);
        assert_eq!(inst.return_().ty(), Type::I64);
        assert_eq!(inst.r_values.as_slice()[0].ty(), Type::I32);
    }

    #[test]
    fn format_includes_blocks_and_instructions() {
        let mut b = Builder::new();
        b.init(Signature::new(SignatureId(0), vec![], vec![]));
        let entry = b.allocate_basic_block();
        let exit = BasicBlockId(u32::MAX);
        b.set_current_block(entry);
        let zero = b.insert_instruction(b.allocate_instruction().as_iconst32(0));
        let zero = b.instruction(zero).return_();
        b.insert_instruction(b.allocate_instruction().as_brz(zero, Values::new(), exit));
        let formatted = b.format();
        assert!(formatted.contains("blk0"));
        assert!(formatted.contains(&format!("{:?}", Opcode::Iconst)));
        assert!(formatted.contains("blk_ret"));
    }
}
