use crate::frontend::{function_index_to_func_ref, Compiler, ControlFrame, ControlFrameKind};
use crate::ssa::{
    BasicBlock, FloatCmpCond, IntegerCmpCond, Opcode, SourceOffset, Type, Value, Values,
};
use razero_wasm::instruction::*;
use razero_wasm::{leb128, module as wasm};

impl super::LoweringState {
    pub(super) fn reset(&mut self) {
        self.values.clear();
        self.control_frames.clear();
        self.unreachable = false;
        self.unreachable_depth = 0;
        self.pc = 0;
    }

    fn push(&mut self, value: Value) {
        self.values.push(value);
    }

    fn pop(&mut self) -> Value {
        self.values.pop().expect("value stack underflow")
    }

    fn peek(&self) -> Value {
        *self.values.last().expect("value stack underflow")
    }

    fn ctrl_push(&mut self, frame: ControlFrame) {
        self.control_frames.push(frame);
    }

    fn ctrl_pop(&mut self) -> ControlFrame {
        self.control_frames.pop().expect("control frame underflow")
    }

    fn ctrl_peek_at(&self, depth: usize) -> &ControlFrame {
        let index = self.control_frames.len() - 1 - depth;
        &self.control_frames[index]
    }

    fn br_target_arg_num_for(&self, label_index: u32) -> (BasicBlock, usize) {
        let target_frame = self.ctrl_peek_at(label_index as usize);
        if target_frame.is_loop() {
            (
                target_frame
                    .block
                    .expect("loop frame must have a header block"),
                target_frame.block_type.params.len(),
            )
        } else {
            (
                target_frame.following_block,
                target_frame.block_type.results.len(),
            )
        }
    }
}

impl<'a> Compiler<'a> {
    pub(super) fn lower_body(&mut self, entry_block: BasicBlock) {
        self.ssa_builder.seal(entry_block);
        self.lowering_state.ctrl_push(ControlFrame {
            kind: ControlFrameKind::Function,
            original_stack_len_without_param: 0,
            block: None,
            following_block: self.ssa_builder.return_block(),
            block_type: self.wasm_function_typ.clone(),
            cloned_args: Vec::new(),
        });

        while self.lowering_state.pc < self.wasm_function_body.len() {
            self.lower_current_opcode();
        }
    }

    fn lower_current_opcode(&mut self) {
        let op = self.wasm_function_body[self.lowering_state.pc];
        if self.need_source_offset_info {
            self.ssa_builder.set_current_source_offset(SourceOffset(
                self.lowering_state.pc as i64
                    + self.wasm_function_body_offset_in_code_section as i64,
            ));
        }

        match op {
            OPCODE_I32_CONST => {
                let imm = self.read_i32() as u32;
                if !self.lowering_state.unreachable {
                    let id = self.ssa_builder.insert_instruction(
                        self.ssa_builder.allocate_instruction().as_iconst32(imm),
                    );
                    self.lowering_state
                        .push(self.ssa_builder.instruction(id).return_());
                }
            }
            OPCODE_I64_CONST => {
                let imm = self.read_i64() as u64;
                if !self.lowering_state.unreachable {
                    let id = self.ssa_builder.insert_instruction(
                        self.ssa_builder.allocate_instruction().as_iconst64(imm),
                    );
                    self.lowering_state
                        .push(self.ssa_builder.instruction(id).return_());
                }
            }
            OPCODE_F32_CONST => {
                let imm = self.read_f32();
                if !self.lowering_state.unreachable {
                    let id = self.ssa_builder.insert_instruction(
                        self.ssa_builder.allocate_instruction().as_f32const(imm),
                    );
                    self.lowering_state
                        .push(self.ssa_builder.instruction(id).return_());
                }
            }
            OPCODE_F64_CONST => {
                let imm = self.read_f64();
                if !self.lowering_state.unreachable {
                    let id = self.ssa_builder.insert_instruction(
                        self.ssa_builder.allocate_instruction().as_f64const(imm),
                    );
                    self.lowering_state
                        .push(self.ssa_builder.instruction(id).return_());
                }
            }
            OPCODE_LOCAL_GET => {
                let index = self.read_u32();
                if !self.lowering_state.unreachable {
                    let variable = self.local_variable(index);
                    let value = self.ssa_builder.must_find_value(variable);
                    self.lowering_state.push(value);
                }
            }
            OPCODE_LOCAL_SET => {
                let index = self.read_u32();
                if !self.lowering_state.unreachable {
                    let variable = self.local_variable(index);
                    let new_value = self.lowering_state.pop();
                    self.ssa_builder
                        .define_variable_in_current_bb(variable, new_value);
                }
            }
            OPCODE_LOCAL_TEE => {
                let index = self.read_u32();
                if !self.lowering_state.unreachable {
                    let variable = self.local_variable(index);
                    let new_value = self.lowering_state.peek();
                    self.ssa_builder
                        .define_variable_in_current_bb(variable, new_value);
                }
            }
            OPCODE_SELECT | OPCODE_TYPED_SELECT => {
                if op == OPCODE_TYPED_SELECT {
                    self.lowering_state.pc += 2;
                }
                if !self.lowering_state.unreachable {
                    let cond = self.lowering_state.pop();
                    let v2 = self.lowering_state.pop();
                    let v1 = self.lowering_state.pop();
                    let value = self.emit_ternary(Opcode::Select, cond, v1, v2, v1.ty());
                    self.lowering_state.push(value);
                }
            }
            OPCODE_I32_ADD | OPCODE_I64_ADD => self.lower_binary_same(|this, x, y| {
                this.ssa_builder.allocate_instruction().as_iadd(x, y)
            }),
            OPCODE_I32_SUB | OPCODE_I64_SUB => self.lower_binary_same(|this, x, y| {
                this.ssa_builder.allocate_instruction().as_isub(x, y)
            }),
            OPCODE_I32_MUL | OPCODE_I64_MUL => self.lower_binary_same(|this, x, y| {
                this.ssa_builder.allocate_instruction().as_imul(x, y)
            }),
            OPCODE_F32_ADD | OPCODE_F64_ADD => self.lower_binary_generic(Opcode::Fadd),
            OPCODE_F32_SUB | OPCODE_F64_SUB => self.lower_binary_generic(Opcode::Fsub),
            OPCODE_F32_MUL | OPCODE_F64_MUL => self.lower_binary_generic(Opcode::Fmul),
            OPCODE_F32_DIV | OPCODE_F64_DIV => self.lower_binary_generic(Opcode::Fdiv),
            OPCODE_I32_AND | OPCODE_I64_AND => self.lower_binary_generic(Opcode::Band),
            OPCODE_I32_OR | OPCODE_I64_OR => self.lower_binary_generic(Opcode::Bor),
            OPCODE_I32_XOR | OPCODE_I64_XOR => self.lower_binary_generic(Opcode::Bxor),
            OPCODE_I32_SHL | OPCODE_I64_SHL => self.lower_binary_generic(Opcode::Ishl),
            OPCODE_I32_SHR_U | OPCODE_I64_SHR_U => self.lower_binary_generic(Opcode::Ushr),
            OPCODE_I32_SHR_S | OPCODE_I64_SHR_S => self.lower_binary_generic(Opcode::Sshr),
            OPCODE_I32_ROTL | OPCODE_I64_ROTL => self.lower_binary_generic(Opcode::Rotl),
            OPCODE_I32_ROTR | OPCODE_I64_ROTR => self.lower_binary_generic(Opcode::Rotr),
            OPCODE_I32_EQZ => self.lower_eqz(Type::I32),
            OPCODE_I64_EQZ => self.lower_eqz(Type::I64),
            OPCODE_I32_EQ => self.lower_icmp(IntegerCmpCond::Equal),
            OPCODE_I32_NE => self.lower_icmp(IntegerCmpCond::NotEqual),
            OPCODE_I32_LT_S => self.lower_icmp(IntegerCmpCond::SignedLessThan),
            OPCODE_I32_LT_U => self.lower_icmp(IntegerCmpCond::UnsignedLessThan),
            OPCODE_I32_GT_S => self.lower_icmp(IntegerCmpCond::SignedGreaterThan),
            OPCODE_I32_GT_U => self.lower_icmp(IntegerCmpCond::UnsignedGreaterThan),
            OPCODE_I32_LE_S => self.lower_icmp(IntegerCmpCond::SignedLessThanOrEqual),
            OPCODE_I32_LE_U => self.lower_icmp(IntegerCmpCond::UnsignedLessThanOrEqual),
            OPCODE_I32_GE_S => self.lower_icmp(IntegerCmpCond::SignedGreaterThanOrEqual),
            OPCODE_I32_GE_U => self.lower_icmp(IntegerCmpCond::UnsignedGreaterThanOrEqual),
            OPCODE_I64_EQ => self.lower_icmp(IntegerCmpCond::Equal),
            OPCODE_I64_NE => self.lower_icmp(IntegerCmpCond::NotEqual),
            OPCODE_I64_LT_S => self.lower_icmp(IntegerCmpCond::SignedLessThan),
            OPCODE_I64_LT_U => self.lower_icmp(IntegerCmpCond::UnsignedLessThan),
            OPCODE_I64_GT_S => self.lower_icmp(IntegerCmpCond::SignedGreaterThan),
            OPCODE_I64_GT_U => self.lower_icmp(IntegerCmpCond::UnsignedGreaterThan),
            OPCODE_I64_LE_S => self.lower_icmp(IntegerCmpCond::SignedLessThanOrEqual),
            OPCODE_I64_LE_U => self.lower_icmp(IntegerCmpCond::UnsignedLessThanOrEqual),
            OPCODE_I64_GE_S => self.lower_icmp(IntegerCmpCond::SignedGreaterThanOrEqual),
            OPCODE_I64_GE_U => self.lower_icmp(IntegerCmpCond::UnsignedGreaterThanOrEqual),
            OPCODE_F32_EQ | OPCODE_F64_EQ => self.lower_fcmp(FloatCmpCond::Equal),
            OPCODE_F32_NE | OPCODE_F64_NE => self.lower_fcmp(FloatCmpCond::NotEqual),
            OPCODE_F32_LT | OPCODE_F64_LT => self.lower_fcmp(FloatCmpCond::LessThan),
            OPCODE_F32_LE | OPCODE_F64_LE => self.lower_fcmp(FloatCmpCond::LessThanOrEqual),
            OPCODE_F32_GT | OPCODE_F64_GT => self.lower_fcmp(FloatCmpCond::GreaterThan),
            OPCODE_F32_GE | OPCODE_F64_GE => self.lower_fcmp(FloatCmpCond::GreaterThanOrEqual),
            OPCODE_DROP | OPCODE_NOP => {
                if op == OPCODE_DROP && !self.lowering_state.unreachable {
                    let _ = self.lowering_state.pop();
                }
            }
            OPCODE_UNREACHABLE => {
                if !self.lowering_state.unreachable {
                    let mut instr = self
                        .ssa_builder
                        .allocate_instruction()
                        .with_opcode(Opcode::ExitWithCode);
                    instr.v = self.exec_ctx_ptr_value;
                    instr.u1 = 0;
                    self.ssa_builder.insert_instruction(instr);
                    self.lowering_state.unreachable = true;
                }
            }
            OPCODE_BLOCK => {
                let block_type = self.read_block_type();
                if self.lowering_state.unreachable {
                    self.lowering_state.unreachable_depth += 1;
                } else {
                    let following_block = self.ssa_builder.allocate_basic_block();
                    self.add_block_params_from_wasm_types(&block_type.results, following_block);
                    self.lowering_state.ctrl_push(ControlFrame {
                        kind: ControlFrameKind::Block,
                        original_stack_len_without_param: self.lowering_state.values.len()
                            - block_type.params.len(),
                        block: None,
                        following_block,
                        block_type,
                        cloned_args: Vec::new(),
                    });
                }
            }
            OPCODE_LOOP => {
                let block_type = self.read_block_type();
                if self.lowering_state.unreachable {
                    self.lowering_state.unreachable_depth += 1;
                } else {
                    let loop_header = self.ssa_builder.allocate_basic_block();
                    let after_loop = self.ssa_builder.allocate_basic_block();
                    self.add_block_params_from_wasm_types(&block_type.params, loop_header);
                    self.add_block_params_from_wasm_types(&block_type.results, after_loop);
                    let original_len = self
                        .lowering_state
                        .values
                        .len()
                        .saturating_sub(block_type.params.len());
                    self.lowering_state.ctrl_push(ControlFrame {
                        kind: ControlFrameKind::Loop,
                        original_stack_len_without_param: original_len,
                        block: Some(loop_header),
                        following_block: after_loop,
                        block_type: block_type.clone(),
                        cloned_args: Vec::new(),
                    });
                    let args = self.n_peek_dup(block_type.params.len());
                    self.insert_jump_to_block(args, loop_header);
                    self.switch_to(original_len, loop_header);
                }
            }
            OPCODE_IF => {
                let block_type = self.read_block_type();
                if self.lowering_state.unreachable {
                    self.lowering_state.unreachable_depth += 1;
                } else {
                    let condition = self.lowering_state.pop();
                    let then_block = self.ssa_builder.allocate_basic_block();
                    let else_block = self.ssa_builder.allocate_basic_block();
                    let following_block = self.ssa_builder.allocate_basic_block();
                    self.add_block_params_from_wasm_types(&block_type.results, following_block);
                    let args = self.n_peek_dup(block_type.params.len()).as_slice().to_vec();
                    self.ssa_builder.insert_instruction(
                        self.ssa_builder.allocate_instruction().as_brz(
                            condition,
                            Values::new(),
                            else_block,
                        ),
                    );
                    self.ssa_builder.insert_instruction(
                        self.ssa_builder
                            .allocate_instruction()
                            .as_jump(Values::new(), then_block),
                    );
                    self.lowering_state.ctrl_push(ControlFrame {
                        kind: ControlFrameKind::IfWithoutElse,
                        original_stack_len_without_param: self.lowering_state.values.len()
                            - block_type.params.len(),
                        block: Some(else_block),
                        following_block,
                        block_type,
                        cloned_args: args,
                    });
                    self.ssa_builder.set_current_block(then_block);
                    self.ssa_builder.seal(then_block);
                    self.ssa_builder.seal(else_block);
                }
            }
            OPCODE_ELSE => {
                let current_unreachable = self.lowering_state.unreachable;
                if current_unreachable && self.lowering_state.unreachable_depth > 0 {
                    // Nested unreachable else body is dropped.
                } else {
                    let top = self.lowering_state.control_frames.len() - 1;
                    self.lowering_state.control_frames[top].kind = ControlFrameKind::IfWithElse;
                    let ctrl = self.lowering_state.control_frames[top].clone();
                    if !current_unreachable {
                        let args = self.n_peek_dup(ctrl.block_type.results.len());
                        self.insert_jump_to_block(args, ctrl.following_block);
                    } else {
                        self.lowering_state.unreachable = false;
                    }
                    self.lowering_state
                        .values
                        .truncate(ctrl.original_stack_len_without_param);
                    self.lowering_state
                        .values
                        .extend(ctrl.cloned_args.iter().copied());
                    self.ssa_builder
                        .set_current_block(ctrl.block.expect("if frame must have else block"));
                }
            }
            OPCODE_END => {
                if self.lowering_state.unreachable_depth > 0 {
                    self.lowering_state.unreachable_depth -= 1;
                } else {
                    let ctrl = self.lowering_state.ctrl_pop();
                    let following_block = ctrl.following_block;
                    if !self.lowering_state.unreachable {
                        let args = self.n_peek_dup(ctrl.block_type.results.len());
                        self.insert_jump_to_block(args, following_block);
                    } else {
                        self.lowering_state.unreachable = false;
                    }

                    match ctrl.kind {
                        ControlFrameKind::Function => {}
                        ControlFrameKind::Loop => {
                            self.ssa_builder
                                .seal(ctrl.block.expect("loop frame must have a header"));
                        }
                        ControlFrameKind::IfWithoutElse => {
                            let else_block = ctrl.block.expect("if frame must have else block");
                            self.ssa_builder.set_current_block(else_block);
                            self.insert_jump_to_block(
                                Values::from_vec(ctrl.cloned_args.clone()),
                                following_block,
                            );
                        }
                        ControlFrameKind::IfWithElse | ControlFrameKind::Block => {}
                    }

                    if !matches!(ctrl.kind, ControlFrameKind::Function) {
                        self.ssa_builder.seal(following_block);
                        self.switch_to(ctrl.original_stack_len_without_param, following_block);
                    }
                }
            }
            OPCODE_BR => {
                let label_index = self.read_u32();
                if !self.lowering_state.unreachable {
                    let (target_block, arg_num) =
                        self.lowering_state.br_target_arg_num_for(label_index);
                    let args = self.n_peek_dup(arg_num);
                    self.insert_jump_to_block(args, target_block);
                    self.lowering_state.unreachable = true;
                }
            }
            OPCODE_BR_IF => {
                let label_index = self.read_u32();
                if !self.lowering_state.unreachable {
                    let condition = self.lowering_state.pop();
                    let (target_block, arg_num) =
                        self.lowering_state.br_target_arg_num_for(label_index);
                    let args = self.n_peek_dup(arg_num);
                    self.ssa_builder.insert_instruction(
                        self.ssa_builder.allocate_instruction().as_brnz(
                            condition,
                            args,
                            target_block,
                        ),
                    );
                    let else_block = self.ssa_builder.allocate_basic_block();
                    self.insert_jump_to_block(Values::new(), else_block);
                    self.ssa_builder.seal(else_block);
                    self.ssa_builder.set_current_block(else_block);
                }
            }
            OPCODE_RETURN => {
                if !self.lowering_state.unreachable {
                    self.lower_return();
                    self.lowering_state.unreachable = true;
                }
            }
            OPCODE_CALL => {
                let function_index = self.read_u32();
                if !self.lowering_state.unreachable {
                    self.lower_call(function_index);
                }
            }
            _ => panic!(
                "unsupported wasm opcode in Rust frontend lowering: 0x{op:02x} at pc {}",
                self.lowering_state.pc
            ),
        }

        self.lowering_state.pc += 1;
    }

    fn lower_binary_same(
        &mut self,
        make: impl FnOnce(&mut Self, Value, Value) -> crate::ssa::Instruction,
    ) {
        if !self.lowering_state.unreachable {
            let y = self.lowering_state.pop();
            let x = self.lowering_state.pop();
            let instr = make(self, x, y);
            let id = self.ssa_builder.insert_instruction(instr);
            self.lowering_state
                .push(self.ssa_builder.instruction(id).return_());
        }
    }

    fn lower_binary_generic(&mut self, opcode: Opcode) {
        if !self.lowering_state.unreachable {
            let y = self.lowering_state.pop();
            let x = self.lowering_state.pop();
            let value = self.emit_binary(opcode, x, y, x.ty());
            self.lowering_state.push(value);
        }
    }

    fn lower_eqz(&mut self, ty: Type) {
        if !self.lowering_state.unreachable {
            let x = self.lowering_state.pop();
            let zero = self.ssa_builder.insert_zero_value(ty);
            let id = self.ssa_builder.insert_instruction(
                self.ssa_builder
                    .allocate_instruction()
                    .as_icmp(x, zero, IntegerCmpCond::Equal),
            );
            self.lowering_state
                .push(self.ssa_builder.instruction(id).return_());
        }
    }

    fn lower_icmp(&mut self, cond: IntegerCmpCond) {
        if !self.lowering_state.unreachable {
            let y = self.lowering_state.pop();
            let x = self.lowering_state.pop();
            let id = self
                .ssa_builder
                .insert_instruction(self.ssa_builder.allocate_instruction().as_icmp(x, y, cond));
            self.lowering_state
                .push(self.ssa_builder.instruction(id).return_());
        }
    }

    fn lower_fcmp(&mut self, cond: FloatCmpCond) {
        if !self.lowering_state.unreachable {
            let y = self.lowering_state.pop();
            let x = self.lowering_state.pop();
            let id = self
                .ssa_builder
                .insert_instruction(self.ssa_builder.allocate_instruction().as_fcmp(x, y, cond));
            self.lowering_state
                .push(self.ssa_builder.instruction(id).return_());
        }
    }

    fn lower_return(&mut self) {
        let results = self.n_peek_dup(self.wasm_function_typ.results.len());
        self.ssa_builder
            .insert_instruction(self.ssa_builder.allocate_instruction().as_return(results));
    }

    fn lower_call(&mut self, function_index: wasm::Index) {
        assert!(
            function_index >= self.module.import_function_count,
            "imported calls are not supported by the Rust frontend yet"
        );
        let defined_index = (function_index - self.module.import_function_count) as usize;
        let type_index = self.module.function_section[defined_index];
        let function_type = &self.module.type_section[type_index as usize];
        let arg_count = function_type.params.len();
        let tail = self.lowering_state.values.len() - arg_count;
        let wasm_args = self.lowering_state.values[tail..].to_vec();
        self.lowering_state.values.truncate(tail);

        let mut args = Vec::with_capacity(2 + wasm_args.len());
        args.push(self.exec_ctx_ptr_value);
        args.push(self.module_ctx_ptr_value);
        args.extend(wasm_args);

        let call_id =
            self.ssa_builder
                .insert_instruction(self.ssa_builder.allocate_instruction().as_call(
                    function_index_to_func_ref(function_index),
                    self.signatures[type_index as usize].id,
                    Values::from_vec(args),
                ));
        let instr = self.ssa_builder.instruction(call_id);
        if instr.return_().valid() {
            self.lowering_state.push(instr.return_());
        }
        self.lowering_state
            .values
            .extend(instr.r_values.as_slice().iter().copied());
    }

    fn insert_jump_to_block(&mut self, args: Values, target_block: BasicBlock) {
        self.ssa_builder.insert_instruction(
            self.ssa_builder
                .allocate_instruction()
                .as_jump(args, target_block),
        );
    }

    fn switch_to(&mut self, original_stack_len: usize, target_block: BasicBlock) {
        if self.ssa_builder.block(target_block).preds_len() == 0 {
            self.lowering_state.unreachable = true;
        }
        self.lowering_state.values.truncate(original_stack_len);
        self.ssa_builder.set_current_block(target_block);
        let params = self
            .ssa_builder
            .block(target_block)
            .params
            .as_slice()
            .to_vec();
        self.lowering_state.values.extend(params);
    }

    fn n_peek_dup(&self, n: usize) -> Values {
        if n == 0 {
            return Values::new();
        }
        let tail = self.lowering_state.values.len();
        Values::from_vec(self.lowering_state.values[tail - n..tail].to_vec())
    }

    fn emit_binary(&mut self, opcode: Opcode, x: Value, y: Value, result_ty: Type) -> Value {
        let mut instr = self.ssa_builder.allocate_instruction().with_opcode(opcode);
        instr.v = x;
        instr.v2 = y;
        instr.typ = result_ty;
        let id = self.ssa_builder.insert_instruction(instr);
        self.ssa_builder.instruction(id).return_()
    }

    fn emit_ternary(
        &mut self,
        opcode: Opcode,
        v1: Value,
        v2: Value,
        v3: Value,
        result_ty: Type,
    ) -> Value {
        let mut instr = self.ssa_builder.allocate_instruction().with_opcode(opcode);
        instr.v = v1;
        instr.v2 = v2;
        instr.v3 = v3;
        instr.typ = result_ty;
        let id = self.ssa_builder.insert_instruction(instr);
        self.ssa_builder.instruction(id).return_()
    }

    fn read_u32(&mut self) -> u32 {
        let (value, read) =
            leb128::load_u32(&self.wasm_function_body[self.lowering_state.pc + 1..])
                .expect("invalid u32 LEB128 immediate");
        self.lowering_state.pc += read;
        value
    }

    fn read_i32(&mut self) -> i32 {
        let (value, read) =
            leb128::load_i32(&self.wasm_function_body[self.lowering_state.pc + 1..])
                .expect("invalid i32 LEB128 immediate");
        self.lowering_state.pc += read;
        value
    }

    fn read_i64(&mut self) -> i64 {
        let (value, read) =
            leb128::load_i64(&self.wasm_function_body[self.lowering_state.pc + 1..])
                .expect("invalid i64 LEB128 immediate");
        self.lowering_state.pc += read;
        value
    }

    fn read_f32(&mut self) -> f32 {
        let start = self.lowering_state.pc + 1;
        let end = start + 4;
        self.lowering_state.pc += 4;
        f32::from_le_bytes(
            self.wasm_function_body[start..end]
                .try_into()
                .expect("f32 immediate requires 4 bytes"),
        )
    }

    fn read_f64(&mut self) -> f64 {
        let start = self.lowering_state.pc + 1;
        let end = start + 8;
        self.lowering_state.pc += 8;
        f64::from_le_bytes(
            self.wasm_function_body[start..end]
                .try_into()
                .expect("f64 immediate requires 8 bytes"),
        )
    }

    fn read_block_type(&mut self) -> wasm::FunctionType {
        let bytes = &self.wasm_function_body[self.lowering_state.pc + 1..];
        match bytes.first().copied().expect("missing block type") {
            0x40 => {
                self.lowering_state.pc += 1;
                wasm::FunctionType::default()
            }
            0x7f | 0x7e | 0x7d | 0x7c | 0x7b | 0x70 | 0x6f => {
                self.lowering_state.pc += 1;
                let mut ty = wasm::FunctionType::default();
                ty.results.push(wasm::ValueType(bytes[0]));
                ty
            }
            _ => {
                let (type_index, read) =
                    leb128::decode_i33_as_i64(bytes).expect("invalid block type immediate");
                self.lowering_state.pc += read;
                assert!(type_index >= 0, "negative type index in block type");
                self.module.type_section[type_index as usize].clone()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{signature_for_wasm_function_type, Compiler};
    use crate::ssa::{Builder, Signature, SignatureId, Type};
    use razero_wasm::module::{Code, FunctionType, Module, ValueType};

    fn function_type(params: &[ValueType], results: &[ValueType]) -> FunctionType {
        let mut ty = FunctionType::default();
        ty.params.extend_from_slice(params);
        ty.results.extend_from_slice(results);
        ty
    }

    fn compiler_for(module: &Module) -> Compiler<'_> {
        Compiler::new(
            module,
            Builder::new(),
            None,
            false,
            false,
            false,
            false,
            false,
        )
    }

    #[test]
    fn lowers_add_sub_function_to_ssa() {
        let module = Module {
            type_section: vec![function_type(
                &[ValueType::I32, ValueType::I32],
                &[ValueType::I32],
            )],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I32_ADD,
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_I32_SUB,
                    OPCODE_END,
                ],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i32)\n\tv4:i32 = Iadd v2, v3\n\tv5:i32 = Isub v4, v2\n\tJump blk_ret, v5\n"
        );
    }

    #[test]
    fn lowers_if_else_with_result_phi() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_IF,
                    0x7f,
                    OPCODE_I32_CONST,
                    1,
                    OPCODE_ELSE,
                    OPCODE_I32_CONST,
                    2,
                    OPCODE_END,
                    OPCODE_END,
                ],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv2:invalid = Brz v2, blk2\n\tmodule_ctx:invalid = Jump blk1\n\nblk1: () <-- (blk0)\n\tv4:i32 = Iconst 1\n\tv3:invalid = Jump blk3, v4\n\nblk2: () <-- (blk0)\n\tv5:i32 = Iconst 2\n\tv3:invalid = Jump blk3, v5\n\nblk3: (v3:i32) <-- (blk1, blk2)\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_direct_call_with_abi_context_params() {
        let ty = function_type(&[ValueType::I32], &[ValueType::I32]);
        let module = Module {
            type_section: vec![ty.clone()],
            function_section: vec![0, 0],
            code_section: vec![
                Code {
                    body: vec![OPCODE_LOCAL_GET, 0, OPCODE_END],
                    ..Code::default()
                },
                Code {
                    body: vec![OPCODE_I32_CONST, 7, OPCODE_CALL, 0, OPCODE_END],
                    ..Code::default()
                },
            ],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(1, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i32 = Iconst 7\n\tv4:i32 = Call f0, sig0, exec_ctx, module_ctx, v3\n\tJump blk_ret, v4\n"
        );
    }

    #[test]
    fn signature_includes_hidden_context_params() {
        let sig = signature_for_wasm_function_type(&function_type(
            &[ValueType::I64, ValueType::F32],
            &[ValueType::F64],
        ));
        assert_eq!(
            sig,
            Signature::new(
                SignatureId(0),
                vec![Type::I64, Type::I64, Type::I64, Type::F32],
                vec![Type::F64]
            )
        );
    }
}
