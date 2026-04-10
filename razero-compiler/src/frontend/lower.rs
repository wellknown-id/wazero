use crate::frontend::{function_index_to_func_ref, Compiler, ControlFrame, ControlFrameKind};
use crate::ssa::{
    BasicBlock, FloatCmpCond, IntegerCmpCond, Opcode, SourceOffset, Type, Value, Values,
};
use crate::wazevoapi::offsetdata::{
    EXECUTION_CONTEXT_OFFSET_CALLER_MODULE_CONTEXT_PTR, EXECUTION_CONTEXT_OFFSET_FUEL,
};
use crate::wazevoapi::{
    ExitCode, FUNCTION_INSTANCE_EXECUTABLE_OFFSET,
    FUNCTION_INSTANCE_MODULE_CONTEXT_OPAQUE_PTR_OFFSET, FUNCTION_INSTANCE_TYPE_ID_OFFSET,
};
use razero_wasm::instruction::*;
use razero_wasm::{leb128, module as wasm};

const TABLE_INSTANCE_BASE_ADDRESS_OFFSET: u32 = 0;
const TABLE_INSTANCE_LEN_OFFSET: u32 = TABLE_INSTANCE_BASE_ADDRESS_OFFSET + 8;

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
        self.insert_function_entry_checks();
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

    fn insert_function_entry_checks(&mut self) {
        self.insert_fuel_check();
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
            OPCODE_I32_DIV_S | OPCODE_I64_DIV_S => self.lower_trapping_binary_generic(Opcode::Sdiv),
            OPCODE_I32_DIV_U | OPCODE_I64_DIV_U => self.lower_trapping_binary_generic(Opcode::Udiv),
            OPCODE_I32_REM_S | OPCODE_I64_REM_S => self.lower_trapping_binary_generic(Opcode::Srem),
            OPCODE_I32_REM_U | OPCODE_I64_REM_U => self.lower_trapping_binary_generic(Opcode::Urem),
            OPCODE_F32_ADD | OPCODE_F64_ADD => self.lower_binary_generic(Opcode::Fadd),
            OPCODE_F32_SUB | OPCODE_F64_SUB => self.lower_binary_generic(Opcode::Fsub),
            OPCODE_F32_MUL | OPCODE_F64_MUL => self.lower_binary_generic(Opcode::Fmul),
            OPCODE_F32_DIV | OPCODE_F64_DIV => self.lower_binary_generic(Opcode::Fdiv),
            OPCODE_F32_ABS | OPCODE_F64_ABS => self.lower_unary_generic(Opcode::Fabs),
            OPCODE_F32_NEG | OPCODE_F64_NEG => self.lower_unary_generic(Opcode::Fneg),
            OPCODE_F32_COPYSIGN | OPCODE_F64_COPYSIGN => {
                self.lower_binary_generic(Opcode::Fcopysign)
            }
            OPCODE_F32_MIN | OPCODE_F64_MIN => self.lower_binary_generic(Opcode::Fmin),
            OPCODE_F32_MAX | OPCODE_F64_MAX => self.lower_binary_generic(Opcode::Fmax),
            OPCODE_F32_SQRT | OPCODE_F64_SQRT => self.lower_unary_generic(Opcode::Sqrt),
            OPCODE_I32_WRAP_I64 => self.lower_typed_unary(Opcode::Ireduce, Type::I32),
            OPCODE_I32_TRUNC_F32_S => self.lower_typed_unary(Opcode::FcvtToSint, Type::I32),
            OPCODE_I32_TRUNC_F32_U => self.lower_typed_unary(Opcode::FcvtToUint, Type::I32),
            OPCODE_I32_TRUNC_F64_S => self.lower_typed_unary(Opcode::FcvtToSint, Type::I32),
            OPCODE_I32_TRUNC_F64_U => self.lower_typed_unary(Opcode::FcvtToUint, Type::I32),
            OPCODE_I64_TRUNC_F32_S => self.lower_typed_unary(Opcode::FcvtToSint, Type::I64),
            OPCODE_I64_TRUNC_F32_U => self.lower_typed_unary(Opcode::FcvtToUint, Type::I64),
            OPCODE_I64_TRUNC_F64_S => self.lower_typed_unary(Opcode::FcvtToSint, Type::I64),
            OPCODE_I64_TRUNC_F64_U => self.lower_typed_unary(Opcode::FcvtToUint, Type::I64),
            OPCODE_I32_EXTEND8_S => self.insert_integer_extend(true, 8, 32),
            OPCODE_I32_EXTEND16_S => self.insert_integer_extend(true, 16, 32),
            OPCODE_I64_EXTEND8_S => self.insert_integer_extend(true, 8, 64),
            OPCODE_I64_EXTEND16_S => self.insert_integer_extend(true, 16, 64),
            OPCODE_I64_EXTEND32_S | OPCODE_I64_EXTEND_I32_S => {
                self.insert_integer_extend(true, 32, 64)
            }
            OPCODE_I64_EXTEND_I32_U => self.insert_integer_extend(false, 32, 64),
            OPCODE_I32_REINTERPRET_F32 => self.lower_typed_unary(Opcode::Bitcast, Type::I32),
            OPCODE_I64_REINTERPRET_F64 => self.lower_typed_unary(Opcode::Bitcast, Type::I64),
            OPCODE_F32_REINTERPRET_I32 => self.lower_typed_unary(Opcode::Bitcast, Type::F32),
            OPCODE_F64_REINTERPRET_I64 => self.lower_typed_unary(Opcode::Bitcast, Type::F64),
            OPCODE_F32_CONVERT_I32_S => self.lower_typed_unary(Opcode::FcvtFromSint, Type::F32),
            OPCODE_F64_CONVERT_I32_S => self.lower_typed_unary(Opcode::FcvtFromSint, Type::F64),
            OPCODE_F32_CONVERT_I64_S => self.lower_typed_unary(Opcode::FcvtFromSint, Type::F32),
            OPCODE_F64_CONVERT_I64_S => self.lower_typed_unary(Opcode::FcvtFromSint, Type::F64),
            OPCODE_F32_CONVERT_I32_U => self.lower_typed_unary(Opcode::FcvtFromUint, Type::F32),
            OPCODE_F64_CONVERT_I32_U => self.lower_typed_unary(Opcode::FcvtFromUint, Type::F64),
            OPCODE_F32_CONVERT_I64_U => self.lower_typed_unary(Opcode::FcvtFromUint, Type::F32),
            OPCODE_F64_CONVERT_I64_U => self.lower_typed_unary(Opcode::FcvtFromUint, Type::F64),
            OPCODE_F64_PROMOTE_F32 => self.lower_typed_unary(Opcode::Fpromote, Type::F64),
            OPCODE_F32_DEMOTE_F64 => self.lower_typed_unary(Opcode::Fdemote, Type::F32),
            OPCODE_F32_CEIL | OPCODE_F64_CEIL => self.lower_unary_generic(Opcode::Ceil),
            OPCODE_F32_FLOOR | OPCODE_F64_FLOOR => self.lower_unary_generic(Opcode::Floor),
            OPCODE_F32_TRUNC | OPCODE_F64_TRUNC => self.lower_unary_generic(Opcode::Trunc),
            OPCODE_F32_NEAREST | OPCODE_F64_NEAREST => self.lower_unary_generic(Opcode::Nearest),
            OPCODE_I32_AND | OPCODE_I64_AND => self.lower_binary_generic(Opcode::Band),
            OPCODE_I32_OR | OPCODE_I64_OR => self.lower_binary_generic(Opcode::Bor),
            OPCODE_I32_XOR | OPCODE_I64_XOR => self.lower_binary_generic(Opcode::Bxor),
            OPCODE_I32_SHL | OPCODE_I64_SHL => self.lower_binary_generic(Opcode::Ishl),
            OPCODE_I32_SHR_U | OPCODE_I64_SHR_U => self.lower_binary_generic(Opcode::Ushr),
            OPCODE_I32_SHR_S | OPCODE_I64_SHR_S => self.lower_binary_generic(Opcode::Sshr),
            OPCODE_I32_ROTL | OPCODE_I64_ROTL => self.lower_binary_generic(Opcode::Rotl),
            OPCODE_I32_ROTR | OPCODE_I64_ROTR => self.lower_binary_generic(Opcode::Rotr),
            OPCODE_I32_CLZ | OPCODE_I64_CLZ => self.lower_unary_generic(Opcode::Clz),
            OPCODE_I32_CTZ | OPCODE_I64_CTZ => self.lower_unary_generic(Opcode::Ctz),
            OPCODE_I32_POPCNT | OPCODE_I64_POPCNT => self.lower_unary_generic(Opcode::Popcnt),
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
                    self.insert_loop_header_checks();
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
            OPCODE_BR_TABLE => {
                let label_count = self.read_u32() as usize;
                let mut labels = Vec::with_capacity(label_count + 1);
                for _ in 0..label_count {
                    labels.push(self.read_u32());
                }
                labels.push(self.read_u32());
                if !self.lowering_state.unreachable {
                    let index = self.lowering_state.pop();
                    if label_count == 0 {
                        let (target_block, arg_num) =
                            self.lowering_state.br_target_arg_num_for(labels[0]);
                        let args = self.n_peek_dup(arg_num);
                        self.insert_jump_to_block(args, target_block);
                    } else {
                        self.lower_br_table(&labels, index);
                    }
                    self.lowering_state.unreachable = true;
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
            OPCODE_CALL_INDIRECT => {
                let type_index = self.read_u32();
                let table_index = self.read_u32();
                if !self.lowering_state.unreachable {
                    self.lower_call_indirect(type_index, table_index);
                }
            }
            OPCODE_I32_LOAD => {
                let _align = self.read_u32();
                let offset = self.read_u32();
                if !self.lowering_state.unreachable {
                    let base_addr = self.lowering_state.pop();
                    let addr = self.lower_local_memory_address(base_addr, offset as u64, 4);
                    let value = self.emit_load(addr, offset, Type::I32);
                    self.lowering_state.push(value);
                }
            }
            OPCODE_I64_LOAD => {
                let _align = self.read_u32();
                let offset = self.read_u32();
                if !self.lowering_state.unreachable {
                    let base_addr = self.lowering_state.pop();
                    let addr = self.lower_local_memory_address(base_addr, offset as u64, 8);
                    let value = self.emit_load(addr, offset, Type::I64);
                    self.lowering_state.push(value);
                }
            }
            OPCODE_F32_LOAD => {
                let _align = self.read_u32();
                let offset = self.read_u32();
                if !self.lowering_state.unreachable {
                    let base_addr = self.lowering_state.pop();
                    let addr = self.lower_local_memory_address(base_addr, offset as u64, 4);
                    let value = self.emit_load(addr, offset, Type::F32);
                    self.lowering_state.push(value);
                }
            }
            OPCODE_F64_LOAD => {
                let _align = self.read_u32();
                let offset = self.read_u32();
                if !self.lowering_state.unreachable {
                    let base_addr = self.lowering_state.pop();
                    let addr = self.lower_local_memory_address(base_addr, offset as u64, 8);
                    let value = self.emit_load(addr, offset, Type::F64);
                    self.lowering_state.push(value);
                }
            }
            OPCODE_I32_STORE => {
                let _align = self.read_u32();
                let offset = self.read_u32();
                if !self.lowering_state.unreachable {
                    let value = self.lowering_state.pop();
                    let base_addr = self.lowering_state.pop();
                    let addr = self.lower_local_memory_address(base_addr, offset as u64, 4);
                    self.emit_store(value, addr, offset);
                }
            }
            OPCODE_I64_STORE => {
                let _align = self.read_u32();
                let offset = self.read_u32();
                if !self.lowering_state.unreachable {
                    let value = self.lowering_state.pop();
                    let base_addr = self.lowering_state.pop();
                    let addr = self.lower_local_memory_address(base_addr, offset as u64, 8);
                    self.emit_store(value, addr, offset);
                }
            }
            OPCODE_F32_STORE => {
                let _align = self.read_u32();
                let offset = self.read_u32();
                if !self.lowering_state.unreachable {
                    let value = self.lowering_state.pop();
                    let base_addr = self.lowering_state.pop();
                    let addr = self.lower_local_memory_address(base_addr, offset as u64, 4);
                    self.emit_store(value, addr, offset);
                }
            }
            OPCODE_F64_STORE => {
                let _align = self.read_u32();
                let offset = self.read_u32();
                if !self.lowering_state.unreachable {
                    let value = self.lowering_state.pop();
                    let base_addr = self.lowering_state.pop();
                    let addr = self.lower_local_memory_address(base_addr, offset as u64, 8);
                    self.emit_store(value, addr, offset);
                }
            }
            OPCODE_I32_STORE8 => self.lower_ext_store(Opcode::Istore8, 1),
            OPCODE_I32_STORE16 => self.lower_ext_store(Opcode::Istore16, 2),
            OPCODE_I64_STORE8 => self.lower_ext_store(Opcode::Istore8, 1),
            OPCODE_I64_STORE16 => self.lower_ext_store(Opcode::Istore16, 2),
            OPCODE_I64_STORE32 => self.lower_ext_store(Opcode::Istore32, 4),
            OPCODE_I32_LOAD8_S => self.lower_ext_load(Opcode::Sload8, 1, Type::I32),
            OPCODE_I32_LOAD8_U => self.lower_ext_load(Opcode::Uload8, 1, Type::I32),
            OPCODE_I32_LOAD16_S => self.lower_ext_load(Opcode::Sload16, 2, Type::I32),
            OPCODE_I32_LOAD16_U => self.lower_ext_load(Opcode::Uload16, 2, Type::I32),
            OPCODE_I64_LOAD8_S => self.lower_ext_load(Opcode::Sload8, 1, Type::I64),
            OPCODE_I64_LOAD8_U => self.lower_ext_load(Opcode::Uload8, 1, Type::I64),
            OPCODE_I64_LOAD16_S => self.lower_ext_load(Opcode::Sload16, 2, Type::I64),
            OPCODE_I64_LOAD16_U => self.lower_ext_load(Opcode::Uload16, 2, Type::I64),
            OPCODE_I64_LOAD32_S => self.lower_ext_load(Opcode::Sload32, 4, Type::I64),
            OPCODE_I64_LOAD32_U => self.lower_ext_load(Opcode::Uload32, 4, Type::I64),
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

    fn lower_trapping_binary_generic(&mut self, opcode: Opcode) {
        if !self.lowering_state.unreachable {
            let y = self.lowering_state.pop();
            let x = self.lowering_state.pop();
            let value = self.emit_trapping_binary(opcode, x, y, x.ty());
            self.lowering_state.push(value);
        }
    }

    fn lower_ext_load(&mut self, opcode: Opcode, size: u64, ty: Type) {
        let _align = self.read_u32();
        let offset = self.read_u32();
        if !self.lowering_state.unreachable {
            let base_addr = self.lowering_state.pop();
            let addr = self.lower_local_memory_address(base_addr, offset as u64, size);
            let value = self.emit_ext_load(opcode, addr, offset, ty);
            self.lowering_state.push(value);
        }
    }

    fn lower_ext_store(&mut self, opcode: Opcode, size: u64) {
        let _align = self.read_u32();
        let offset = self.read_u32();
        if !self.lowering_state.unreachable {
            let value = self.lowering_state.pop();
            let base_addr = self.lowering_state.pop();
            let addr = self.lower_local_memory_address(base_addr, offset as u64, size);
            self.emit_store_with_opcode(opcode, value, addr, offset);
        }
    }

    fn lower_unary_generic(&mut self, opcode: Opcode) {
        if !self.lowering_state.unreachable {
            let x = self.lowering_state.pop();
            let value = self.emit_unary(opcode, x, x.ty());
            self.lowering_state.push(value);
        }
    }

    fn lower_typed_unary(&mut self, opcode: Opcode, result_ty: Type) {
        if !self.lowering_state.unreachable {
            let x = self.lowering_state.pop();
            let value = self.emit_unary(opcode, x, result_ty);
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

    fn function_type_index(&self, function_index: wasm::Index) -> wasm::Index {
        if function_index < self.module.import_function_count {
            self.module
                .import_section
                .iter()
                .filter_map(|import| match import.desc {
                    wasm::ImportDesc::Func(type_index) => Some(type_index),
                    _ => None,
                })
                .nth(function_index as usize)
                .unwrap_or_else(|| panic!("missing import type for function {function_index}"))
        } else {
            self.module.function_section
                [(function_index - self.module.import_function_count) as usize]
        }
    }

    fn lower_call(&mut self, function_index: wasm::Index) {
        let type_index = self.function_type_index(function_index);
        let function_type = &self.module.type_section[type_index as usize];
        let arg_count = function_type.params.len();
        let tail = self.lowering_state.values.len() - arg_count;
        let wasm_args = self.lowering_state.values[tail..].to_vec();
        self.lowering_state.values.truncate(tail);

        let mut args = Vec::with_capacity(2 + wasm_args.len());
        args.push(self.exec_ctx_ptr_value);
        let call_id = if function_index < self.module.import_function_count {
            let (func_ptr_offset, module_ctx_ptr_offset, _) = self
                .offset
                .as_ref()
                .expect("module context offsets are required for imported calls")
                .imported_function_offset(function_index);
            self.store_caller_module_context();
            let func_ptr =
                self.emit_load(self.module_ctx_ptr_value, func_ptr_offset.u32(), Type::I64);
            let callee_module_ctx = self.emit_load(
                self.module_ctx_ptr_value,
                module_ctx_ptr_offset.u32(),
                Type::I64,
            );
            args.push(callee_module_ctx);
            args.extend(wasm_args);
            self.ssa_builder.insert_instruction(
                self.ssa_builder.allocate_instruction().as_call_indirect(
                    func_ptr,
                    self.signatures[type_index as usize].id,
                    Values::from_vec(args),
                ),
            )
        } else {
            args.push(self.module_ctx_ptr_value);
            args.extend(wasm_args);
            self.ssa_builder
                .insert_instruction(self.ssa_builder.allocate_instruction().as_call(
                    function_index_to_func_ref(function_index),
                    self.signatures[type_index as usize].id,
                    Values::from_vec(args),
                ))
        };
        let instr = self.ssa_builder.instruction(call_id);
        if instr.return_().valid() {
            self.lowering_state.push(instr.return_());
        }
        self.lowering_state
            .values
            .extend(instr.r_values.as_slice().iter().copied());
    }

    fn lower_call_indirect(&mut self, type_index: wasm::Index, table_index: wasm::Index) {
        let arg_count = self.module.type_section[type_index as usize].params.len();
        let element_offset = self.lowering_state.pop();
        let function_instance_ptr_address =
            self.lower_access_table_with_bounds_check(table_index, element_offset);
        let function_instance_ptr = self.emit_load(function_instance_ptr_address, 0, Type::I64);

        let zero = self.emit_iconst64(0);
        let is_null = self.emit_icmp(function_instance_ptr, zero, IntegerCmpCond::Equal);
        self.emit_exit_if_true_with_code(
            self.exec_ctx_ptr_value,
            is_null,
            ExitCode::INDIRECT_CALL_NULL_POINTER,
        );

        let actual_type_id = self.emit_load(
            function_instance_ptr,
            FUNCTION_INSTANCE_TYPE_ID_OFFSET.u32(),
            Type::I32,
        );
        let type_ids_offset = self
            .offset
            .as_ref()
            .expect("module context offsets are required for call_indirect")
            .type_ids_1st_element
            .u32();
        let type_ids_begin = self.emit_load(self.module_ctx_ptr_value, type_ids_offset, Type::I64);
        let expected_type_id = self.emit_load(type_ids_begin, type_index * 4, Type::I32);
        let mismatched = self.emit_icmp(actual_type_id, expected_type_id, IntegerCmpCond::NotEqual);
        self.emit_exit_if_true_with_code(
            self.exec_ctx_ptr_value,
            mismatched,
            ExitCode::INDIRECT_CALL_TYPE_MISMATCH,
        );

        let executable_ptr = self.emit_load(
            function_instance_ptr,
            FUNCTION_INSTANCE_EXECUTABLE_OFFSET.u32(),
            Type::I64,
        );
        let callee_module_ctx = self.emit_load(
            function_instance_ptr,
            FUNCTION_INSTANCE_MODULE_CONTEXT_OPAQUE_PTR_OFFSET.u32(),
            Type::I64,
        );

        let tail = self.lowering_state.values.len() - arg_count;
        let wasm_args = self.lowering_state.values[tail..].to_vec();
        self.lowering_state.values.truncate(tail);

        self.store_caller_module_context();

        let mut args = Vec::with_capacity(2 + wasm_args.len());
        args.push(self.exec_ctx_ptr_value);
        args.push(callee_module_ctx);
        args.extend(wasm_args);
        let call_id = self.ssa_builder.insert_instruction(
            self.ssa_builder.allocate_instruction().as_call_indirect(
                executable_ptr,
                self.signatures[type_index as usize].id,
                Values::from_vec(args),
            ),
        );
        let instr = self.ssa_builder.instruction(call_id);
        if instr.return_().valid() {
            self.lowering_state.push(instr.return_());
        }
        self.lowering_state
            .values
            .extend(instr.r_values.as_slice().iter().copied());
    }

    fn lower_access_table_with_bounds_check(
        &mut self,
        table_index: wasm::Index,
        element_offset_in_table: Value,
    ) -> Value {
        let table_offset = self
            .offset
            .as_ref()
            .expect("module context offsets are required for table access")
            .table_offset(table_index as usize)
            .u32();
        let table_instance_ptr = self.emit_load(self.module_ctx_ptr_value, table_offset, Type::I64);
        let table_len = self.emit_load(table_instance_ptr, TABLE_INSTANCE_LEN_OFFSET, Type::I32);
        let out_of_bounds = self.emit_icmp(
            element_offset_in_table,
            table_len,
            IntegerCmpCond::UnsignedGreaterThanOrEqual,
        );
        self.emit_exit_if_true_with_code(
            self.exec_ctx_ptr_value,
            out_of_bounds,
            ExitCode::TABLE_OUT_OF_BOUNDS,
        );
        let table_base = self.emit_load(
            table_instance_ptr,
            TABLE_INSTANCE_BASE_ADDRESS_OFFSET,
            Type::I64,
        );
        let shift = self.emit_iconst64(3);
        let scaled = self.emit_binary(
            Opcode::Ishl,
            element_offset_in_table,
            shift,
            element_offset_in_table.ty(),
        );
        self.emit_binary(Opcode::Iadd, table_base, scaled, table_base.ty())
    }

    fn lower_local_memory_address(
        &mut self,
        base_addr: Value,
        const_offset: u64,
        operation_size_in_bytes: u64,
    ) -> Value {
        let offsets = self
            .offset
            .as_ref()
            .expect("module context offsets are required for memory access");
        let mem_len_offset = offsets.local_memory_len().u32();
        let mem_base_offset = offsets.local_memory_base().u32();
        let ext_base_addr = self.emit_uextend(base_addr, Type::I64);
        if self.memory_isolation_enabled {
            let mem_base = self.emit_load(self.module_ctx_ptr_value, mem_base_offset, Type::I64);
            return self.emit_binary(Opcode::Iadd, mem_base, ext_base_addr, Type::I64);
        }
        let ceil = self.emit_iconst64(const_offset + operation_size_in_bytes);
        let base_addr_plus_ceil = self.emit_binary(Opcode::Iadd, ext_base_addr, ceil, Type::I64);
        let mem_len = self.emit_ext_load(
            Opcode::Uload32,
            self.module_ctx_ptr_value,
            mem_len_offset,
            Type::I64,
        );
        let out_of_bounds = self.emit_icmp(
            mem_len,
            base_addr_plus_ceil,
            IntegerCmpCond::UnsignedLessThan,
        );
        self.emit_exit_if_true_with_code(
            self.exec_ctx_ptr_value,
            out_of_bounds,
            ExitCode::MEMORY_OUT_OF_BOUNDS,
        );
        let mem_base = self.emit_load(self.module_ctx_ptr_value, mem_base_offset, Type::I64);
        self.emit_binary(Opcode::Iadd, mem_base, ext_base_addr, Type::I64)
    }

    fn lower_br_table(&mut self, labels: &[u32], index: Value) {
        let (_, arg_num) = self.lowering_state.br_target_arg_num_for(labels[0]);
        let current_block = self
            .ssa_builder
            .current_block()
            .expect("br_table requires a current block");
        let mut targets = Vec::with_capacity(labels.len());

        for &label in labels {
            let args = self.n_peek_dup(arg_num);
            let (target_block, _) = self.lowering_state.br_target_arg_num_for(label);
            let trampoline = self.ssa_builder.allocate_basic_block();
            self.ssa_builder.set_current_block(trampoline);
            self.insert_jump_to_block(args, target_block);
            targets.push(Value(trampoline.0 as u64));
        }

        self.ssa_builder.set_current_block(current_block);
        self.ssa_builder.insert_instruction(
            self.ssa_builder
                .allocate_instruction()
                .as_br_table(index, Values::from_vec(targets.clone())),
        );
        for target in targets {
            self.ssa_builder
                .seal(crate::ssa::BasicBlockId(target.0 as u32));
        }
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

    fn emit_trapping_binary(
        &mut self,
        opcode: Opcode,
        x: Value,
        y: Value,
        result_ty: Type,
    ) -> Value {
        let mut instr = self.ssa_builder.allocate_instruction().with_opcode(opcode);
        instr.v = x;
        instr.v2 = y;
        instr.v3 = self.exec_ctx_ptr_value;
        instr.typ = result_ty;
        let id = self.ssa_builder.insert_instruction(instr);
        self.ssa_builder.instruction(id).return_()
    }

    fn emit_unary(&mut self, opcode: Opcode, x: Value, result_ty: Type) -> Value {
        let mut instr = self.ssa_builder.allocate_instruction().with_opcode(opcode);
        instr.v = x;
        if matches!(
            opcode,
            Opcode::FcvtToSint | Opcode::FcvtToUint | Opcode::FcvtToSintSat | Opcode::FcvtToUintSat
        ) {
            instr.v3 = self.exec_ctx_ptr_value;
        }
        instr.typ = result_ty;
        let id = self.ssa_builder.insert_instruction(instr);
        self.ssa_builder.instruction(id).return_()
    }

    fn emit_load(&mut self, ptr: Value, offset: u32, ty: Type) -> Value {
        let id = self.ssa_builder.insert_instruction(
            self.ssa_builder
                .allocate_instruction()
                .as_load(ptr, offset, ty),
        );
        self.ssa_builder.instruction(id).return_()
    }

    fn insert_loop_header_checks(&mut self) {
        self.insert_fuel_check();
        self.insert_termination_check();
    }

    fn insert_fuel_check(&mut self) {
        if !self.fuel_enabled {
            return;
        }

        let remaining = self.emit_load(
            self.exec_ctx_ptr_value,
            EXECUTION_CONTEXT_OFFSET_FUEL.u32(),
            Type::I64,
        );
        let decrement = self.emit_iconst64(1);
        let decremented = self.emit_binary(Opcode::Isub, remaining, decrement, Type::I64);
        self.emit_store(
            decremented,
            self.exec_ctx_ptr_value,
            EXECUTION_CONTEXT_OFFSET_FUEL.u32(),
        );
        let zero = self.emit_iconst64(0);
        let exhausted = self.emit_icmp(decremented, zero, IntegerCmpCond::SignedLessThan);
        self.emit_exit_if_true_with_code(
            self.exec_ctx_ptr_value,
            exhausted,
            ExitCode::FUEL_EXHAUSTED,
        );
    }

    fn insert_termination_check(&mut self) {
        if !self.ensure_termination {
            return;
        }

        let ptr = self.emit_load(
            self.exec_ctx_ptr_value,
            crate::wazevoapi::offsetdata::EXECUTION_CONTEXT_OFFSET_CHECK_MODULE_EXIT_CODE_TRAMPOLINE_ADDRESS
                .u32(),
            Type::I64,
        );
        self.ssa_builder.insert_instruction(
            self.ssa_builder.allocate_instruction().as_call_indirect(
                ptr,
                self.check_module_exit_code_sig.id,
                Values::from_vec(vec![self.exec_ctx_ptr_value]),
            ),
        );
    }

    fn emit_ext_load(&mut self, opcode: Opcode, ptr: Value, offset: u32, ty: Type) -> Value {
        let mut instr = self.ssa_builder.allocate_instruction().with_opcode(opcode);
        instr.v = ptr;
        instr.u1 = offset as u64;
        instr.typ = ty;
        let id = self.ssa_builder.insert_instruction(instr);
        self.ssa_builder.instruction(id).return_()
    }

    fn emit_store(&mut self, value: Value, ptr: Value, offset: u32) {
        self.emit_store_with_opcode(Opcode::Store, value, ptr, offset);
    }

    fn emit_store_with_opcode(&mut self, opcode: Opcode, value: Value, ptr: Value, offset: u32) {
        self.ssa_builder.insert_instruction(
            self.ssa_builder
                .allocate_instruction()
                .as_store(opcode, value, ptr, offset),
        );
    }

    fn emit_iconst64(&mut self, value: u64) -> Value {
        let id = self
            .ssa_builder
            .insert_instruction(self.ssa_builder.allocate_instruction().as_iconst64(value));
        self.ssa_builder.instruction(id).return_()
    }

    fn insert_integer_extend(&mut self, signed: bool, from: u8, to: u8) {
        if self.lowering_state.unreachable {
            return;
        }
        let value = self.lowering_state.pop();
        let instr = if signed {
            self.ssa_builder
                .allocate_instruction()
                .as_sextend(value, from, to)
        } else {
            self.ssa_builder
                .allocate_instruction()
                .as_uextend(value, from, to)
        };
        let id = self.ssa_builder.insert_instruction(instr);
        self.lowering_state
            .push(self.ssa_builder.instruction(id).return_());
    }

    fn emit_uextend(&mut self, value: Value, ty: Type) -> Value {
        let id = self.ssa_builder.insert_instruction(
            self.ssa_builder.allocate_instruction().as_uextend(
                value,
                value.ty().bits() as u8,
                ty.bits() as u8,
            ),
        );
        self.ssa_builder.instruction(id).return_()
    }

    fn emit_icmp(&mut self, x: Value, y: Value, cond: IntegerCmpCond) -> Value {
        let id = self
            .ssa_builder
            .insert_instruction(self.ssa_builder.allocate_instruction().as_icmp(x, y, cond));
        self.ssa_builder.instruction(id).return_()
    }

    fn emit_exit_if_true_with_code(&mut self, ctx: Value, cond: Value, code: ExitCode) {
        self.ssa_builder.insert_instruction(
            self.ssa_builder
                .allocate_instruction()
                .as_exit_if_true_with_code(ctx, cond, code),
        );
    }

    fn store_caller_module_context(&mut self) {
        self.emit_store(
            self.module_ctx_ptr_value,
            self.exec_ctx_ptr_value,
            EXECUTION_CONTEXT_OFFSET_CALLER_MODULE_CONTEXT_PTR.u32(),
        );
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
    use crate::wazevoapi::ModuleContextOffsetData;
    use razero_wasm::module::{Code, FunctionType, Module, ValueType};

    fn function_type(params: &[ValueType], results: &[ValueType]) -> FunctionType {
        let mut ty = FunctionType::default();
        ty.params.extend_from_slice(params);
        ty.results.extend_from_slice(results);
        ty
    }

    fn compiler_for(module: &Module) -> Compiler<'_> {
        compiler_for_with_ensure_termination(module, false)
    }

    fn compiler_for_with_ensure_termination(
        module: &Module,
        ensure_termination: bool,
    ) -> Compiler<'_> {
        compiler_for_with_flags(module, ensure_termination, false)
    }

    fn compiler_for_with_fuel_enabled(module: &Module, fuel_enabled: bool) -> Compiler<'_> {
        compiler_for_with_flags(module, false, fuel_enabled)
    }

    fn compiler_for_with_memory_isolation_enabled(
        module: &Module,
        memory_isolation_enabled: bool,
    ) -> Compiler<'_> {
        Compiler::new(
            module,
            Builder::new(),
            Some(ModuleContextOffsetData::new(module, false)),
            false,
            false,
            false,
            false,
            memory_isolation_enabled,
        )
    }

    fn compiler_for_with_flags(
        module: &Module,
        ensure_termination: bool,
        fuel_enabled: bool,
    ) -> Compiler<'_> {
        Compiler::new(
            module,
            Builder::new(),
            Some(ModuleContextOffsetData::new(module, false)),
            ensure_termination,
            false,
            false,
            fuel_enabled,
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
    fn lowers_imported_call_via_module_context_indirection() {
        let ty = function_type(&[ValueType::I32], &[ValueType::I32]);
        let module = Module {
            type_section: vec![ty.clone()],
            import_section: vec![wasm::Import {
                ty: wasm::ExternType::FUNC,
                module: "env".into(),
                name: "f".into(),
                desc: wasm::ImportDesc::Func(0),
                index_per_type: 0,
            }],
            import_function_count: 1,
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_I32_CONST, 7, OPCODE_CALL, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(1, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i32 = Iconst 7\n\tStore module_ctx, exec_ctx, 0x8\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Load module_ctx, 0x10\n\tv6:i32 = CallIndirect sig0, v4, exec_ctx, v5, v3\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_loop_header_module_exit_check_when_termination_is_enabled() {
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOOP, 0x40, OPCODE_BR, 0, OPCODE_END, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_ensure_termination(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        let formatted = compiler.format();
        let offset = crate::wazevoapi::offsetdata::EXECUTION_CONTEXT_OFFSET_CHECK_MODULE_EXIT_CODE_TRAMPOLINE_ADDRESS
            .u32();
        assert!(formatted.contains(&format!("Load exec_ctx, {offset:#x}")));
        assert!(formatted.contains("CallIndirect sig1,"));
        assert!(formatted.contains("exec_ctx"));
    }

    #[test]
    fn lowers_loop_header_fuel_check_when_fuel_is_enabled() {
        let module = Module {
            type_section: vec![function_type(&[], &[])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOOP, 0x40, OPCODE_BR, 0, OPCODE_END, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_fuel_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        let formatted = compiler.format();
        let fuel_offset = EXECUTION_CONTEXT_OFFSET_FUEL.u32();
        assert!(formatted.contains(&format!("Load exec_ctx, {fuel_offset:#x}")));
        assert!(formatted.contains("Store v"));
        assert!(formatted.contains(&format!(", exec_ctx, {fuel_offset:#x}")));
        assert!(formatted.contains("ExitIfTrueWithCode"));
        assert!(formatted.contains("fuel_exhausted"));
    }

    #[test]
    fn lowers_function_entry_fuel_check_when_fuel_is_enabled() {
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_I32_CONST, 7, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_fuel_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        let formatted = compiler.format();
        let fuel_offset = EXECUTION_CONTEXT_OFFSET_FUEL.u32();
        assert!(formatted.contains(&format!("Load exec_ctx, {fuel_offset:#x}")));
        assert!(formatted.contains("Store v"));
        assert!(formatted.contains(&format!(", exec_ctx, {fuel_offset:#x}")));
        assert!(formatted.contains("ExitIfTrueWithCode"));
        assert!(formatted.contains("fuel_exhausted"));
        assert!(formatted.contains("Jump blk_ret"));
    }

    #[test]
    fn lowers_i32_div_s_with_trap_check() {
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
                    OPCODE_I32_DIV_S,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i32)\n\tv4:i32 = Sdiv v2, v3, exec_ctx\n\tJump blk_ret, v4\n"
        );
    }

    #[test]
    fn lowers_i64_div_u_with_trap_check() {
        let module = Module {
            type_section: vec![function_type(
                &[ValueType::I64, ValueType::I64],
                &[ValueType::I64],
            )],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I64_DIV_U,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i64, v3:i64)\n\tv4:i64 = Udiv v2, v3, exec_ctx\n\tJump blk_ret, v4\n"
        );
    }

    #[test]
    fn lowers_i32_rem_s_with_trap_check() {
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
                    OPCODE_I32_REM_S,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i32)\n\tv4:i32 = Srem v2, v3, exec_ctx\n\tJump blk_ret, v4\n"
        );
    }

    #[test]
    fn lowers_i64_rem_u_with_trap_check() {
        let module = Module {
            type_section: vec![function_type(
                &[ValueType::I64, ValueType::I64],
                &[ValueType::I64],
            )],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I64_REM_U,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i64, v3:i64)\n\tv4:i64 = Urem v2, v3, exec_ctx\n\tJump blk_ret, v4\n"
        );
    }

    #[test]
    fn lowers_f32_sqrt_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F32], &[ValueType::F32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F32_SQRT, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f32)\n\tv3:f32 = Sqrt v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f64_sqrt_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F64], &[ValueType::F64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F64_SQRT, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f64)\n\tv3:f64 = Sqrt v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f32_ceil_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F32], &[ValueType::F32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F32_CEIL, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f32)\n\tv3:f32 = Ceil v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f64_floor_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F64], &[ValueType::F64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F64_FLOOR, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f64)\n\tv3:f64 = Floor v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f32_trunc_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F32], &[ValueType::F32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F32_TRUNC, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f32)\n\tv3:f32 = Trunc v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f64_nearest_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F64], &[ValueType::F64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F64_NEAREST, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f64)\n\tv3:f64 = Nearest v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f32_min_to_ssa() {
        let module = Module {
            type_section: vec![function_type(
                &[ValueType::F32, ValueType::F32],
                &[ValueType::F32],
            )],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_F32_MIN,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f32, v3:f32)\n\tv4:f32 = Fmin v2, v3\n\tJump blk_ret, v4\n"
        );
    }

    #[test]
    fn lowers_f64_promote_f32_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F32], &[ValueType::F64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F64_PROMOTE_F32, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f32)\n\tv3:f64 = Fpromote v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f32_convert_i32_s_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::F32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F32_CONVERT_I32_S, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:f32 = FcvtFromSint v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i32_reinterpret_f32_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_REINTERPRET_F32, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f32)\n\tv3:i32 = Bitcast v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i32_wrap_i64_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I64], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_WRAP_I64, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i64)\n\tv3:i32 = Ireduce v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f32_abs_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F32], &[ValueType::F32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F32_ABS, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f32)\n\tv3:f32 = Fabs v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f64_neg_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F64], &[ValueType::F64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F64_NEG, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f64)\n\tv3:f64 = Fneg v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f32_copysign_to_ssa() {
        let module = Module {
            type_section: vec![function_type(
                &[ValueType::F32, ValueType::F32],
                &[ValueType::F32],
            )],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_F32_COPYSIGN,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f32, v3:f32)\n\tv4:f32 = Fcopysign v2, v3\n\tJump blk_ret, v4\n"
        );
    }

    #[test]
    fn lowers_i32_trunc_f32_s_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_TRUNC_F32_S, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f32)\n\tv3:i32 = FcvtToSint v2, exec_ctx\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i32_trunc_f64_s_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F64], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_TRUNC_F64_S, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f64)\n\tv3:i32 = FcvtToSint v2, exec_ctx\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i32_trunc_f32_u_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_TRUNC_F32_U, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f32)\n\tv3:i32 = FcvtToUint v2, exec_ctx\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i32_trunc_f64_u_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F64], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_TRUNC_F64_U, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f64)\n\tv3:i32 = FcvtToUint v2, exec_ctx\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i64_trunc_f32_s_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F32], &[ValueType::I64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_TRUNC_F32_S, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f32)\n\tv3:i64 = FcvtToSint v2, exec_ctx\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i64_trunc_f64_s_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F64], &[ValueType::I64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_TRUNC_F64_S, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f64)\n\tv3:i64 = FcvtToSint v2, exec_ctx\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i64_trunc_f32_u_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F32], &[ValueType::I64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_TRUNC_F32_U, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f32)\n\tv3:i64 = FcvtToUint v2, exec_ctx\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i64_trunc_f64_u_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F64], &[ValueType::I64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_TRUNC_F64_U, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f64)\n\tv3:i64 = FcvtToUint v2, exec_ctx\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i32_extend8_s_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_EXTEND8_S, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i32 = SExtend v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i64_extend32_s_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I64], &[ValueType::I64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_EXTEND32_S, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i64)\n\tv3:i64 = SExtend v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i64_extend_i32_s_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_EXTEND_I32_S, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = SExtend v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i64_extend_i32_u_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_EXTEND_I32_U, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f32_reinterpret_i32_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::F32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F32_REINTERPRET_I32, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:f32 = Bitcast v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f64_convert_i32_s_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::F64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F64_CONVERT_I32_S, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:f64 = FcvtFromSint v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i64_reinterpret_f64_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F64], &[ValueType::I64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_REINTERPRET_F64, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f64)\n\tv3:i64 = Bitcast v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f64_reinterpret_i64_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I64], &[ValueType::F64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F64_REINTERPRET_I64, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i64)\n\tv3:f64 = Bitcast v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f32_convert_i32_u_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::F32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F32_CONVERT_I32_U, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:f32 = FcvtFromUint v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f32_convert_i64_u_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I64], &[ValueType::F32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F32_CONVERT_I64_U, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i64)\n\tv3:f32 = FcvtFromUint v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f32_convert_i64_s_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I64], &[ValueType::F32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F32_CONVERT_I64_S, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i64)\n\tv3:f32 = FcvtFromSint v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f64_convert_i32_u_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::F64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F64_CONVERT_I32_U, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:f64 = FcvtFromUint v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f64_convert_i64_u_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I64], &[ValueType::F64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F64_CONVERT_I64_U, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i64)\n\tv3:f64 = FcvtFromUint v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f64_convert_i64_s_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I64], &[ValueType::F64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F64_CONVERT_I64_S, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i64)\n\tv3:f64 = FcvtFromSint v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f32_demote_f64_to_ssa() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::F64], &[ValueType::F32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F32_DEMOTE_F64, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f64)\n\tv3:f32 = Fdemote v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_f64_max_to_ssa() {
        let module = Module {
            type_section: vec![function_type(
                &[ValueType::F64, ValueType::F64],
                &[ValueType::F64],
            )],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_F64_MAX,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:f64, v3:f64)\n\tv4:f64 = Fmax v2, v3\n\tJump blk_ret, v4\n"
        );
    }

    #[test]
    fn lowers_call_indirect_with_table_checks() {
        let empty = function_type(&[], &[]);
        let callee = function_type(&[], &[ValueType::I32]);
        let caller = function_type(&[ValueType::I32], &[ValueType::I32]);
        let module = Module {
            type_section: vec![empty.clone(), empty, callee.clone(), caller],
            function_section: vec![3],
            table_section: vec![wasm::Table {
                min: 1,
                max: None,
                ty: wasm::RefType::FUNCREF,
            }],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_CALL_INDIRECT, 2, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = Load module_ctx, 0x10\n\tv4:i32 = Load v3, 0x8\n\tv5:i32 = Icmp v2, v4\n\tExitIfTrueWithCode v5, exec_ctx, table_out_of_bounds\n\tv6:i64 = Load v3, 0x0\n\tv7:i64 = Iconst 3\n\tv8:i32 = Ishl v2, v7\n\tv9:i64 = Iadd v6, v8\n\tv10:i64 = Load v9, 0x0\n\tv11:i64 = Iconst 0\n\tv12:i32 = Icmp v10, v11\n\tExitIfTrueWithCode v12, exec_ctx, indirect_call_null_pointer\n\tv13:i32 = Load v10, 0x10\n\tv14:i64 = Load module_ctx, 0x8\n\tv15:i32 = Load v14, 0x8\n\tv16:i32 = Icmp v13, v15\n\tExitIfTrueWithCode v16, exec_ctx, indirect_call_type_mismatch\n\tv17:i64 = Load v10, 0x0\n\tv18:i64 = Load v10, 0x8\n\tStore module_ctx, exec_ctx, 0x8\n\tv19:i32 = CallIndirect sig2, v17, exec_ctx, v18\n\tJump blk_ret, v19\n"
        );
    }

    #[test]
    fn lowers_br_table_with_trampoline_blocks() {
        let module = Module {
            type_section: vec![
                function_type(&[ValueType::I32], &[ValueType::I32]),
                function_type(&[], &[]),
            ],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![
                    OPCODE_BLOCK,
                    1,
                    OPCODE_BLOCK,
                    1,
                    OPCODE_BLOCK,
                    1,
                    OPCODE_BLOCK,
                    1,
                    OPCODE_BLOCK,
                    1,
                    OPCODE_BLOCK,
                    1,
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_BR_TABLE,
                    6,
                    0,
                    1,
                    2,
                    3,
                    4,
                    5,
                    0,
                    OPCODE_END,
                    OPCODE_I32_CONST,
                    11,
                    OPCODE_RETURN,
                    OPCODE_END,
                    OPCODE_I32_CONST,
                    12,
                    OPCODE_RETURN,
                    OPCODE_END,
                    OPCODE_I32_CONST,
                    13,
                    OPCODE_RETURN,
                    OPCODE_END,
                    OPCODE_I32_CONST,
                    14,
                    OPCODE_RETURN,
                    OPCODE_END,
                    OPCODE_I32_CONST,
                    15,
                    OPCODE_RETURN,
                    OPCODE_END,
                    OPCODE_I32_CONST,
                    16,
                    OPCODE_RETURN,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tBrTable v2, [blk7, blk8, blk9, blk10, blk11, blk12, blk13]\n\nblk1: () <-- (blk12)\n\tv8:i32 = Iconst 16\n\tReturn v8\n\nblk2: () <-- (blk11)\n\tv7:i32 = Iconst 15\n\tReturn v7\n\nblk3: () <-- (blk10)\n\tv6:i32 = Iconst 14\n\tReturn v6\n\nblk4: () <-- (blk9)\n\tv5:i32 = Iconst 13\n\tReturn v5\n\nblk5: () <-- (blk8)\n\tv4:i32 = Iconst 12\n\tReturn v4\n\nblk6: () <-- (blk7, blk13)\n\tv3:i32 = Iconst 11\n\tReturn v3\n\nblk7: () <-- (blk0)\n\tv6:invalid = Jump blk6\n\nblk8: () <-- (blk0)\n\tv5:invalid = Jump blk5\n\nblk9: () <-- (blk0)\n\tv4:invalid = Jump blk4\n\nblk10: () <-- (blk0)\n\tv3:invalid = Jump blk3\n\nblk11: () <-- (blk0)\n\tv2:invalid = Jump blk2\n\nblk12: () <-- (blk0)\n\tmodule_ctx:invalid = Jump blk1\n\nblk13: () <-- (blk0)\n\tv6:invalid = Jump blk6\n"
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

    #[test]
    fn lowers_i32_load8_u_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_LOAD8_U, 0, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 1\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:i32 = Uload8 v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_i32_load8_u_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_LOAD8_U, 0, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:i32 = Uload8 v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i32_load_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_LOAD, 2, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:i32 = Load v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i64_load_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD, 3, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:i64 = Load v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i64_load32_s_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD32_S, 2, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:i64 = Sload32 v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i64_load32_u_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD32_U, 2, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:i64 = Uload32 v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i64_load8_s_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD8_S, 0, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:i64 = Sload8 v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i64_load8_u_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD8_U, 0, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:i64 = Uload8 v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i64_load16_s_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD16_S, 1, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:i64 = Sload16 v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i64_load16_u_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD16_U, 1, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:i64 = Uload16 v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i32_load8_s_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_LOAD8_S, 0, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:i32 = Sload8 v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i32_load16_s_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_LOAD16_S, 1, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:i32 = Sload16 v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i32_load16_u_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_LOAD16_U, 1, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:i32 = Uload16 v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i32_load8_s_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_LOAD8_S, 0, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 1\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:i32 = Sload8 v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_i32_load16_u_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_LOAD16_U, 1, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 2\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:i32 = Uload16 v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_i32_load16_s_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_LOAD16_S, 1, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 2\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:i32 = Sload16 v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_i64_load32_s_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD32_S, 2, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 4\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:i64 = Sload32 v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_i64_load32_u_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD32_U, 2, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 4\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:i64 = Uload32 v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_i64_load8_s_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD8_S, 0, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 1\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:i64 = Sload8 v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_i64_load8_u_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD8_U, 0, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 1\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:i64 = Uload8 v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_i64_load16_s_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD16_S, 1, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 2\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:i64 = Sload16 v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_i64_load16_u_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD16_U, 1, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 2\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:i64 = Uload16 v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_i32_clz() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_CLZ, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i32 = Clz v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i64_ctz() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I64], &[ValueType::I64])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_CTZ, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i64)\n\tv3:i64 = Ctz v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i32_popcnt() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_POPCNT, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i32 = Popcnt v2\n\tJump blk_ret, v3\n"
        );
    }

    #[test]
    fn lowers_i32_load_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I32_LOAD, 2, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 4\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:i32 = Load v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_i64_load_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_I64_LOAD, 3, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 8\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:i64 = Load v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_f32_load_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::F32])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F32_LOAD, 2, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 4\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:f32 = Load v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_f64_load_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::F64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F64_LOAD, 3, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for(&module);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Iconst 8\n\tv5:i64 = Iadd v3, v4\n\tv6:i64 = Uload32 module_ctx, 0x10\n\tv7:i32 = Icmp v6, v5\n\tExitIfTrueWithCode v7, exec_ctx, memory_out_of_bounds\n\tv8:i64 = Load module_ctx, 0x8\n\tv9:i64 = Iadd v8, v3\n\tv10:f64 = Load v9, 0x0\n\tJump blk_ret, v10\n"
        );
    }

    #[test]
    fn lowers_f32_load_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::F32])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F32_LOAD, 2, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:f32 = Load v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_f64_load_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::F64])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![OPCODE_LOCAL_GET, 0, OPCODE_F64_LOAD, 3, 0, OPCODE_END],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32)\n\tv3:i64 = UExtend v2\n\tv4:i64 = Load module_ctx, 0x8\n\tv5:i64 = Iadd v4, v3\n\tv6:f64 = Load v5, 0x0\n\tJump blk_ret, v6\n"
        );
    }

    #[test]
    fn lowers_i32_store_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I32], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I32_STORE,
                    2,
                    0,
                    OPCODE_END,
                ],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i32)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Load module_ctx, 0x8\n\tv6:i64 = Iadd v5, v4\n\tStore v3, v6, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i64_store_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I64], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I64_STORE,
                    3,
                    0,
                    OPCODE_END,
                ],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i64)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Load module_ctx, 0x8\n\tv6:i64 = Iadd v5, v4\n\tStore v3, v6, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i32_store8_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I32], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I32_STORE8,
                    0,
                    0,
                    OPCODE_END,
                ],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i32)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Load module_ctx, 0x8\n\tv6:i64 = Iadd v5, v4\n\tIstore8 v3, v6, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i32_store16_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I32], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I32_STORE16,
                    1,
                    0,
                    OPCODE_END,
                ],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i32)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Load module_ctx, 0x8\n\tv6:i64 = Iadd v5, v4\n\tIstore16 v3, v6, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i64_store32_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I64], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I64_STORE32,
                    2,
                    0,
                    OPCODE_END,
                ],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i64)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Load module_ctx, 0x8\n\tv6:i64 = Iadd v5, v4\n\tIstore32 v3, v6, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i64_store8_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I64], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I64_STORE8,
                    0,
                    0,
                    OPCODE_END,
                ],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i64)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Load module_ctx, 0x8\n\tv6:i64 = Iadd v5, v4\n\tIstore8 v3, v6, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i64_store16_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I64], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I64_STORE16,
                    1,
                    0,
                    OPCODE_END,
                ],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i64)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Load module_ctx, 0x8\n\tv6:i64 = Iadd v5, v4\n\tIstore16 v3, v6, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i32_store_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I32], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I32_STORE,
                    2,
                    0,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i32)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Iconst 4\n\tv6:i64 = Iadd v4, v5\n\tv7:i64 = Uload32 module_ctx, 0x10\n\tv8:i32 = Icmp v7, v6\n\tExitIfTrueWithCode v8, exec_ctx, memory_out_of_bounds\n\tv9:i64 = Load module_ctx, 0x8\n\tv10:i64 = Iadd v9, v4\n\tStore v3, v10, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i64_store_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I64], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I64_STORE,
                    3,
                    0,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i64)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Iconst 8\n\tv6:i64 = Iadd v4, v5\n\tv7:i64 = Uload32 module_ctx, 0x10\n\tv8:i32 = Icmp v7, v6\n\tExitIfTrueWithCode v8, exec_ctx, memory_out_of_bounds\n\tv9:i64 = Load module_ctx, 0x8\n\tv10:i64 = Iadd v9, v4\n\tStore v3, v10, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_f32_store_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::F32], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_F32_STORE,
                    2,
                    0,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:f32)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Iconst 4\n\tv6:i64 = Iadd v4, v5\n\tv7:i64 = Uload32 module_ctx, 0x10\n\tv8:i32 = Icmp v7, v6\n\tExitIfTrueWithCode v8, exec_ctx, memory_out_of_bounds\n\tv9:i64 = Load module_ctx, 0x8\n\tv10:i64 = Iadd v9, v4\n\tStore v3, v10, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_f64_store_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::F64], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_F64_STORE,
                    3,
                    0,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:f64)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Iconst 8\n\tv6:i64 = Iadd v4, v5\n\tv7:i64 = Uload32 module_ctx, 0x10\n\tv8:i32 = Icmp v7, v6\n\tExitIfTrueWithCode v8, exec_ctx, memory_out_of_bounds\n\tv9:i64 = Load module_ctx, 0x8\n\tv10:i64 = Iadd v9, v4\n\tStore v3, v10, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_f32_store_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::F32], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_F32_STORE,
                    2,
                    0,
                    OPCODE_END,
                ],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:f32)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Load module_ctx, 0x8\n\tv6:i64 = Iadd v5, v4\n\tStore v3, v6, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_f64_store_without_local_memory_bounds_check_when_memory_isolation_enabled() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::F64], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_F64_STORE,
                    3,
                    0,
                    OPCODE_END,
                ],
                ..Code::default()
            }],
            ..Module::default()
        };

        let mut compiler = compiler_for_with_memory_isolation_enabled(&module, true);
        compiler.init_with_module_function(0, false);
        compiler.lower_to_ssa();

        assert_eq!(
            compiler.format(),
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:f64)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Load module_ctx, 0x8\n\tv6:i64 = Iadd v5, v4\n\tStore v3, v6, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i32_store8_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I32], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I32_STORE8,
                    0,
                    0,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i32)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Iconst 1\n\tv6:i64 = Iadd v4, v5\n\tv7:i64 = Uload32 module_ctx, 0x10\n\tv8:i32 = Icmp v7, v6\n\tExitIfTrueWithCode v8, exec_ctx, memory_out_of_bounds\n\tv9:i64 = Load module_ctx, 0x8\n\tv10:i64 = Iadd v9, v4\n\tIstore8 v3, v10, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i32_store16_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I32], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I32_STORE16,
                    1,
                    0,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i32)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Iconst 2\n\tv6:i64 = Iadd v4, v5\n\tv7:i64 = Uload32 module_ctx, 0x10\n\tv8:i32 = Icmp v7, v6\n\tExitIfTrueWithCode v8, exec_ctx, memory_out_of_bounds\n\tv9:i64 = Load module_ctx, 0x8\n\tv10:i64 = Iadd v9, v4\n\tIstore16 v3, v10, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i64_store32_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I64], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I64_STORE32,
                    2,
                    0,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i64)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Iconst 4\n\tv6:i64 = Iadd v4, v5\n\tv7:i64 = Uload32 module_ctx, 0x10\n\tv8:i32 = Icmp v7, v6\n\tExitIfTrueWithCode v8, exec_ctx, memory_out_of_bounds\n\tv9:i64 = Load module_ctx, 0x8\n\tv10:i64 = Iadd v9, v4\n\tIstore32 v3, v10, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i64_store8_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I64], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I64_STORE8,
                    0,
                    0,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i64)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Iconst 1\n\tv6:i64 = Iadd v4, v5\n\tv7:i64 = Uload32 module_ctx, 0x10\n\tv8:i32 = Icmp v7, v6\n\tExitIfTrueWithCode v8, exec_ctx, memory_out_of_bounds\n\tv9:i64 = Load module_ctx, 0x8\n\tv10:i64 = Iadd v9, v4\n\tIstore8 v3, v10, 0x0\n\tJump blk_ret\n"
        );
    }

    #[test]
    fn lowers_i64_store16_with_local_memory_bounds_check() {
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32, ValueType::I64], &[])],
            function_section: vec![0],
            memory_section: Some(wasm::Memory {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            code_section: vec![Code {
                body: vec![
                    OPCODE_LOCAL_GET,
                    0,
                    OPCODE_LOCAL_GET,
                    1,
                    OPCODE_I64_STORE16,
                    1,
                    0,
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
            "\nblk0: (exec_ctx:i64, module_ctx:i64, v2:i32, v3:i64)\n\tv4:i64 = UExtend v2\n\tv5:i64 = Iconst 2\n\tv6:i64 = Iadd v4, v5\n\tv7:i64 = Uload32 module_ctx, 0x10\n\tv8:i32 = Icmp v7, v6\n\tExitIfTrueWithCode v8, exec_ctx, memory_out_of_bounds\n\tv9:i64 = Load module_ctx, 0x8\n\tv10:i64 = Iadd v9, v4\n\tIstore16 v3, v10, 0x0\n\tJump blk_ret\n"
        );
    }
}
