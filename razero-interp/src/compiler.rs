#![doc = "Wasm-to-interpreter lowering."]

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use crate::operations::{
    FloatKind, InclusiveRange, Instruction, Label, LabelKind, MemoryArg, OperationKind, SignedInt,
    SignedType, UnsignedInt, UnsignedType,
};

const OPCODE_MISC_PREFIX: u8 = 0xfc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueType {
    I32,
    I64,
    F32,
    F64,
    V128,
    FuncRef,
    ExternRef,
}

impl ValueType {
    fn from_block_byte(byte: u8) -> Option<Self> {
        match byte {
            0x7f => Some(Self::I32),
            0x7e => Some(Self::I64),
            0x7d => Some(Self::F32),
            0x7c => Some(Self::F64),
            0x7b => Some(Self::V128),
            0x70 => Some(Self::FuncRef),
            0x6f => Some(Self::ExternRef),
            _ => None,
        }
    }

    fn as_stack_type(self) -> UnsignedType {
        match self {
            Self::I32 => UnsignedType::I32,
            Self::I64 | Self::FuncRef | Self::ExternRef => UnsignedType::I64,
            Self::F32 => UnsignedType::F32,
            Self::F64 => UnsignedType::F64,
            Self::V128 => UnsignedType::V128,
        }
    }

    fn slots(self) -> usize {
        usize::from(matches!(self, Self::V128)) + 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FunctionType {
    pub params: Vec<ValueType>,
    pub results: Vec<ValueType>,
    pub param_num_in_u64: usize,
    pub result_num_in_u64: usize,
}

impl FunctionType {
    pub fn new(params: Vec<ValueType>, results: Vec<ValueType>) -> Self {
        let param_num_in_u64 = params.iter().map(|ty| ty.slots()).sum();
        let result_num_in_u64 = results.iter().map(|ty| ty.slots()).sum();
        Self {
            params,
            results,
            param_num_in_u64,
            result_num_in_u64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalType {
    pub value_type: ValueType,
}

#[derive(Debug, Clone)]
pub struct CompileConfig<'a> {
    pub body: &'a [u8],
    pub signature: FunctionType,
    pub local_types: &'a [ValueType],
    pub globals: &'a [GlobalType],
    pub functions: &'a [u32],
    pub types: &'a [FunctionType],
    pub call_frame_stack_size_in_u64: usize,
    pub ensure_termination: bool,
}

impl<'a> CompileConfig<'a> {
    pub fn new(body: &'a [u8]) -> Self {
        Self {
            body,
            signature: FunctionType::default(),
            local_types: &[],
            globals: &[],
            functions: &[],
            types: &[],
            call_frame_stack_size_in_u64: 0,
            ensure_termination: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompilationResult {
    pub operations: Vec<Instruction>,
    pub label_callers: HashMap<Label, u32>,
    pub uses_memory: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compiler;

impl Compiler {
    pub fn lower(&self, body: &[u8]) -> Result<Vec<Instruction>, CompileError> {
        self.lower_with_config(CompileConfig::new(body))
            .map(|result| result.operations)
    }

    pub fn lower_with_config(
        &self,
        config: CompileConfig<'_>,
    ) -> Result<CompilationResult, CompileError> {
        Lowerer::new(config).lower()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError(String);

impl CompileError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for CompileError {}

impl From<Leb128Error> for CompileError {
    fn from(value: Leb128Error) -> Self {
        Self::new(value.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlFrameKind {
    BlockWithContinuationLabel,
    BlockWithoutContinuationLabel,
    Function,
    Loop,
    IfWithElse,
    IfWithoutElse,
}

#[derive(Debug, Clone)]
struct ControlFrame {
    frame_id: u32,
    original_stack_len_without_param: usize,
    original_stack_len_without_param_u64: usize,
    block_type: FunctionType,
    kind: ControlFrameKind,
}

impl ControlFrame {
    fn ensure_continuation(&mut self) {
        if self.kind == ControlFrameKind::BlockWithoutContinuationLabel {
            self.kind = ControlFrameKind::BlockWithContinuationLabel;
        }
    }

    fn as_label(&self) -> Label {
        match self.kind {
            ControlFrameKind::BlockWithContinuationLabel
            | ControlFrameKind::BlockWithoutContinuationLabel
            | ControlFrameKind::IfWithElse
            | ControlFrameKind::IfWithoutElse => Label::new(LabelKind::Continuation, self.frame_id),
            ControlFrameKind::Loop => Label::new(LabelKind::Header, self.frame_id),
            ControlFrameKind::Function => Label::new(LabelKind::Return, 0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SigType {
    Known(UnsignedType),
    Unknown,
}

#[derive(Debug, Clone, Default)]
struct Signature {
    input: Vec<SigType>,
    output: Vec<SigType>,
}

struct Lowerer<'a> {
    config: CompileConfig<'a>,
    result: CompilationResult,
    stack: Vec<UnsignedType>,
    stack_len_in_u64: usize,
    current_frame_id: u32,
    control_frames: Vec<ControlFrame>,
    unreachable_on: bool,
    unreachable_depth: usize,
    pc: usize,
    local_index_to_stack_height_in_u64: Vec<usize>,
}

impl<'a> Lowerer<'a> {
    fn new(config: CompileConfig<'a>) -> Self {
        Self {
            config,
            result: CompilationResult::default(),
            stack: Vec::new(),
            stack_len_in_u64: 0,
            current_frame_id: 0,
            control_frames: Vec::new(),
            unreachable_on: false,
            unreachable_depth: 0,
            pc: 0,
            local_index_to_stack_height_in_u64: Vec::new(),
        }
    }

    fn lower(mut self) -> Result<CompilationResult, CompileError> {
        self.initialize_stack();

        for ty in self.config.local_types {
            self.emit_default_value(*ty);
        }

        let frame_id = self.next_frame_id();
        self.control_frames.push(ControlFrame {
            frame_id,
            original_stack_len_without_param: 0,
            original_stack_len_without_param_u64: 0,
            block_type: self.config.signature.clone(),
            kind: ControlFrameKind::Function,
        });

        while !self.control_frames.is_empty() && self.pc < self.config.body.len() {
            self.handle_instruction()?;
        }
        Ok(self.result)
    }

    fn initialize_stack(&mut self) {
        self.local_index_to_stack_height_in_u64.clear();
        let mut current = 0usize;
        for ty in &self.config.signature.params {
            self.local_index_to_stack_height_in_u64.push(current);
            current += ty.slots();
        }

        if self.config.call_frame_stack_size_in_u64 > 0 {
            let diff = self
                .config
                .signature
                .result_num_in_u64
                .saturating_sub(self.config.signature.param_num_in_u64);
            current += diff + self.config.call_frame_stack_size_in_u64;
        }

        for ty in self.config.local_types {
            self.local_index_to_stack_height_in_u64.push(current);
            current += ty.slots();
        }

        let params = self.config.signature.params.clone();
        for ty in params {
            self.stack_push(ty.as_stack_type());
        }

        if self.config.call_frame_stack_size_in_u64 > 0 {
            let diff = self
                .config
                .signature
                .result_num_in_u64
                .saturating_sub(self.config.signature.param_num_in_u64);
            for _ in 0..diff {
                self.stack_push(UnsignedType::I64);
            }
            for _ in 0..self.config.call_frame_stack_size_in_u64 {
                self.stack_push(UnsignedType::I64);
            }
        }
    }

    fn handle_instruction(&mut self) -> Result<(), CompileError> {
        let opcode = self.byte_at(self.pc)?;
        let peek_value_type = self.stack.last().copied();
        let index = self.apply_to_stack(opcode)?;

        match opcode {
            0x00 => {
                self.emit(Instruction::unreachable());
                self.mark_unreachable();
            }
            0x01 => {}
            0x02 => {
                let block_type = self.decode_block_type()?;
                if self.unreachable_on {
                    self.unreachable_depth += 1;
                } else {
                    let frame_id = self.next_frame_id();
                    self.control_frames.push(ControlFrame {
                        frame_id,
                        original_stack_len_without_param: self.stack.len()
                            - block_type.params.len(),
                        original_stack_len_without_param_u64: self.stack_len_in_u64
                            - block_type.param_num_in_u64,
                        block_type,
                        kind: ControlFrameKind::BlockWithoutContinuationLabel,
                    });
                }
            }
            0x03 => {
                let block_type = self.decode_block_type()?;
                if self.unreachable_on {
                    self.unreachable_depth += 1;
                } else {
                    let frame_id = self.next_frame_id();
                    self.control_frames.push(ControlFrame {
                        frame_id,
                        original_stack_len_without_param: self.stack.len()
                            - block_type.params.len(),
                        original_stack_len_without_param_u64: self.stack_len_in_u64
                            - block_type.param_num_in_u64,
                        block_type,
                        kind: ControlFrameKind::Loop,
                    });
                    let loop_label = Label::new(LabelKind::Header, frame_id);
                    self.bump_label(loop_label);
                    self.emit(Instruction::br(loop_label));
                    self.emit(Instruction::label(loop_label));
                    if self.config.ensure_termination {
                        self.emit(Instruction::new(
                            OperationKind::BuiltinFunctionCheckExitCode,
                        ));
                    }
                }
            }
            0x04 => {
                let block_type = self.decode_block_type()?;
                if self.unreachable_on {
                    self.unreachable_depth += 1;
                } else {
                    let frame_id = self.next_frame_id();
                    self.control_frames.push(ControlFrame {
                        frame_id,
                        original_stack_len_without_param: self.stack.len()
                            - block_type.params.len(),
                        original_stack_len_without_param_u64: self.stack_len_in_u64
                            - block_type.param_num_in_u64,
                        block_type,
                        kind: ControlFrameKind::IfWithoutElse,
                    });
                    let then_label = Label::new(LabelKind::Header, frame_id);
                    let else_label = Label::new(LabelKind::Else, frame_id);
                    self.bump_label(then_label);
                    self.bump_label(else_label);
                    self.emit(Instruction::br_if(
                        then_label,
                        else_label,
                        InclusiveRange::NOP,
                    ));
                    self.emit(Instruction::label(then_label));
                }
            }
            0x05 => {
                if self.unreachable_on && self.unreachable_depth > 0 {
                } else if self.unreachable_on {
                    let frame = self.top_frame_mut()?;
                    let frame_id = frame.frame_id;
                    let block_type = frame.block_type.clone();
                    frame.kind = ControlFrameKind::IfWithElse;
                    let frame = frame.clone();
                    self.stack_switch_at(&frame);
                    for ty in block_type.params {
                        self.stack_push(ty.as_stack_type());
                    }
                    self.reset_unreachable();
                    self.emit(Instruction::label(Label::new(LabelKind::Else, frame_id)));
                } else {
                    let frame = self.top_frame_mut()?;
                    frame.kind = ControlFrameKind::IfWithElse;
                    let frame = frame.clone();
                    let drop_op = Instruction::drop(self.get_frame_drop_range(&frame, false));
                    self.stack_switch_at(&frame);
                    for ty in &frame.block_type.params {
                        self.stack_push(ty.as_stack_type());
                    }
                    let else_label = Label::new(LabelKind::Else, frame.frame_id);
                    let continuation_label = Label::new(LabelKind::Continuation, frame.frame_id);
                    self.bump_label(continuation_label);
                    self.emit(drop_op);
                    self.emit(Instruction::br(continuation_label));
                    self.emit(Instruction::label(else_label));
                }
            }
            0x0b => {
                if self.unreachable_on && self.unreachable_depth > 0 {
                    self.unreachable_depth -= 1;
                } else if self.unreachable_on {
                    self.reset_unreachable();
                    let frame = self
                        .control_frames
                        .pop()
                        .ok_or_else(|| CompileError::new("missing control frame"))?;
                    if self.control_frames.is_empty() {
                        return Ok(());
                    }
                    self.stack_switch_at(&frame);
                    for ty in &frame.block_type.results {
                        self.stack_push(ty.as_stack_type());
                    }
                    let continuation_label = Label::new(LabelKind::Continuation, frame.frame_id);
                    if frame.kind == ControlFrameKind::IfWithoutElse {
                        let else_label = Label::new(LabelKind::Else, frame.frame_id);
                        self.bump_label(continuation_label);
                        self.emit(Instruction::label(else_label));
                        self.emit(Instruction::br(continuation_label));
                        self.emit(Instruction::label(continuation_label));
                    } else {
                        self.emit(Instruction::label(continuation_label));
                    }
                } else {
                    let frame = self
                        .control_frames
                        .pop()
                        .ok_or_else(|| CompileError::new("missing control frame"))?;
                    let drop_op = Instruction::drop(self.get_frame_drop_range(&frame, true));
                    self.stack_switch_at(&frame);
                    for ty in &frame.block_type.results {
                        self.stack_push(ty.as_stack_type());
                    }
                    match frame.kind {
                        ControlFrameKind::Function => {
                            self.emit(drop_op);
                            self.emit(Instruction::br(Label::new(LabelKind::Return, 0)));
                        }
                        ControlFrameKind::IfWithoutElse => {
                            let else_label = Label::new(LabelKind::Else, frame.frame_id);
                            let continuation_label =
                                Label::new(LabelKind::Continuation, frame.frame_id);
                            self.bump_label_by(continuation_label, 2);
                            self.emit(drop_op);
                            self.emit(Instruction::br(continuation_label));
                            self.emit(Instruction::label(else_label));
                            self.emit(Instruction::br(continuation_label));
                            self.emit(Instruction::label(continuation_label));
                        }
                        ControlFrameKind::BlockWithContinuationLabel
                        | ControlFrameKind::IfWithElse => {
                            let continuation_label =
                                Label::new(LabelKind::Continuation, frame.frame_id);
                            self.bump_label(continuation_label);
                            self.emit(drop_op);
                            self.emit(Instruction::br(continuation_label));
                            self.emit(Instruction::label(continuation_label));
                        }
                        ControlFrameKind::Loop
                        | ControlFrameKind::BlockWithoutContinuationLabel => {
                            self.emit(drop_op);
                        }
                    }
                }
            }
            0x0c => {
                let target_index = decode_u32(&self.config.body[self.pc + 1..])?.0 as usize;
                if !self.unreachable_on {
                    let target_frame = self.frame_at_depth_mut(target_index)?;
                    target_frame.ensure_continuation();
                    let frame = target_frame.clone();
                    let drop_op = Instruction::drop(self.get_frame_drop_range(&frame, false));
                    let target = frame.as_label();
                    self.bump_label(target);
                    self.emit(drop_op);
                    self.emit(Instruction::br(target));
                    self.mark_unreachable();
                }
            }
            0x0d => {
                let target_index = decode_u32(&self.config.body[self.pc + 1..])?.0 as usize;
                if !self.unreachable_on {
                    let target_frame = self.frame_at_depth_mut(target_index)?;
                    target_frame.ensure_continuation();
                    let frame = target_frame.clone();
                    let target = frame.as_label();
                    self.bump_label(target);
                    let continuation = Label::new(LabelKind::Header, self.next_frame_id());
                    self.bump_label(continuation);
                    self.emit(Instruction::br_if(
                        target,
                        continuation,
                        self.get_frame_drop_range(&frame, false),
                    ));
                    self.emit(Instruction::label(continuation));
                }
            }
            0x0e => {
                let (num_targets, bytes_read) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += bytes_read;
                if self.unreachable_on {
                    for _ in 0..=num_targets {
                        let (_, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                        self.pc += consumed;
                    }
                } else {
                    let mut targets = Vec::with_capacity((num_targets as usize + 1) * 2);
                    for _ in 0..num_targets {
                        let (depth, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                        self.pc += consumed;
                        let frame = {
                            let target = self.frame_at_depth_mut(depth as usize)?;
                            target.ensure_continuation();
                            target.clone()
                        };
                        let label = frame.as_label();
                        self.bump_label(label);
                        targets.push(label.into_raw());
                        targets.push(self.get_frame_drop_range(&frame, false).as_u64());
                    }
                    let (depth, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                    self.pc += consumed;
                    let frame = {
                        let target = self.frame_at_depth_mut(depth as usize)?;
                        target.ensure_continuation();
                        target.clone()
                    };
                    let label = frame.as_label();
                    self.bump_label(label);
                    targets.push(label.into_raw());
                    targets.push(self.get_frame_drop_range(&frame, false).as_u64());
                    self.emit(Instruction::br_table(targets));
                    self.mark_unreachable();
                }
            }
            0x0f => {
                let function_frame = self.function_frame()?.clone();
                self.emit(Instruction::drop(
                    self.get_frame_drop_range(&function_frame, false),
                ));
                self.emit(Instruction::br(function_frame.as_label()));
                self.mark_unreachable();
            }
            0x10 => self.emit(Instruction::call(index)),
            0x11 => {
                let (table_index, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(Instruction::call_indirect(index, table_index));
            }
            0x1a => {
                let range = if peek_value_type == Some(UnsignedType::V128) {
                    InclusiveRange::new(0, 1)
                } else {
                    InclusiveRange::new(0, 0)
                };
                self.emit(Instruction::drop(range));
            }
            0x1b => {
                if !self.unreachable_on {
                    self.emit(Instruction::select(
                        self.stack_peek()? == UnsignedType::V128,
                    ));
                }
            }
            0x1c => {
                self.pc += 2;
                if !self.unreachable_on {
                    self.emit(Instruction::select(
                        self.stack_peek()? == UnsignedType::V128,
                    ));
                }
            }
            0x20 => {
                let depth = self.local_depth(index as usize)?;
                let vector = self.local_type(index as usize)? == ValueType::V128;
                self.emit(Instruction::pick(
                    depth - if vector { 2 } else { 1 },
                    vector,
                ));
            }
            0x21 => {
                let depth = self.local_depth(index as usize)?;
                let vector = self.local_type(index as usize)? == ValueType::V128;
                self.emit(Instruction::set(depth + if vector { 2 } else { 1 }, vector));
            }
            0x22 => {
                let depth = self.local_depth(index as usize)?;
                let vector = self.local_type(index as usize)? == ValueType::V128;
                if vector {
                    self.emit(Instruction::pick(1, true));
                    self.emit(Instruction::set(depth + 2, true));
                } else {
                    self.emit(Instruction::pick(0, false));
                    self.emit(Instruction::set(depth + 1, false));
                }
            }
            0x23 => self.emit(Instruction::global_get(index)),
            0x24 => self.emit(Instruction::global_set(index)),
            0x25 => {
                let (table_index, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(Instruction::new(OperationKind::TableGet).with_u1(table_index as u64));
            }
            0x26 => {
                let (table_index, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(Instruction::new(OperationKind::TableSet).with_u1(table_index as u64));
            }
            0x28 => {
                let op = self.read_memory_and(|arg| Instruction::load(UnsignedType::I32, arg))?;
                self.emit(op);
            }
            0x29 => {
                let op = self.read_memory_and(|arg| Instruction::load(UnsignedType::I64, arg))?;
                self.emit(op);
            }
            0x2a => {
                let op = self.read_memory_and(|arg| Instruction::load(UnsignedType::F32, arg))?;
                self.emit(op);
            }
            0x2b => {
                let op = self.read_memory_and(|arg| Instruction::load(UnsignedType::F64, arg))?;
                self.emit(op);
            }
            0x2c => {
                let op = self.read_memory_and(|arg| Instruction::load8(SignedInt::Int32, arg))?;
                self.emit(op);
            }
            0x2d => {
                let op = self.read_memory_and(|arg| Instruction::load8(SignedInt::Uint32, arg))?;
                self.emit(op);
            }
            0x2e => {
                let op = self.read_memory_and(|arg| Instruction::load16(SignedInt::Int32, arg))?;
                self.emit(op);
            }
            0x2f => {
                let op = self.read_memory_and(|arg| Instruction::load16(SignedInt::Uint32, arg))?;
                self.emit(op);
            }
            0x30 => {
                let op = self.read_memory_and(|arg| Instruction::load8(SignedInt::Int64, arg))?;
                self.emit(op);
            }
            0x31 => {
                let op = self.read_memory_and(|arg| Instruction::load8(SignedInt::Uint64, arg))?;
                self.emit(op);
            }
            0x32 => {
                let op = self.read_memory_and(|arg| Instruction::load16(SignedInt::Int64, arg))?;
                self.emit(op);
            }
            0x33 => {
                let op = self.read_memory_and(|arg| Instruction::load16(SignedInt::Uint64, arg))?;
                self.emit(op);
            }
            0x34 => {
                let op = self.read_memory_and(|arg| Instruction::load32(true, arg))?;
                self.emit(op);
            }
            0x35 => {
                let op = self.read_memory_and(|arg| Instruction::load32(false, arg))?;
                self.emit(op);
            }
            0x36 => {
                let op = self.read_memory_and(|arg| Instruction::store(UnsignedType::I32, arg))?;
                self.emit(op);
            }
            0x37 => {
                let op = self.read_memory_and(|arg| Instruction::store(UnsignedType::I64, arg))?;
                self.emit(op);
            }
            0x38 => {
                let op = self.read_memory_and(|arg| Instruction::store(UnsignedType::F32, arg))?;
                self.emit(op);
            }
            0x39 => {
                let op = self.read_memory_and(|arg| Instruction::store(UnsignedType::F64, arg))?;
                self.emit(op);
            }
            0x3a | 0x3c => {
                let op = self.read_memory_and(Instruction::store8)?;
                self.emit(op);
            }
            0x3b | 0x3d => {
                let op = self.read_memory_and(Instruction::store16)?;
                self.emit(op);
            }
            0x3e => {
                let op = self.read_memory_and(Instruction::store32)?;
                self.emit(op);
            }
            0x3f => {
                self.result.uses_memory = true;
                self.pc += 1;
                self.emit(Instruction::new(OperationKind::MemorySize));
            }
            0x40 => {
                self.result.uses_memory = true;
                self.pc += 1;
                self.emit(Instruction::new(OperationKind::MemoryGrow));
            }
            0x41 => {
                let (value, consumed) = decode_i32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(Instruction::const_i32(value as u32));
            }
            0x42 => {
                let (value, consumed) = decode_i64(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(Instruction::const_i64(value as u64));
            }
            0x43 => {
                let value = f32::from_bits(read_le_u32(self.config.body, self.pc + 1)?);
                self.pc += 4;
                self.emit(Instruction::const_f32(value));
            }
            0x44 => {
                let value = f64::from_bits(read_le_u64(self.config.body, self.pc + 1)?);
                self.pc += 8;
                self.emit(Instruction::const_f64(value));
            }
            0x45 => self.emit(op_unsigned_int(OperationKind::Eqz, UnsignedInt::I32)),
            0x46 => self.emit(op_unsigned(OperationKind::Eq, UnsignedType::I32)),
            0x47 => self.emit(op_unsigned(OperationKind::Ne, UnsignedType::I32)),
            0x48 => self.emit(op_signed_type(OperationKind::Lt, SignedType::Int32)),
            0x49 => self.emit(op_signed_type(OperationKind::Lt, SignedType::Uint32)),
            0x4a => self.emit(op_signed_type(OperationKind::Gt, SignedType::Int32)),
            0x4b => self.emit(op_signed_type(OperationKind::Gt, SignedType::Uint32)),
            0x4c => self.emit(op_signed_type(OperationKind::Le, SignedType::Int32)),
            0x4d => self.emit(op_signed_type(OperationKind::Le, SignedType::Uint32)),
            0x4e => self.emit(op_signed_type(OperationKind::Ge, SignedType::Int32)),
            0x4f => self.emit(op_signed_type(OperationKind::Ge, SignedType::Uint32)),
            0x50 => self.emit(op_unsigned_int(OperationKind::Eqz, UnsignedInt::I64)),
            0x51 => self.emit(op_unsigned(OperationKind::Eq, UnsignedType::I64)),
            0x52 => self.emit(op_unsigned(OperationKind::Ne, UnsignedType::I64)),
            0x53 => self.emit(op_signed_type(OperationKind::Lt, SignedType::Int64)),
            0x54 => self.emit(op_signed_type(OperationKind::Lt, SignedType::Uint64)),
            0x55 => self.emit(op_signed_type(OperationKind::Gt, SignedType::Int64)),
            0x56 => self.emit(op_signed_type(OperationKind::Gt, SignedType::Uint64)),
            0x57 => self.emit(op_signed_type(OperationKind::Le, SignedType::Int64)),
            0x58 => self.emit(op_signed_type(OperationKind::Le, SignedType::Uint64)),
            0x59 => self.emit(op_signed_type(OperationKind::Ge, SignedType::Int64)),
            0x5a => self.emit(op_signed_type(OperationKind::Ge, SignedType::Uint64)),
            0x5b => self.emit(op_unsigned(OperationKind::Eq, UnsignedType::F32)),
            0x5c => self.emit(op_unsigned(OperationKind::Ne, UnsignedType::F32)),
            0x5d => self.emit(op_signed_type(OperationKind::Lt, SignedType::Float32)),
            0x5e => self.emit(op_signed_type(OperationKind::Gt, SignedType::Float32)),
            0x5f => self.emit(op_signed_type(OperationKind::Le, SignedType::Float32)),
            0x60 => self.emit(op_signed_type(OperationKind::Ge, SignedType::Float32)),
            0x61 => self.emit(op_unsigned(OperationKind::Eq, UnsignedType::F64)),
            0x62 => self.emit(op_unsigned(OperationKind::Ne, UnsignedType::F64)),
            0x63 => self.emit(op_signed_type(OperationKind::Lt, SignedType::Float64)),
            0x64 => self.emit(op_signed_type(OperationKind::Gt, SignedType::Float64)),
            0x65 => self.emit(op_signed_type(OperationKind::Le, SignedType::Float64)),
            0x66 => self.emit(op_signed_type(OperationKind::Ge, SignedType::Float64)),
            0x67 => self.emit(op_unsigned_int(OperationKind::Clz, UnsignedInt::I32)),
            0x68 => self.emit(op_unsigned_int(OperationKind::Ctz, UnsignedInt::I32)),
            0x69 => self.emit(op_unsigned_int(OperationKind::Popcnt, UnsignedInt::I32)),
            0x6a => self.emit(op_unsigned(OperationKind::Add, UnsignedType::I32)),
            0x6b => self.emit(op_unsigned(OperationKind::Sub, UnsignedType::I32)),
            0x6c => self.emit(op_unsigned(OperationKind::Mul, UnsignedType::I32)),
            0x6d => self.emit(op_signed_type(OperationKind::Div, SignedType::Int32)),
            0x6e => self.emit(op_signed_type(OperationKind::Div, SignedType::Uint32)),
            0x6f => self.emit(op_signed_int(OperationKind::Rem, SignedInt::Int32)),
            0x70 => self.emit(op_signed_int(OperationKind::Rem, SignedInt::Uint32)),
            0x71 => self.emit(op_unsigned_int(OperationKind::And, UnsignedInt::I32)),
            0x72 => self.emit(op_unsigned_int(OperationKind::Or, UnsignedInt::I32)),
            0x73 => self.emit(op_unsigned_int(OperationKind::Xor, UnsignedInt::I32)),
            0x74 => self.emit(op_unsigned_int(OperationKind::Shl, UnsignedInt::I32)),
            0x75 => self.emit(op_signed_int(OperationKind::Shr, SignedInt::Int32)),
            0x76 => self.emit(op_signed_int(OperationKind::Shr, SignedInt::Uint32)),
            0x77 => self.emit(op_unsigned_int(OperationKind::Rotl, UnsignedInt::I32)),
            0x78 => self.emit(op_unsigned_int(OperationKind::Rotr, UnsignedInt::I32)),
            0x79 => self.emit(op_unsigned_int(OperationKind::Clz, UnsignedInt::I64)),
            0x7a => self.emit(op_unsigned_int(OperationKind::Ctz, UnsignedInt::I64)),
            0x7b => self.emit(op_unsigned_int(OperationKind::Popcnt, UnsignedInt::I64)),
            0x7c => self.emit(op_unsigned(OperationKind::Add, UnsignedType::I64)),
            0x7d => self.emit(op_unsigned(OperationKind::Sub, UnsignedType::I64)),
            0x7e => self.emit(op_unsigned(OperationKind::Mul, UnsignedType::I64)),
            0x7f => self.emit(op_signed_type(OperationKind::Div, SignedType::Int64)),
            0x80 => self.emit(op_signed_type(OperationKind::Div, SignedType::Uint64)),
            0x81 => self.emit(op_signed_int(OperationKind::Rem, SignedInt::Int64)),
            0x82 => self.emit(op_signed_int(OperationKind::Rem, SignedInt::Uint64)),
            0x83 => self.emit(op_unsigned_int(OperationKind::And, UnsignedInt::I64)),
            0x84 => self.emit(op_unsigned_int(OperationKind::Or, UnsignedInt::I64)),
            0x85 => self.emit(op_unsigned_int(OperationKind::Xor, UnsignedInt::I64)),
            0x86 => self.emit(op_unsigned_int(OperationKind::Shl, UnsignedInt::I64)),
            0x87 => self.emit(op_signed_int(OperationKind::Shr, SignedInt::Int64)),
            0x88 => self.emit(op_signed_int(OperationKind::Shr, SignedInt::Uint64)),
            0x89 => self.emit(op_unsigned_int(OperationKind::Rotl, UnsignedInt::I64)),
            0x8a => self.emit(op_unsigned_int(OperationKind::Rotr, UnsignedInt::I64)),
            0x8b => self.emit(op_float(OperationKind::Abs, FloatKind::F32)),
            0x8c => self.emit(op_float(OperationKind::Neg, FloatKind::F32)),
            0x8d => self.emit(op_float(OperationKind::Ceil, FloatKind::F32)),
            0x8e => self.emit(op_float(OperationKind::Floor, FloatKind::F32)),
            0x8f => self.emit(op_float(OperationKind::Trunc, FloatKind::F32)),
            0x90 => self.emit(op_float(OperationKind::Nearest, FloatKind::F32)),
            0x91 => self.emit(op_float(OperationKind::Sqrt, FloatKind::F32)),
            0x92 => self.emit(op_unsigned(OperationKind::Add, UnsignedType::F32)),
            0x93 => self.emit(op_unsigned(OperationKind::Sub, UnsignedType::F32)),
            0x94 => self.emit(op_unsigned(OperationKind::Mul, UnsignedType::F32)),
            0x95 => self.emit(op_signed_type(OperationKind::Div, SignedType::Float32)),
            0x96 => self.emit(op_float(OperationKind::Min, FloatKind::F32)),
            0x97 => self.emit(op_float(OperationKind::Max, FloatKind::F32)),
            0x98 => self.emit(op_float(OperationKind::Copysign, FloatKind::F32)),
            0x99 => self.emit(op_float(OperationKind::Abs, FloatKind::F64)),
            0x9a => self.emit(op_float(OperationKind::Neg, FloatKind::F64)),
            0x9b => self.emit(op_float(OperationKind::Ceil, FloatKind::F64)),
            0x9c => self.emit(op_float(OperationKind::Floor, FloatKind::F64)),
            0x9d => self.emit(op_float(OperationKind::Trunc, FloatKind::F64)),
            0x9e => self.emit(op_float(OperationKind::Nearest, FloatKind::F64)),
            0x9f => self.emit(op_float(OperationKind::Sqrt, FloatKind::F64)),
            0xa0 => self.emit(op_unsigned(OperationKind::Add, UnsignedType::F64)),
            0xa1 => self.emit(op_unsigned(OperationKind::Sub, UnsignedType::F64)),
            0xa2 => self.emit(op_unsigned(OperationKind::Mul, UnsignedType::F64)),
            0xa3 => self.emit(op_signed_type(OperationKind::Div, SignedType::Float64)),
            0xa4 => self.emit(op_float(OperationKind::Min, FloatKind::F64)),
            0xa5 => self.emit(op_float(OperationKind::Max, FloatKind::F64)),
            0xa6 => self.emit(op_float(OperationKind::Copysign, FloatKind::F64)),
            0xa7 => self.emit(Instruction::new(OperationKind::I32WrapFromI64)),
            0xa8 => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F32,
                SignedInt::Int32,
                false,
            )),
            0xa9 => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F32,
                SignedInt::Uint32,
                false,
            )),
            0xaa => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F64,
                SignedInt::Int32,
                false,
            )),
            0xab => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F64,
                SignedInt::Uint32,
                false,
            )),
            0xac => self.emit(Instruction::extend(true)),
            0xad => self.emit(Instruction::extend(false)),
            0xae => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F32,
                SignedInt::Int64,
                false,
            )),
            0xaf => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F32,
                SignedInt::Uint64,
                false,
            )),
            0xb0 => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F64,
                SignedInt::Int64,
                false,
            )),
            0xb1 => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F64,
                SignedInt::Uint64,
                false,
            )),
            0xb2 => self.emit(Instruction::f_convert_from_i(
                SignedInt::Int32,
                FloatKind::F32,
            )),
            0xb3 => self.emit(Instruction::f_convert_from_i(
                SignedInt::Uint32,
                FloatKind::F32,
            )),
            0xb4 => self.emit(Instruction::f_convert_from_i(
                SignedInt::Int64,
                FloatKind::F32,
            )),
            0xb5 => self.emit(Instruction::f_convert_from_i(
                SignedInt::Uint64,
                FloatKind::F32,
            )),
            0xb6 => self.emit(Instruction::new(OperationKind::F32DemoteFromF64)),
            0xb7 => self.emit(Instruction::f_convert_from_i(
                SignedInt::Int32,
                FloatKind::F64,
            )),
            0xb8 => self.emit(Instruction::f_convert_from_i(
                SignedInt::Uint32,
                FloatKind::F64,
            )),
            0xb9 => self.emit(Instruction::f_convert_from_i(
                SignedInt::Int64,
                FloatKind::F64,
            )),
            0xba => self.emit(Instruction::f_convert_from_i(
                SignedInt::Uint64,
                FloatKind::F64,
            )),
            0xbb => self.emit(Instruction::new(OperationKind::F64PromoteFromF32)),
            0xbc => self.emit(Instruction::new(OperationKind::I32ReinterpretFromF32)),
            0xbd => self.emit(Instruction::new(OperationKind::I64ReinterpretFromF64)),
            0xbe => self.emit(Instruction::new(OperationKind::F32ReinterpretFromI32)),
            0xbf => self.emit(Instruction::new(OperationKind::F64ReinterpretFromI64)),
            0xc0 => self.emit(Instruction::new(OperationKind::SignExtend32From8)),
            0xc1 => self.emit(Instruction::new(OperationKind::SignExtend32From16)),
            0xc2 => self.emit(Instruction::new(OperationKind::SignExtend64From8)),
            0xc3 => self.emit(Instruction::new(OperationKind::SignExtend64From16)),
            0xc4 => self.emit(Instruction::new(OperationKind::SignExtend64From32)),
            0xd0 => {
                self.pc += 1;
                self.emit(Instruction::const_i64(0));
            }
            0xd1 => self.emit(op_unsigned_int(OperationKind::Eqz, UnsignedInt::I64)),
            0xd2 => {
                let (fn_index, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(Instruction::new(OperationKind::RefFunc).with_u1(fn_index as u64));
            }
            OPCODE_MISC_PREFIX => self.handle_misc()?,
            0xfd | 0xfe | 0x12 | 0x13 => {
                return Err(CompileError::new(format!(
                    "unsupported instruction in interpreter compiler: 0x{opcode:x}"
                )));
            }
            _ => {
                return Err(CompileError::new(format!(
                    "unsupported instruction in interpreter compiler: 0x{opcode:x}"
                )));
            }
        }

        self.pc += 1;
        Ok(())
    }

    fn handle_misc(&mut self) -> Result<(), CompileError> {
        self.pc += 1;
        let (misc_op, consumed) = decode_u32(&self.config.body[self.pc..])?;
        self.pc += consumed - 1;
        match misc_op as u8 {
            0x00 => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F32,
                SignedInt::Int32,
                true,
            )),
            0x01 => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F32,
                SignedInt::Uint32,
                true,
            )),
            0x02 => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F64,
                SignedInt::Int32,
                true,
            )),
            0x03 => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F64,
                SignedInt::Uint32,
                true,
            )),
            0x04 => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F32,
                SignedInt::Int64,
                true,
            )),
            0x05 => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F32,
                SignedInt::Uint64,
                true,
            )),
            0x06 => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F64,
                SignedInt::Int64,
                true,
            )),
            0x07 => self.emit(Instruction::i_trunc_from_f(
                FloatKind::F64,
                SignedInt::Uint64,
                true,
            )),
            0x08 => {
                self.result.uses_memory = true;
                let (data_index, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed + 1;
                self.emit(Instruction::new(OperationKind::MemoryInit).with_u1(data_index as u64));
            }
            0x09 => {
                let (data_index, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(Instruction::new(OperationKind::DataDrop).with_u1(data_index as u64));
            }
            0x0a => {
                self.result.uses_memory = true;
                self.pc += 2;
                self.emit(Instruction::new(OperationKind::MemoryCopy));
            }
            0x0b => {
                self.result.uses_memory = true;
                self.pc += 1;
                self.emit(Instruction::new(OperationKind::MemoryFill));
            }
            0x0c => {
                let (elem_index, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                let (table_index, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(
                    Instruction::new(OperationKind::TableInit)
                        .with_u1(elem_index as u64)
                        .with_u2(table_index as u64),
                );
            }
            0x0d => {
                let (elem_index, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(Instruction::new(OperationKind::ElemDrop).with_u1(elem_index as u64));
            }
            0x0e => {
                let (dst, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                let (src, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(
                    Instruction::new(OperationKind::TableCopy)
                        .with_u1(src as u64)
                        .with_u2(dst as u64),
                );
            }
            0x0f => {
                let (table_index, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(Instruction::new(OperationKind::TableGrow).with_u1(table_index as u64));
            }
            0x10 => {
                let (table_index, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(Instruction::new(OperationKind::TableSize).with_u1(table_index as u64));
            }
            0x11 => {
                let (table_index, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                self.emit(Instruction::new(OperationKind::TableFill).with_u1(table_index as u64));
            }
            _ => {
                return Err(CompileError::new(format!(
                    "unsupported misc instruction in interpreter compiler: 0x{misc_op:x}"
                )));
            }
        }
        Ok(())
    }

    fn apply_to_stack(&mut self, opcode: u8) -> Result<u32, CompileError> {
        let mut index = 0;
        match opcode {
            0x10 | 0x11 | 0x20 | 0x21 | 0x22 | 0x23 | 0x24 | 0x12 | 0x13 => {
                let (value, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
                self.pc += consumed;
                index = value;
            }
            _ => {}
        }

        if self.unreachable_on {
            return Ok(index);
        }

        let sig = self.opcode_signature(opcode, index)?;
        let mut inferred = None;
        for want in sig.input.iter().rev().copied() {
            let actual = self.stack_pop()?;
            let expected = match want {
                SigType::Known(ty) => ty,
                SigType::Unknown => *inferred.get_or_insert(actual),
            };
            if actual != expected {
                return Err(CompileError::new(format!(
                    "input signature mismatch: expected {:?} but found {:?}",
                    expected, actual
                )));
            }
        }
        for ty in sig.output {
            let ty = match ty {
                SigType::Known(ty) => ty,
                SigType::Unknown => {
                    inferred.ok_or_else(|| CompileError::new("cannot infer result type"))?
                }
            };
            self.stack_push(ty);
        }
        Ok(index)
    }

    fn opcode_signature(&self, opcode: u8, index: u32) -> Result<Signature, CompileError> {
        match opcode {
            0x00 | 0x01 | 0x02 | 0x03 | 0x05 | 0x0b | 0x0c | 0x0f => Ok(sig([], [])),
            0x04 | 0x0d | 0x0e => Ok(sig([k(UnsignedType::I32)], [])),
            0x10 => self.direct_call_signature(index),
            0x11 => self.indirect_call_signature(index),
            0x1a => Ok(sig([u()], [])),
            0x1b | 0x1c => Ok(sig([u(), u(), k(UnsignedType::I32)], [u()])),
            0x20 => Ok(sig(
                [],
                [k(self.local_type(index as usize)?.as_stack_type())],
            )),
            0x21 => Ok(sig(
                [k(self.local_type(index as usize)?.as_stack_type())],
                [],
            )),
            0x22 => {
                let ty = self.local_type(index as usize)?.as_stack_type();
                Ok(sig([k(ty)], [k(ty)]))
            }
            0x23 => Ok(sig(
                [],
                [k(self.global_type(index as usize)?.as_stack_type())],
            )),
            0x24 => Ok(sig(
                [k(self.global_type(index as usize)?.as_stack_type())],
                [],
            )),
            0x25 => Ok(sig([k(UnsignedType::I32)], [k(UnsignedType::I64)])),
            0x26 => Ok(sig([k(UnsignedType::I32), k(UnsignedType::I64)], [])),
            0x28 | 0x2c | 0x2d | 0x2e | 0x2f | 0x45 => {
                Ok(sig([k(UnsignedType::I32)], [k(UnsignedType::I32)]))
            }
            0x29 | 0x30 | 0x31 | 0x32 | 0x33 | 0x34 | 0x35 | 0x50 => {
                Ok(sig([k(UnsignedType::I32)], [k(UnsignedType::I64)]))
            }
            0x2a => Ok(sig([k(UnsignedType::I32)], [k(UnsignedType::F32)])),
            0x2b => Ok(sig([k(UnsignedType::I32)], [k(UnsignedType::F64)])),
            0x36 | 0x3a | 0x3b => Ok(sig([k(UnsignedType::I32), k(UnsignedType::I32)], [])),
            0x37 | 0x3c | 0x3d | 0x3e => Ok(sig([k(UnsignedType::I32), k(UnsignedType::I64)], [])),
            0x38 => Ok(sig([k(UnsignedType::I32), k(UnsignedType::F32)], [])),
            0x39 => Ok(sig([k(UnsignedType::I32), k(UnsignedType::F64)], [])),
            0x3f | 0x41 => Ok(sig([], [k(UnsignedType::I32)])),
            0x40 => Ok(sig([k(UnsignedType::I32)], [k(UnsignedType::I32)])),
            0x42 => Ok(sig([], [k(UnsignedType::I64)])),
            0x43 => Ok(sig([], [k(UnsignedType::F32)])),
            0x44 => Ok(sig([], [k(UnsignedType::F64)])),
            0x46..=0x4f | 0x51..=0x5a => {
                let in_ty = if opcode <= 0x4f {
                    UnsignedType::I32
                } else {
                    UnsignedType::I64
                };
                Ok(sig([k(in_ty), k(in_ty)], [k(UnsignedType::I32)]))
            }
            0x5b..=0x60 => Ok(sig(
                [k(UnsignedType::F32), k(UnsignedType::F32)],
                [k(UnsignedType::I32)],
            )),
            0x61..=0x66 => Ok(sig(
                [k(UnsignedType::F64), k(UnsignedType::F64)],
                [k(UnsignedType::I32)],
            )),
            0x67..=0x69 | 0xc0 | 0xc1 => Ok(sig([k(UnsignedType::I32)], [k(UnsignedType::I32)])),
            0x6a..=0x78 => Ok(sig(
                [k(UnsignedType::I32), k(UnsignedType::I32)],
                [k(UnsignedType::I32)],
            )),
            0x79..=0x7b | 0xc2 | 0xc3 | 0xc4 => {
                Ok(sig([k(UnsignedType::I64)], [k(UnsignedType::I64)]))
            }
            0x7c..=0x8a => Ok(sig(
                [k(UnsignedType::I64), k(UnsignedType::I64)],
                [k(UnsignedType::I64)],
            )),
            0x8b..=0x91 => Ok(sig([k(UnsignedType::F32)], [k(UnsignedType::F32)])),
            0x92..=0x98 => Ok(sig(
                [k(UnsignedType::F32), k(UnsignedType::F32)],
                [k(UnsignedType::F32)],
            )),
            0x99..=0x9f => Ok(sig([k(UnsignedType::F64)], [k(UnsignedType::F64)])),
            0xa0..=0xa6 => Ok(sig(
                [k(UnsignedType::F64), k(UnsignedType::F64)],
                [k(UnsignedType::F64)],
            )),
            0xa7 | 0xd1 => Ok(sig([k(UnsignedType::I64)], [k(UnsignedType::I32)])),
            0xa8 | 0xa9 => Ok(sig([k(UnsignedType::F32)], [k(UnsignedType::I32)])),
            0xaa | 0xab => Ok(sig([k(UnsignedType::F64)], [k(UnsignedType::I32)])),
            0xac | 0xad => Ok(sig([k(UnsignedType::I32)], [k(UnsignedType::I64)])),
            0xae | 0xaf => Ok(sig([k(UnsignedType::F32)], [k(UnsignedType::I64)])),
            0xb0 | 0xb1 => Ok(sig([k(UnsignedType::F64)], [k(UnsignedType::I64)])),
            0xb2 | 0xb3 => Ok(sig([k(UnsignedType::I32)], [k(UnsignedType::F32)])),
            0xb4 | 0xb5 => Ok(sig([k(UnsignedType::I64)], [k(UnsignedType::F32)])),
            0xb6 => Ok(sig([k(UnsignedType::F64)], [k(UnsignedType::F32)])),
            0xb7 | 0xb8 => Ok(sig([k(UnsignedType::I32)], [k(UnsignedType::F64)])),
            0xb9 | 0xba => Ok(sig([k(UnsignedType::I64)], [k(UnsignedType::F64)])),
            0xbb => Ok(sig([k(UnsignedType::F32)], [k(UnsignedType::F64)])),
            0xbc => Ok(sig([k(UnsignedType::F32)], [k(UnsignedType::I32)])),
            0xbd => Ok(sig([k(UnsignedType::F64)], [k(UnsignedType::I64)])),
            0xbe => Ok(sig([k(UnsignedType::I32)], [k(UnsignedType::F32)])),
            0xbf => Ok(sig([k(UnsignedType::I64)], [k(UnsignedType::F64)])),
            0xd0 | 0xd2 => Ok(sig([], [k(UnsignedType::I64)])),
            OPCODE_MISC_PREFIX => self.misc_signature(),
            0xfd | 0xfe | 0x12 | 0x13 => Err(CompileError::new(format!(
                "unsupported instruction in interpreter compiler: 0x{opcode:x}"
            ))),
            _ => Err(CompileError::new(format!(
                "unsupported instruction in interpreter compiler: 0x{opcode:x}"
            ))),
        }
    }

    fn misc_signature(&self) -> Result<Signature, CompileError> {
        let misc = *self
            .config
            .body
            .get(self.pc + 1)
            .ok_or_else(|| CompileError::new("unexpected eof reading misc opcode"))?;
        match misc {
            0x00 | 0x01 => Ok(sig([k(UnsignedType::F32)], [k(UnsignedType::I32)])),
            0x02 | 0x03 => Ok(sig([k(UnsignedType::F64)], [k(UnsignedType::I32)])),
            0x04 | 0x05 => Ok(sig([k(UnsignedType::F32)], [k(UnsignedType::I64)])),
            0x06 | 0x07 => Ok(sig([k(UnsignedType::F64)], [k(UnsignedType::I64)])),
            0x08 | 0x0a | 0x0b | 0x0c | 0x0e => Ok(sig(
                [
                    k(UnsignedType::I32),
                    k(UnsignedType::I32),
                    k(UnsignedType::I32),
                ],
                [],
            )),
            0x09 | 0x0d => Ok(sig([], [])),
            0x0f => Ok(sig(
                [k(UnsignedType::I64), k(UnsignedType::I32)],
                [k(UnsignedType::I32)],
            )),
            0x10 => Ok(sig([], [k(UnsignedType::I32)])),
            0x11 => Ok(sig(
                [
                    k(UnsignedType::I32),
                    k(UnsignedType::I64),
                    k(UnsignedType::I32),
                ],
                [],
            )),
            _ => Err(CompileError::new(format!(
                "unsupported misc instruction in interpreter compiler: 0x{misc:x}"
            ))),
        }
    }

    fn direct_call_signature(&self, function_index: u32) -> Result<Signature, CompileError> {
        let type_index = *self
            .config
            .functions
            .get(function_index as usize)
            .ok_or_else(|| CompileError::new(format!("invalid function index {function_index}")))?;
        self.function_type_signature(type_index, false)
    }

    fn indirect_call_signature(&self, type_index: u32) -> Result<Signature, CompileError> {
        self.function_type_signature(type_index, true)
    }

    fn function_type_signature(
        &self,
        type_index: u32,
        indirect: bool,
    ) -> Result<Signature, CompileError> {
        let ty = self
            .config
            .types
            .get(type_index as usize)
            .ok_or_else(|| CompileError::new(format!("invalid type index {type_index}")))?;
        let mut input = ty
            .params
            .iter()
            .map(|ty| SigType::Known(ty.as_stack_type()))
            .collect::<Vec<_>>();
        if indirect {
            input.push(SigType::Known(UnsignedType::I32));
        }
        let output = ty
            .results
            .iter()
            .map(|ty| SigType::Known(ty.as_stack_type()))
            .collect::<Vec<_>>();
        Ok(Signature { input, output })
    }

    fn decode_block_type(&mut self) -> Result<FunctionType, CompileError> {
        let next = self.pc + 1;
        let byte = self.byte_at(next)?;
        if byte == 0x40 {
            self.pc += 1;
            return Ok(FunctionType::default());
        }
        if let Some(value_type) = ValueType::from_block_byte(byte) {
            self.pc += 1;
            return Ok(FunctionType::new(Vec::new(), vec![value_type]));
        }
        let (type_index, consumed) = decode_i33_as_i64(&self.config.body[next..])?;
        self.pc += consumed;
        let ty =
            self.config.types.get(type_index as usize).ok_or_else(|| {
                CompileError::new(format!("invalid block type index {type_index}"))
            })?;
        Ok(ty.clone())
    }

    fn emit_default_value(&mut self, ty: ValueType) {
        self.stack_push(ty.as_stack_type());
        match ty {
            ValueType::I32 => self.emit(Instruction::const_i32(0)),
            ValueType::I64 | ValueType::FuncRef | ValueType::ExternRef => {
                self.emit(Instruction::const_i64(0))
            }
            ValueType::F32 => self.emit(Instruction::const_f32(0.0)),
            ValueType::F64 => self.emit(Instruction::const_f64(0.0)),
            ValueType::V128 => self.emit(Instruction::v128_const(0, 0)),
        }
    }

    fn read_memory_and<F>(&mut self, f: F) -> Result<Instruction, CompileError>
    where
        F: FnOnce(MemoryArg) -> Instruction,
    {
        Ok(f(self.read_memory_arg()?))
    }

    fn read_memory_arg(&mut self) -> Result<MemoryArg, CompileError> {
        self.result.uses_memory = true;
        let (alignment, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
        self.pc += consumed;
        let (offset, consumed) = decode_u32(&self.config.body[self.pc + 1..])?;
        self.pc += consumed;
        Ok(MemoryArg { alignment, offset })
    }

    fn get_frame_drop_range(&self, frame: &ControlFrame, is_end: bool) -> InclusiveRange {
        let start = if !is_end && frame.kind == ControlFrameKind::Loop {
            frame.block_type.param_num_in_u64
        } else {
            frame.block_type.result_num_in_u64
        };
        let end = self.stack_len_in_u64 as isize
            - 1
            - frame.original_stack_len_without_param_u64 as isize;
        if start as isize <= end {
            InclusiveRange::new(start as i32, end as i32)
        } else {
            InclusiveRange::NOP
        }
    }

    fn local_type(&self, index: usize) -> Result<ValueType, CompileError> {
        if index < self.config.signature.params.len() {
            Ok(self.config.signature.params[index])
        } else {
            self.config
                .local_types
                .get(index - self.config.signature.params.len())
                .copied()
                .ok_or_else(|| CompileError::new(format!("invalid local index {index}")))
        }
    }

    fn global_type(&self, index: usize) -> Result<ValueType, CompileError> {
        self.config
            .globals
            .get(index)
            .map(|g| g.value_type)
            .ok_or_else(|| CompileError::new(format!("invalid global index {index}")))
    }

    fn local_depth(&self, index: usize) -> Result<usize, CompileError> {
        let height = *self
            .local_index_to_stack_height_in_u64
            .get(index)
            .ok_or_else(|| CompileError::new(format!("invalid local index {index}")))?;
        Ok(self.stack_len_in_u64 - 1 - height)
    }

    fn function_frame(&self) -> Result<&ControlFrame, CompileError> {
        self.control_frames
            .first()
            .ok_or_else(|| CompileError::new("missing function frame"))
    }

    fn frame_at_depth_mut(&mut self, depth: usize) -> Result<&mut ControlFrame, CompileError> {
        let len = self.control_frames.len();
        self.control_frames
            .get_mut(len.saturating_sub(depth + 1))
            .ok_or_else(|| CompileError::new(format!("invalid branch depth {depth}")))
    }

    fn top_frame_mut(&mut self) -> Result<&mut ControlFrame, CompileError> {
        self.control_frames
            .last_mut()
            .ok_or_else(|| CompileError::new("missing top frame"))
    }

    fn emit(&mut self, op: Instruction) {
        if self.unreachable_on {
            return;
        }
        if op.kind == OperationKind::Drop && InclusiveRange::from_u64(op.u1) == InclusiveRange::NOP
        {
            return;
        }
        self.result.operations.push(op);
    }

    fn stack_push(&mut self, ty: UnsignedType) {
        self.stack.push(ty);
        self.stack_len_in_u64 += slots_for_stack_type(ty);
    }

    fn stack_pop(&mut self) -> Result<UnsignedType, CompileError> {
        let ty = self
            .stack
            .pop()
            .ok_or_else(|| CompileError::new("stack underflow"))?;
        self.stack_len_in_u64 -= slots_for_stack_type(ty);
        Ok(ty)
    }

    fn stack_peek(&self) -> Result<UnsignedType, CompileError> {
        self.stack
            .last()
            .copied()
            .ok_or_else(|| CompileError::new("empty stack"))
    }

    fn stack_switch_at(&mut self, frame: &ControlFrame) {
        self.stack.truncate(frame.original_stack_len_without_param);
        self.stack_len_in_u64 = frame.original_stack_len_without_param_u64;
    }

    fn next_frame_id(&mut self) -> u32 {
        self.current_frame_id += 1;
        self.current_frame_id
    }

    fn mark_unreachable(&mut self) {
        self.unreachable_on = true;
    }

    fn reset_unreachable(&mut self) {
        self.unreachable_on = false;
    }

    fn bump_label(&mut self, label: Label) {
        self.bump_label_by(label, 1);
    }

    fn bump_label_by(&mut self, label: Label, by: u32) {
        *self.result.label_callers.entry(label).or_insert(0) += by;
    }

    fn byte_at(&self, index: usize) -> Result<u8, CompileError> {
        self.config
            .body
            .get(index)
            .copied()
            .ok_or_else(|| CompileError::new("unexpected eof"))
    }
}

fn slots_for_stack_type(ty: UnsignedType) -> usize {
    usize::from(matches!(ty, UnsignedType::V128)) + 1
}

fn op_unsigned(kind: OperationKind, ty: UnsignedType) -> Instruction {
    Instruction::new(kind).with_b1(ty as u8)
}

fn op_unsigned_int(kind: OperationKind, ty: UnsignedInt) -> Instruction {
    Instruction::new(kind).with_b1(ty as u8)
}

fn op_signed_int(kind: OperationKind, ty: SignedInt) -> Instruction {
    Instruction::new(kind).with_b1(ty as u8)
}

fn op_signed_type(kind: OperationKind, ty: SignedType) -> Instruction {
    Instruction::new(kind).with_b1(ty as u8)
}

fn op_float(kind: OperationKind, ty: FloatKind) -> Instruction {
    Instruction::new(kind).with_b1(ty as u8)
}

fn k(ty: UnsignedType) -> SigType {
    SigType::Known(ty)
}

fn u() -> SigType {
    SigType::Unknown
}

fn sig<const I: usize, const O: usize>(input: [SigType; I], output: [SigType; O]) -> Signature {
    Signature {
        input: input.to_vec(),
        output: output.to_vec(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leb128Error {
    UnexpectedEof,
    Overflow32,
    Overflow33,
    Overflow64,
}

impl fmt::Display for Leb128Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => f.write_str("unexpected eof"),
            Self::Overflow32 => f.write_str("u32 overflow"),
            Self::Overflow33 => f.write_str("i33 overflow"),
            Self::Overflow64 => f.write_str("i64 overflow"),
        }
    }
}

fn decode_u32(bytes: &[u8]) -> Result<(u32, usize), Leb128Error> {
    let mut ret = 0u32;
    let mut shift = 0u32;
    for i in 0..5 {
        let byte = *bytes.get(i).ok_or(Leb128Error::UnexpectedEof)?;
        if byte < 0x80 {
            if i == 4 && (byte & 0xf0) != 0 {
                return Err(Leb128Error::Overflow32);
            }
            return Ok((ret | ((byte as u32) << shift), i + 1));
        }
        ret |= ((byte & 0x7f) as u32) << shift;
        shift += 7;
    }
    Err(Leb128Error::Overflow32)
}

fn decode_i32(bytes: &[u8]) -> Result<(i32, usize), Leb128Error> {
    let mut ret = 0u32;
    let mut shift = 0u32;
    let mut bytes_read = 0usize;
    loop {
        let byte = *bytes.get(bytes_read).ok_or(Leb128Error::UnexpectedEof)?;
        ret |= ((byte & 0x7f) as u32) << shift;
        shift += 7;
        bytes_read += 1;
        if byte & 0x80 == 0 {
            if shift < 32 && byte & 0x40 != 0 {
                ret |= (!0u32) << shift;
            }
            let signed = ret as i32;
            if bytes_read > 5 {
                return Err(Leb128Error::Overflow32);
            }
            return Ok((signed, bytes_read));
        }
    }
}

fn decode_i64(bytes: &[u8]) -> Result<(i64, usize), Leb128Error> {
    let mut ret = 0u64;
    let mut shift = 0u32;
    let mut bytes_read = 0usize;
    loop {
        let byte = *bytes.get(bytes_read).ok_or(Leb128Error::UnexpectedEof)?;
        ret |= ((byte & 0x7f) as u64) << shift;
        shift += 7;
        bytes_read += 1;
        if byte & 0x80 == 0 {
            if shift < 64 && byte & 0x40 != 0 {
                ret |= (!0u64) << shift;
            }
            if bytes_read > 10 {
                return Err(Leb128Error::Overflow64);
            }
            return Ok((ret as i64, bytes_read));
        }
    }
}

fn decode_i33_as_i64(bytes: &[u8]) -> Result<(i64, usize), Leb128Error> {
    let mut ret = 0i64;
    let mut shift = 0u32;
    let mut bytes_read = 0usize;
    let mut last = 0i64;
    while shift < 35 {
        let byte = *bytes.get(bytes_read).ok_or(Leb128Error::UnexpectedEof)?;
        last = i64::from(byte);
        ret |= (last & !0x80) << shift;
        shift += 7;
        bytes_read += 1;
        if last & 0x80 == 0 {
            break;
        }
    }
    if shift < 33 && (last & 0x40) != 0 {
        ret |= 8_589_934_591i64 << shift;
    }
    ret &= 8_589_934_591;
    if ret & (1 << 32) != 0 {
        ret -= 8_589_934_592;
    }
    if bytes_read > 5 {
        return Err(Leb128Error::Overflow33);
    }
    Ok((ret, bytes_read))
}

fn read_le_u32(bytes: &[u8], start: usize) -> Result<u32, CompileError> {
    let slice = bytes
        .get(start..start + 4)
        .ok_or_else(|| CompileError::new("unexpected eof"))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_le_u64(bytes: &[u8], start: usize) -> Result<u64, CompileError> {
    let slice = bytes
        .get(start..start + 8)
        .ok_or_else(|| CompileError::new("unexpected eof"))?;
    Ok(u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(kind: LabelKind, frame_id: u32) -> Label {
        Label::new(kind, frame_id)
    }

    fn i32_i32() -> FunctionType {
        FunctionType::new(vec![ValueType::I32], vec![ValueType::I32])
    }

    #[test]
    fn lowers_nullary_function() {
        let result = Compiler
            .lower_with_config(CompileConfig::new(&[0x0b]))
            .unwrap();
        assert_eq!(
            vec![Instruction::br(label(LabelKind::Return, 0))],
            result.operations
        );
    }

    #[test]
    fn lowers_local_get_identity() {
        let result = Compiler
            .lower_with_config(CompileConfig {
                body: &[0x20, 0x00, 0x0b],
                signature: i32_i32(),
                ..CompileConfig::new(&[])
            })
            .unwrap();
        assert_eq!(
            vec![
                Instruction::pick(0, false),
                Instruction::drop(InclusiveRange::new(1, 1)),
                Instruction::br(label(LabelKind::Return, 0)),
            ],
            result.operations
        );
    }

    #[test]
    fn lowers_block_branch_unreachable_sequence() {
        let result = Compiler
            .lower_with_config(CompileConfig {
                body: &[0x02, 0x40, 0x0c, 0x00, 0x6a, 0x1a, 0x0b, 0x0b],
                ..CompileConfig::new(&[])
            })
            .unwrap();
        let continuation = label(LabelKind::Continuation, 2);
        assert_eq!(
            vec![
                Instruction::br(continuation),
                Instruction::label(continuation),
                Instruction::br(label(LabelKind::Return, 0)),
            ],
            result.operations
        );
        assert_eq!(Some(&1), result.label_callers.get(&continuation));
    }

    #[test]
    fn lowers_if_else_with_result() {
        let result = Compiler
            .lower_with_config(CompileConfig {
                body: &[
                    0x41, 0x01, 0x20, 0x00, 0x04, 0x00, 0x41, 0x02, 0x6a, 0x05, 0x41, 0x7e, 0x6b,
                    0x0b, 0x0b,
                ],
                signature: i32_i32(),
                types: &[i32_i32()],
                ..CompileConfig::new(&[])
            })
            .unwrap();
        let header = label(LabelKind::Header, 2);
        let else_label = label(LabelKind::Else, 2);
        let cont = label(LabelKind::Continuation, 2);
        assert_eq!(
            vec![
                Instruction::const_i32(1),
                Instruction::pick(1, false),
                Instruction::br_if(header, else_label, InclusiveRange::NOP),
                Instruction::label(header),
                Instruction::const_i32(2),
                op_unsigned(OperationKind::Add, UnsignedType::I32),
                Instruction::br(cont),
                Instruction::label(else_label),
                Instruction::const_i32((-2i32) as u32),
                op_unsigned(OperationKind::Sub, UnsignedType::I32),
                Instruction::br(cont),
                Instruction::label(cont),
                Instruction::drop(InclusiveRange::new(1, 1)),
                Instruction::br(label(LabelKind::Return, 0)),
            ],
            result.operations
        );
    }

    #[test]
    fn lowers_bulk_memory_ops() {
        let body = [
            0x41, 0x10, 0x41, 0x00, 0x41, 0x07, 0xfc, 0x08, 0x01, 0x00, 0xfc, 0x09, 0x01, 0x0b,
        ];
        let result = Compiler
            .lower_with_config(CompileConfig {
                body: &body,
                ..CompileConfig::new(&[])
            })
            .unwrap();
        assert_eq!(
            vec![
                Instruction::const_i32(16),
                Instruction::const_i32(0),
                Instruction::const_i32(7),
                Instruction::new(OperationKind::MemoryInit).with_u1(1),
                Instruction::new(OperationKind::DataDrop).with_u1(1),
                Instruction::br(label(LabelKind::Return, 0)),
            ],
            result.operations
        );
        assert!(result.uses_memory);
    }

    #[test]
    fn lowers_v128_local_access_depth() {
        let result = Compiler
            .lower_with_config(CompileConfig {
                body: &[0x20, 0x00, 0x1a, 0x0b],
                local_types: &[ValueType::V128],
                ..CompileConfig::new(&[])
            })
            .unwrap();
        assert_eq!(
            vec![
                Instruction::v128_const(0, 0),
                Instruction::pick(1, true),
                Instruction::drop(InclusiveRange::new(0, 1)),
                Instruction::drop(InclusiveRange::new(0, 1)),
                Instruction::br(label(LabelKind::Return, 0)),
            ],
            result.operations
        );
    }

    #[test]
    fn lowers_multivalue_blocktype_by_type_index() {
        let pair = FunctionType::new(vec![], vec![ValueType::F64, ValueType::F64]);
        let body = [
            0x02, 0x00, 0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x40, 0x44, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x14, 0x40, 0x0c, 0x00, 0xa0, 0x44, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x18, 0x40, 0x0b, 0x0b,
        ];
        let result = Compiler
            .lower_with_config(CompileConfig {
                body: &body,
                signature: pair.clone(),
                types: &[pair.clone()],
                ..CompileConfig::new(&[])
            })
            .unwrap();
        let cont = label(LabelKind::Continuation, 2);
        assert_eq!(
            vec![
                Instruction::const_f64(4.0),
                Instruction::const_f64(5.0),
                Instruction::br(cont),
                Instruction::label(cont),
                Instruction::br(label(LabelKind::Return, 0)),
            ],
            result.operations
        );
        assert_eq!(Some(&1), result.label_callers.get(&cont));
    }
}
