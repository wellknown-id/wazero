use crate::ssa::{BasicBlock, Instruction, Opcode, Value};

use super::compiler::Compiler;
use super::machine::Machine;
use super::CompilerContext;

fn instruction_constant(instr: &Instruction) -> bool {
    matches!(
        instr.opcode,
        Opcode::Iconst | Opcode::F32const | Opcode::F64const | Opcode::Vconst
    )
}

impl<M: Machine + 'static> Compiler<M> {
    pub(crate) fn lower_blocks(&mut self) {
        let blocks = self.reverse_post_order_blocks();
        for block in &blocks {
            self.lower_block(*block);
        }
        for pair in blocks.windows(2) {
            self.machine.link_adjacent_blocks(pair[0], pair[1]);
        }
    }

    fn lower_block(&mut self, block: BasicBlock) {
        self.machine.start_block(block);

        let mut cur = self.ssa_builder.block(block).tail_instr;
        let mut br0 = None;
        let mut br1 = None;
        if let Some(instr_id) = cur {
            let instr = self.ssa_builder.instruction(instr_id).clone();
            if instr.is_branching() {
                br0 = Some(instr.clone());
                cur = instr.prev;
                if let Some(prev_id) = cur {
                    let prev = self.ssa_builder.instruction(prev_id).clone();
                    if prev.is_branching() {
                        br1 = Some(prev.clone());
                        cur = prev.prev;
                    }
                }
            }
        }

        if let Some(branch0) = br0.as_ref() {
            self.lower_branches(branch0, br1.as_ref());
        }

        assert!(
            !(br1.is_some() && br0.is_none()),
            "conditional branch must be followed by an unconditional branch"
        );

        while let Some(instr_id) = cur {
            let instr = self.ssa_builder.instruction(instr_id).clone();
            cur = instr.prev;
            self.set_current_group_id(instr.gid);
            if instr.lowered() {
                continue;
            }
            if matches!(instr.opcode, Opcode::Return) {
                if !instr.vs.is_empty() {
                    self.machine.lower_returns(instr.vs.as_slice());
                }
                self.machine.insert_return();
            } else {
                self.machine.lower_instr(&instr);
            }
            self.machine.flush_pending_instructions();
        }

        if block.is_entry() {
            self.lower_function_arguments(block);
        }

        self.machine.end_block();
    }

    fn lower_branches(&mut self, br0: &Instruction, br1: Option<&Instruction>) {
        self.set_current_group_id(br0.gid);
        self.machine.lower_single_branch(br0);
        self.machine.flush_pending_instructions();
        if let Some(br1) = br1 {
            self.set_current_group_id(br1.gid);
            self.machine.lower_conditional_branch(br1);
            self.machine.flush_pending_instructions();
        }

        if matches!(br0.opcode, Opcode::Jump) {
            let (_, args, target) = br0.branch_data();
            let arg_exists = !args.is_empty();
            assert!(
                !(arg_exists && br1.is_some()),
                "critical edge split must be completed before lowering"
            );
            if arg_exists && target.is_return() {
                self.machine.lower_returns(args);
            } else if arg_exists {
                self.lower_block_arguments(args, target);
            }
        }
        self.machine.flush_pending_instructions();
    }

    fn lower_function_arguments(&mut self, entry: BasicBlock) {
        self.tmp_vals.clear();
        let params = self.ssa_builder.block(entry).params.as_slice().to_vec();
        for param in params {
            let value_info = self
                .ssa_values_info
                .get(param.id().0 as usize)
                .copied()
                .unwrap_or_default();
            if value_info.ref_count > 0 {
                self.tmp_vals.push(param);
            } else {
                self.tmp_vals.push(Value::INVALID);
            }
        }
        self.machine.lower_params(&self.tmp_vals);
        self.machine.flush_pending_instructions();
    }

    pub(crate) fn lower_block_arguments(&mut self, args: &[Value], succ: BasicBlock) {
        let succ_params = self.ssa_builder.block(succ).params.as_slice().to_vec();
        assert_eq!(
            args.len(),
            succ_params.len(),
            "mismatched number of block arguments"
        );

        self.var_edges.clear();
        self.var_edge_types.clear();
        self.const_edges.clear();

        for (index, src) in args.iter().copied().enumerate() {
            let dst = succ_params[index];
            let dst_reg = self.v_reg_of(dst);
            if let Some(src_instr) = self.ssa_builder.instruction_of_value(src) {
                if instruction_constant(src_instr) {
                    self.const_edges.push((src_instr.id.unwrap(), dst_reg));
                    continue;
                }
            }

            let src_reg = self.v_reg_of(src);
            self.var_edges.push([src_reg, dst_reg]);
            self.var_edge_types.push(src.ty());
        }

        self.v_reg_ids.clear();
        for [src, _] in &self.var_edges {
            let src = src.id() as usize;
            if self.v_reg_set.len() <= src {
                self.v_reg_set.resize(src + 1, false);
            }
            self.v_reg_set[src] = true;
            self.v_reg_ids.push(src as u32);
        }

        let mut separated = true;
        for [_, dst] in &self.var_edges {
            let dst = dst.id() as usize;
            if self.v_reg_set.len() <= dst {
                self.v_reg_set.resize(dst + 1, false);
            } else if self.v_reg_set[dst] {
                separated = false;
                break;
            }
        }

        for id in &self.v_reg_ids {
            self.v_reg_set[*id as usize] = false;
        }

        if separated {
            for (index, [src, dst]) in self.var_edges.iter().copied().enumerate() {
                self.machine
                    .insert_move(dst, src, self.var_edge_types[index]);
            }
        } else {
            self.temp_regs.clear();
            for index in 0..self.var_edges.len() {
                let [src, _] = self.var_edges[index];
                let ty = self.var_edge_types[index];
                let temp = self.allocate_vreg(ty);
                self.temp_regs.push(temp);
                self.machine.insert_move(temp, src, ty);
            }
            for (index, [_, dst]) in self.var_edges.iter().copied().enumerate() {
                self.machine
                    .insert_move(dst, self.temp_regs[index], self.var_edge_types[index]);
            }
        }

        for (instr_id, dst) in &self.const_edges {
            let instr = self.ssa_builder.instruction(*instr_id).clone();
            self.machine.insert_load_constant_block_arg(&instr, *dst);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ptr::NonNull;

    use super::Compiler;
    use crate::backend::{
        BackendError, CompilerContext, FunctionAbi, Machine, RelocationInfo, VReg,
    };
    use crate::ssa::{
        BasicBlock, BasicBlockId, Builder, Instruction, Signature, SignatureId, SourceOffset, Type,
        Value,
    };
    use crate::wazevoapi::ExitCode;

    #[derive(Default)]
    struct MockMachine {
        arg_result_ints: Vec<u8>,
        arg_result_floats: Vec<u8>,
        inserted_moves: Vec<(VReg, VReg)>,
        inserted_consts: Vec<(Instruction, VReg)>,
    }

    impl Machine for MockMachine {
        fn start_lowering_function(&mut self, _max_block_id: BasicBlockId) {}
        fn link_adjacent_blocks(&mut self, _prev: BasicBlock, _next: BasicBlock) {}
        fn start_block(&mut self, _block: BasicBlock) {}
        fn end_block(&mut self) {}
        fn flush_pending_instructions(&mut self) {}
        fn disable_stack_check(&mut self) {}
        fn set_current_abi(&mut self, _abi: FunctionAbi) {}
        fn set_compiler(&mut self, _compiler: NonNull<dyn CompilerContext>) {}
        fn lower_single_branch(&mut self, _branch: &Instruction) {}
        fn lower_conditional_branch(&mut self, _branch: &Instruction) {}
        fn lower_instr(&mut self, _instruction: &Instruction) {}
        fn reset(&mut self) {}
        fn insert_move(&mut self, dst: VReg, src: VReg, _ty: Type) {
            self.inserted_moves.push((src, dst));
        }
        fn insert_return(&mut self) {}
        fn insert_load_constant_block_arg(&mut self, instr: &Instruction, dst: VReg) {
            self.inserted_consts.push((instr.clone(), dst));
        }
        fn format(&self) -> String {
            String::new()
        }
        fn reg_alloc(&mut self) {}
        fn post_reg_alloc(&mut self) {}
        fn resolve_relocations(
            &mut self,
            _ref_to_binary_offset: &[i32],
            _imported_fns: usize,
            _executable: &mut [u8],
            _relocations: &[RelocationInfo],
            _call_trampoline_island_offsets: &[i32],
        ) {
        }
        fn encode(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
        fn compile_host_function_trampoline(
            &mut self,
            _exit_code: ExitCode,
            _sig: &Signature,
            _need_module_context_ptr: bool,
        ) -> Vec<u8> {
            Vec::new()
        }
        fn compile_stack_grow_call_sequence(&mut self) -> Vec<u8> {
            Vec::new()
        }
        fn compile_entry_preamble(
            &mut self,
            _signature: &Signature,
            _use_host_stack: bool,
        ) -> Vec<u8> {
            Vec::new()
        }
        fn lower_params(&mut self, _params: &[Value]) {}
        fn lower_returns(&mut self, _returns: &[Value]) {}
        fn args_results_regs(&self) -> (&[u8], &[u8]) {
            (&self.arg_result_ints, &self.arg_result_floats)
        }
        fn call_trampoline_island_info(
            &self,
            _num_functions: usize,
        ) -> Result<(usize, usize), BackendError> {
            Ok((0, 0))
        }
        fn add_source_offset_info(
            &mut self,
            _executable_offset: i64,
            _source_offset: SourceOffset,
        ) {
        }
    }

    #[test]
    fn lower_block_arguments_handles_all_constants() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let entry = builder.allocate_basic_block();
        builder.set_current_block(entry);

        let i1 = builder.insert_instruction(builder.allocate_instruction().as_iconst32(1));
        let i2 = builder.insert_instruction(builder.allocate_instruction().as_iconst64(2));
        let i3 = builder.insert_instruction(builder.allocate_instruction().as_f32const(3.0));
        let i4 = builder.insert_instruction(builder.allocate_instruction().as_f64const(4.0));

        let succ = builder.allocate_basic_block();
        for ty in [Type::I32, Type::I64, Type::F32, Type::F64] {
            let value = builder.allocate_value(ty);
            builder.block_mut(succ).add_param(value);
        }

        let mut compiler = Compiler::new(
            MockMachine {
                arg_result_ints: vec![1, 2],
                arg_result_floats: vec![11, 12, 13, 14],
                ..Default::default()
            },
            builder,
        );
        compiler.ssa_value_to_vregs = vec![
            VReg(0),
            VReg(1),
            VReg(2),
            VReg(3),
            VReg(4),
            VReg(5),
            VReg(6),
            VReg(7),
        ];

        let args = [
            compiler.ssa_builder.instruction(i1).return_(),
            compiler.ssa_builder.instruction(i2).return_(),
            compiler.ssa_builder.instruction(i3).return_(),
            compiler.ssa_builder.instruction(i4).return_(),
        ];
        compiler.lower_block_arguments(&args, succ);

        let inserted = &compiler.machine().inserted_consts;
        assert_eq!(inserted.len(), 4);
        assert_eq!(inserted[0].0.id, Some(i1));
        assert_eq!(inserted[0].1, VReg(4));
        assert_eq!(inserted[3].0.id, Some(i4));
        assert_eq!(inserted[3].1, VReg(7));
    }

    #[test]
    fn lower_block_arguments_uses_temps_when_edges_overlap() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let blk = builder.allocate_basic_block();
        let p0 = builder.allocate_value(Type::I32);
        let p1 = builder.allocate_value(Type::I32);
        let p2 = builder.allocate_value(Type::F32);
        builder.block_mut(blk).add_param(p0);
        builder.block_mut(blk).add_param(p1);
        builder.block_mut(blk).add_param(p2);

        let mut compiler = Compiler::new(MockMachine::default(), builder);
        compiler.ssa_value_to_vregs = vec![VReg(0), VReg(1), VReg(2), VReg(3)];
        compiler.next_vreg_id = 100;
        compiler.lower_block_arguments(&[p1, p0, p2], blk);

        assert_eq!(
            compiler.machine().inserted_moves,
            vec![
                (
                    VReg(1),
                    VReg(100).set_reg_type(crate::backend::RegType::Int)
                ),
                (
                    VReg(0),
                    VReg(101).set_reg_type(crate::backend::RegType::Int)
                ),
                (
                    VReg(2),
                    VReg(102).set_reg_type(crate::backend::RegType::Float)
                ),
                (
                    VReg(100).set_reg_type(crate::backend::RegType::Int),
                    VReg(0)
                ),
                (
                    VReg(101).set_reg_type(crate::backend::RegType::Int),
                    VReg(1)
                ),
                (
                    VReg(102).set_reg_type(crate::backend::RegType::Float),
                    VReg(2)
                ),
            ]
        );
    }

    #[test]
    fn lower_block_arguments_moves_directly_when_separate() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let blk = builder.allocate_basic_block();
        let param = builder.allocate_value(Type::I32);
        builder.block_mut(blk).add_param(param);
        builder.set_current_block(blk);
        let add = builder.insert_instruction(builder.allocate_instruction().as_iadd(param, param));

        let mut compiler = Compiler::new(MockMachine::default(), builder);
        compiler.ssa_value_to_vregs = vec![VReg(0), VReg(1)];
        let result = compiler.ssa_builder.instruction(add).return_();
        compiler.lower_block_arguments(&[result], blk);

        assert_eq!(compiler.machine().inserted_moves, vec![(VReg(1), VReg(0))]);
    }
}
