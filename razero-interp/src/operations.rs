#![doc = "Interpreter operation model ported from Go."]

use std::fmt;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Operation {
    #[default]
    Nop,
}

pub type UnionOperation = Instruction;

macro_rules! define_display_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident : $repr:ty {
            $($variant:ident => $display:literal,)*
        }
    ) => {
        $(#[$meta])*
        #[repr($repr)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name {
            $($variant,)*
        }

        impl $name {
            $vis const ALL: &'static [Self] = &[
                $(Self::$variant,)*
            ];

            $vis const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $display,)*
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

define_display_enum! {
    pub enum UnsignedInt : u8 {
        I32 => "i32",
        I64 => "i64",
    }
}

define_display_enum! {
    pub enum SignedInt : u8 {
        Int32 => "s32",
        Int64 => "s64",
        Uint32 => "u32",
        Uint64 => "u64",
    }
}

define_display_enum! {
    pub enum FloatKind : u8 {
        F32 => "f32",
        F64 => "f64",
    }
}

define_display_enum! {
    pub enum UnsignedType : u8 {
        I32 => "i32",
        I64 => "i64",
        F32 => "f32",
        F64 => "f64",
        V128 => "v128",
        Unknown => "unknown",
    }
}

define_display_enum! {
    pub enum SignedType : u8 {
        Int32 => "s32",
        Uint32 => "u32",
        Int64 => "s64",
        Uint64 => "u64",
        Float32 => "f32",
        Float64 => "f64",
    }
}

define_display_enum! {
    pub enum OperationKind : u16 {
        Unreachable => "Unreachable",
        Label => "label",
        Br => "Br",
        BrIf => "BrIf",
        BrTable => "BrTable",
        Call => "Call",
        CallIndirect => "CallIndirect",
        Drop => "Drop",
        Select => "Select",
        Pick => "Pick",
        Set => "Swap",
        GlobalGet => "GlobalGet",
        GlobalSet => "GlobalSet",
        Load => "Load",
        Load8 => "Load8",
        Load16 => "Load16",
        Load32 => "Load32",
        Store => "Store",
        Store8 => "Store8",
        Store16 => "Store16",
        Store32 => "Store32",
        MemorySize => "MemorySize",
        MemoryGrow => "MemoryGrow",
        ConstI32 => "ConstI32",
        ConstI64 => "ConstI64",
        ConstF32 => "ConstF32",
        ConstF64 => "ConstF64",
        Eq => "Eq",
        Ne => "Ne",
        Eqz => "Eqz",
        Lt => "Lt",
        Gt => "Gt",
        Le => "Le",
        Ge => "Ge",
        Add => "Add",
        Sub => "Sub",
        Mul => "Mul",
        Clz => "Clz",
        Ctz => "Ctz",
        Popcnt => "Popcnt",
        Div => "Div",
        Rem => "Rem",
        And => "And",
        Or => "Or",
        Xor => "Xor",
        Shl => "Shl",
        Shr => "Shr",
        Rotl => "Rotl",
        Rotr => "Rotr",
        Abs => "Abs",
        Neg => "Neg",
        Ceil => "Ceil",
        Floor => "Floor",
        Trunc => "Trunc",
        Nearest => "Nearest",
        Sqrt => "Sqrt",
        Min => "Min",
        Max => "Max",
        Copysign => "Copysign",
        I32WrapFromI64 => "I32WrapFromI64",
        ITruncFromF => "ITruncFromF",
        FConvertFromI => "FConvertFromI",
        F32DemoteFromF64 => "F32DemoteFromF64",
        F64PromoteFromF32 => "F64PromoteFromF32",
        I32ReinterpretFromF32 => "I32ReinterpretFromF32",
        I64ReinterpretFromF64 => "I64ReinterpretFromF64",
        F32ReinterpretFromI32 => "F32ReinterpretFromI32",
        F64ReinterpretFromI64 => "F64ReinterpretFromI64",
        Extend => "Extend",
        MemoryInit => "MemoryInit",
        DataDrop => "DataDrop",
        MemoryCopy => "MemoryCopy",
        MemoryFill => "MemoryFill",
        TableInit => "TableInit",
        ElemDrop => "ElemDrop",
        TableCopy => "TableCopy",
        RefFunc => "RefFunc",
        TableGet => "TableGet",
        TableSet => "TableSet",
        TableSize => "TableSize",
        TableGrow => "TableGrow",
        TableFill => "TableFill",
        V128Const => "ConstV128",
        V128Add => "V128Add",
        V128Sub => "V128Sub",
        V128Load => "V128Load",
        V128LoadLane => "V128LoadLane",
        V128Store => "V128Store",
        V128StoreLane => "V128StoreLane",
        V128ExtractLane => "V128ExtractLane",
        V128ReplaceLane => "V128ReplaceLane",
        V128Splat => "V128Splat",
        V128Shuffle => "V128Shuffle",
        V128Swizzle => "V128Swizzle",
        V128AnyTrue => "V128AnyTrue",
        V128AllTrue => "V128AllTrue",
        V128And => "V128And",
        V128Not => "V128Not",
        V128Or => "V128Or",
        V128Xor => "V128Xor",
        V128Bitselect => "V128Bitselect",
        V128AndNot => "V128AndNot",
        V128BitMask => "V128BitMask",
        V128Shl => "V128Shl",
        V128Shr => "V128Shr",
        V128Cmp => "V128Cmp",
        SignExtend32From8 => "SignExtend32From8",
        SignExtend32From16 => "SignExtend32From16",
        SignExtend64From8 => "SignExtend64From8",
        SignExtend64From16 => "SignExtend64From16",
        SignExtend64From32 => "SignExtend64From32",
        V128AddSat => "V128AddSat",
        V128SubSat => "V128SubSat",
        V128Mul => "V128Mul",
        V128Div => "V128Div",
        V128Neg => "V128Neg",
        V128Sqrt => "V128Sqrt",
        V128Abs => "V128Abs",
        V128Popcnt => "V128Popcnt",
        V128Min => "V128Min",
        V128Max => "V128Max",
        V128AvgrU => "V128AvgrU",
        V128Ceil => "V128Ceil",
        V128Floor => "V128Floor",
        V128Trunc => "V128Trunc",
        V128Nearest => "V128Nearest",
        V128Pmin => "V128Pmin",
        V128Pmax => "V128Pmax",
        V128Extend => "V128Extend",
        V128ExtMul => "V128ExtMul",
        V128Q15mulrSatS => "V128Q15mulrSatS",
        V128ExtAddPairwise => "V128ExtAddPairwise",
        V128FloatPromote => "V128FloatPromote",
        V128FloatDemote => "V128FloatDemote",
        V128FConvertFromI => "V128FConvertFromI",
        V128Dot => "V128Dot",
        V128Narrow => "V128Narrow",
        V128ITruncSatFromF => "V128ITruncSatFromF",
        BuiltinFunctionCheckExitCode => "BuiltinFunctionCheckExitCode",
        AtomicMemoryWait => "operationKindAtomicMemoryWait",
        AtomicMemoryNotify => "operationKindAtomicMemoryNotify",
        AtomicFence => "operationKindAtomicFence",
        AtomicLoad => "operationKindAtomicLoad",
        AtomicLoad8 => "operationKindAtomicLoad8",
        AtomicLoad16 => "operationKindAtomicLoad16",
        AtomicStore => "operationKindAtomicStore",
        AtomicStore8 => "operationKindAtomicStore8",
        AtomicStore16 => "operationKindAtomicStore16",
        AtomicRMW => "operationKindAtomicRMW",
        AtomicRMW8 => "operationKindAtomicRMW8",
        AtomicRMW16 => "operationKindAtomicRMW16",
        AtomicRMWCmpxchg => "operationKindAtomicRMWCmpxchg",
        AtomicRMW8Cmpxchg => "operationKindAtomicRMW8Cmpxchg",
        AtomicRMW16Cmpxchg => "operationKindAtomicRMW16Cmpxchg",
        TailCallReturnCall => "operationKindTailCallReturnCall",
        TailCallReturnCallIndirect => "operationKindTailCallReturnCallIndirect",
    }
}

impl Default for OperationKind {
    fn default() -> Self {
        Self::Unreachable
    }
}

define_display_enum! {
    pub enum LabelKind : u8 {
        Header => "header",
        Else => "else",
        Continuation => "continuation",
        Return => "return",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Label(u64);

impl Label {
    pub const fn new(kind: LabelKind, frame_id: u32) -> Self {
        Self(kind as u64 | ((frame_id as u64) << 32))
    }

    pub const fn kind(self) -> LabelKind {
        match self.0 as u32 as u8 {
            0 => LabelKind::Header,
            1 => LabelKind::Else,
            2 => LabelKind::Continuation,
            _ => LabelKind::Return,
        }
    }

    pub const fn frame_id(self) -> u32 {
        (self.0 >> 32) as u32
    }

    pub const fn is_return_target(self) -> bool {
        matches!(self.kind(), LabelKind::Return)
    }

    pub const fn into_raw(self) -> u64 {
        self.0
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            LabelKind::Header => write!(f, ".L{}", self.frame_id()),
            LabelKind::Else => write!(f, ".L{}_else", self.frame_id()),
            LabelKind::Continuation => write!(f, ".L{}_cont", self.frame_id()),
            LabelKind::Return => f.write_str(".return"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InclusiveRange {
    pub start: i32,
    pub end: i32,
}

impl InclusiveRange {
    pub const NOP: Self = Self { start: -1, end: -1 };

    pub const fn new(start: i32, end: i32) -> Self {
        Self { start, end }
    }

    pub const fn as_u64(self) -> u64 {
        ((self.start as u32 as u64) << 32) | (self.end as u32 as u64)
    }

    pub const fn from_u64(value: u64) -> Self {
        Self {
            start: ((value >> 32) as u32) as i32,
            end: value as u32 as i32,
        }
    }
}

impl Default for InclusiveRange {
    fn default() -> Self {
        Self::NOP
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MemoryArg {
    pub alignment: u32,
    pub offset: u32,
}

define_display_enum! {
    pub enum Shape : u8 {
        I8x16 => "I8x16",
        I16x8 => "I16x8",
        I32x4 => "I32x4",
        I64x2 => "I64x2",
        F32x4 => "F32x4",
        F64x2 => "F64x2",
    }
}

define_display_enum! {
    pub enum V128LoadType : u8 {
        Load128 => "128",
        Load8x8S => "8x8s",
        Load8x8U => "8x8u",
        Load16x4S => "16x4s",
        Load16x4U => "16x4u",
        Load32x2S => "32x2s",
        Load32x2U => "32x2u",
        Load8Splat => "8_splat",
        Load16Splat => "16_splat",
        Load32Splat => "32_splat",
        Load64Splat => "64_splat",
        Load32Zero => "32_zero",
        Load64Zero => "64_zero",
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum V128CmpType {
    I8x16Eq,
    I8x16Ne,
    I8x16LtS,
    I8x16LtU,
    I8x16GtS,
    I8x16GtU,
    I8x16LeS,
    I8x16LeU,
    I8x16GeS,
    I8x16GeU,
    I16x8Eq,
    I16x8Ne,
    I16x8LtS,
    I16x8LtU,
    I16x8GtS,
    I16x8GtU,
    I16x8LeS,
    I16x8LeU,
    I16x8GeS,
    I16x8GeU,
    I32x4Eq,
    I32x4Ne,
    I32x4LtS,
    I32x4LtU,
    I32x4GtS,
    I32x4GtU,
    I32x4LeS,
    I32x4LeU,
    I32x4GeS,
    I32x4GeU,
    I64x2Eq,
    I64x2Ne,
    I64x2LtS,
    I64x2GtS,
    I64x2LeS,
    I64x2GeS,
    F32x4Eq,
    F32x4Ne,
    F32x4Lt,
    F32x4Gt,
    F32x4Le,
    F32x4Ge,
    F64x2Eq,
    F64x2Ne,
    F64x2Lt,
    F64x2Gt,
    F64x2Le,
    F64x2Ge,
}

impl V128CmpType {
    pub const ALL: &'static [Self] = &[
        Self::I8x16Eq,
        Self::I8x16Ne,
        Self::I8x16LtS,
        Self::I8x16LtU,
        Self::I8x16GtS,
        Self::I8x16GtU,
        Self::I8x16LeS,
        Self::I8x16LeU,
        Self::I8x16GeS,
        Self::I8x16GeU,
        Self::I16x8Eq,
        Self::I16x8Ne,
        Self::I16x8LtS,
        Self::I16x8LtU,
        Self::I16x8GtS,
        Self::I16x8GtU,
        Self::I16x8LeS,
        Self::I16x8LeU,
        Self::I16x8GeS,
        Self::I16x8GeU,
        Self::I32x4Eq,
        Self::I32x4Ne,
        Self::I32x4LtS,
        Self::I32x4LtU,
        Self::I32x4GtS,
        Self::I32x4GtU,
        Self::I32x4LeS,
        Self::I32x4LeU,
        Self::I32x4GeS,
        Self::I32x4GeU,
        Self::I64x2Eq,
        Self::I64x2Ne,
        Self::I64x2LtS,
        Self::I64x2GtS,
        Self::I64x2LeS,
        Self::I64x2GeS,
        Self::F32x4Eq,
        Self::F32x4Ne,
        Self::F32x4Lt,
        Self::F32x4Gt,
        Self::F32x4Le,
        Self::F32x4Ge,
        Self::F64x2Eq,
        Self::F64x2Ne,
        Self::F64x2Lt,
        Self::F64x2Gt,
        Self::F64x2Le,
        Self::F64x2Ge,
    ];
}

impl fmt::Display for V128CmpType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

define_display_enum! {
    pub enum AtomicArithmeticOp : u8 {
        Add => "add",
        Sub => "sub",
        And => "and",
        Or => "or",
        Xor => "xor",
        Nop => "nop",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub kind: OperationKind,
    pub b1: u8,
    pub b2: u8,
    pub b3: bool,
    pub u1: u64,
    pub u2: u64,
    pub u3: u64,
    pub us: Vec<u64>,
}

impl Default for Instruction {
    fn default() -> Self {
        Self::new(OperationKind::Unreachable)
    }
}

impl Instruction {
    pub fn new(kind: OperationKind) -> Self {
        Self {
            kind,
            b1: 0,
            b2: 0,
            b3: false,
            u1: 0,
            u2: 0,
            u3: 0,
            us: Vec::new(),
        }
    }

    pub fn with_b1(mut self, value: u8) -> Self {
        self.b1 = value;
        self
    }

    pub fn with_b2(mut self, value: u8) -> Self {
        self.b2 = value;
        self
    }

    pub fn with_b3(mut self, value: bool) -> Self {
        self.b3 = value;
        self
    }

    pub fn with_u1(mut self, value: u64) -> Self {
        self.u1 = value;
        self
    }

    pub fn with_u2(mut self, value: u64) -> Self {
        self.u2 = value;
        self
    }

    pub fn with_u3(mut self, value: u64) -> Self {
        self.u3 = value;
        self
    }

    pub fn with_us(mut self, values: Vec<u64>) -> Self {
        self.us = values;
        self
    }

    pub fn unreachable() -> Self {
        Self::new(OperationKind::Unreachable)
    }

    pub fn label(label: Label) -> Self {
        Self::new(OperationKind::Label).with_u1(label.into_raw())
    }

    pub fn br(target: Label) -> Self {
        Self::new(OperationKind::Br).with_u1(target.into_raw())
    }

    pub fn br_if(then_target: Label, else_target: Label, then_drop: InclusiveRange) -> Self {
        Self::new(OperationKind::BrIf)
            .with_u1(then_target.into_raw())
            .with_u2(else_target.into_raw())
            .with_u3(then_drop.as_u64())
    }

    pub fn br_table(target_labels_and_ranges: Vec<u64>) -> Self {
        Self::new(OperationKind::BrTable).with_us(target_labels_and_ranges)
    }

    pub fn call(function_index: u32) -> Self {
        Self::new(OperationKind::Call).with_u1(function_index as u64)
    }

    pub fn call_indirect(type_index: u32, table_index: u32) -> Self {
        Self::new(OperationKind::CallIndirect)
            .with_u1(type_index as u64)
            .with_u2(table_index as u64)
    }

    pub fn drop(range: InclusiveRange) -> Self {
        Self::new(OperationKind::Drop).with_u1(range.as_u64())
    }

    pub fn select(is_target_vector: bool) -> Self {
        Self::new(OperationKind::Select).with_b3(is_target_vector)
    }

    pub fn pick(depth: usize, is_target_vector: bool) -> Self {
        Self::new(OperationKind::Pick)
            .with_u1(depth as u64)
            .with_b3(is_target_vector)
    }

    pub fn set(depth: usize, is_target_vector: bool) -> Self {
        Self::new(OperationKind::Set)
            .with_u1(depth as u64)
            .with_b3(is_target_vector)
    }

    pub fn global_get(index: u32) -> Self {
        Self::new(OperationKind::GlobalGet).with_u1(index as u64)
    }

    pub fn global_set(index: u32) -> Self {
        Self::new(OperationKind::GlobalSet).with_u1(index as u64)
    }

    pub fn load(ty: UnsignedType, arg: MemoryArg) -> Self {
        Self::new(OperationKind::Load)
            .with_b1(ty as u8)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn load8(ty: SignedInt, arg: MemoryArg) -> Self {
        Self::new(OperationKind::Load8)
            .with_b1(ty as u8)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn load16(ty: SignedInt, arg: MemoryArg) -> Self {
        Self::new(OperationKind::Load16)
            .with_b1(ty as u8)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn load32(signed: bool, arg: MemoryArg) -> Self {
        Self::new(OperationKind::Load32)
            .with_b1(u8::from(signed))
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn store(ty: UnsignedType, arg: MemoryArg) -> Self {
        Self::new(OperationKind::Store)
            .with_b1(ty as u8)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn store8(arg: MemoryArg) -> Self {
        Self::new(OperationKind::Store8)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn store16(arg: MemoryArg) -> Self {
        Self::new(OperationKind::Store16)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn store32(arg: MemoryArg) -> Self {
        Self::new(OperationKind::Store32)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn const_i32(value: u32) -> Self {
        Self::new(OperationKind::ConstI32).with_u1(value as u64)
    }

    pub fn const_i64(value: u64) -> Self {
        Self::new(OperationKind::ConstI64).with_u1(value)
    }

    pub fn const_f32(value: f32) -> Self {
        Self::new(OperationKind::ConstF32).with_u1(value.to_bits() as u64)
    }

    pub fn const_f64(value: f64) -> Self {
        Self::new(OperationKind::ConstF64).with_u1(value.to_bits())
    }

    pub fn i_trunc_from_f(
        input_type: FloatKind,
        output_type: SignedInt,
        non_trapping: bool,
    ) -> Self {
        Self::new(OperationKind::ITruncFromF)
            .with_b1(input_type as u8)
            .with_b2(output_type as u8)
            .with_b3(non_trapping)
    }

    pub fn f_convert_from_i(input_type: SignedInt, output_type: FloatKind) -> Self {
        Self::new(OperationKind::FConvertFromI)
            .with_b1(input_type as u8)
            .with_b2(output_type as u8)
    }

    pub fn extend(signed: bool) -> Self {
        Self::new(OperationKind::Extend).with_b1(u8::from(signed))
    }

    pub fn v128_const(lo: u64, hi: u64) -> Self {
        Self::new(OperationKind::V128Const).with_u1(lo).with_u2(hi)
    }

    pub fn v128_load(load_type: V128LoadType, arg: MemoryArg) -> Self {
        Self::new(OperationKind::V128Load)
            .with_b1(load_type as u8)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn v128_load_lane(lane_index: u8, lane_size: u8, arg: MemoryArg) -> Self {
        Self::new(OperationKind::V128LoadLane)
            .with_b1(lane_size)
            .with_b2(lane_index)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn v128_store(arg: MemoryArg) -> Self {
        Self::new(OperationKind::V128Store)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn v128_store_lane(lane_index: u8, lane_size: u8, arg: MemoryArg) -> Self {
        Self::new(OperationKind::V128StoreLane)
            .with_b1(lane_size)
            .with_b2(lane_index)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn v128_extract_lane(lane_index: u8, signed: bool, shape: Shape) -> Self {
        Self::new(OperationKind::V128ExtractLane)
            .with_b1(shape as u8)
            .with_b2(lane_index)
            .with_b3(signed)
    }

    pub fn v128_replace_lane(lane_index: u8, shape: Shape) -> Self {
        Self::new(OperationKind::V128ReplaceLane)
            .with_b1(shape as u8)
            .with_b2(lane_index)
    }

    pub fn v128_shuffle(lanes: Vec<u64>) -> Self {
        Self::new(OperationKind::V128Shuffle).with_us(lanes)
    }

    pub fn v128_cmp(cmp_type: V128CmpType) -> Self {
        Self::new(OperationKind::V128Cmp).with_b1(cmp_type as u8)
    }

    pub fn atomic_memory_wait(ty: UnsignedType, arg: MemoryArg) -> Self {
        Self::new(OperationKind::AtomicMemoryWait)
            .with_b1(ty as u8)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn atomic_memory_notify(arg: MemoryArg) -> Self {
        Self::new(OperationKind::AtomicMemoryNotify)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn atomic_fence() -> Self {
        Self::new(OperationKind::AtomicFence)
    }

    pub fn atomic_load(ty: UnsignedType, arg: MemoryArg) -> Self {
        Self::new(OperationKind::AtomicLoad)
            .with_b1(ty as u8)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn atomic_store(ty: UnsignedType, arg: MemoryArg) -> Self {
        Self::new(OperationKind::AtomicStore)
            .with_b1(ty as u8)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn atomic_rmw(ty: UnsignedType, arg: MemoryArg, op: AtomicArithmeticOp) -> Self {
        Self::new(OperationKind::AtomicRMW)
            .with_b1(ty as u8)
            .with_b2(op as u8)
            .with_u1(arg.alignment as u64)
            .with_u2(arg.offset as u64)
    }

    pub fn tail_call_return_call(function_index: u32) -> Self {
        Self::new(OperationKind::TailCallReturnCall).with_u1(function_index as u64)
    }

    pub fn tail_call_return_call_indirect(
        type_index: u32,
        table_index: u32,
        drop_depth: InclusiveRange,
        return_label: Label,
    ) -> Self {
        Self::new(OperationKind::TailCallReturnCallIndirect)
            .with_u1(type_index as u64)
            .with_u2(table_index as u64)
            .with_us(vec![drop_depth.as_u64(), return_label.into_raw()])
    }
}

impl fmt::Display for Instruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            OperationKind::Unreachable
            | OperationKind::Select
            | OperationKind::MemorySize
            | OperationKind::MemoryGrow
            | OperationKind::I32WrapFromI64
            | OperationKind::F32DemoteFromF64
            | OperationKind::F64PromoteFromF32
            | OperationKind::I32ReinterpretFromF32
            | OperationKind::I64ReinterpretFromF64
            | OperationKind::F32ReinterpretFromI32
            | OperationKind::F64ReinterpretFromI64
            | OperationKind::SignExtend32From8
            | OperationKind::SignExtend32From16
            | OperationKind::SignExtend64From8
            | OperationKind::SignExtend64From16
            | OperationKind::SignExtend64From32
            | OperationKind::MemoryCopy
            | OperationKind::MemoryFill
            | OperationKind::V128And
            | OperationKind::V128Not
            | OperationKind::V128Or
            | OperationKind::V128Xor
            | OperationKind::V128Bitselect
            | OperationKind::V128AndNot
            | OperationKind::V128Swizzle
            | OperationKind::V128AnyTrue
            | OperationKind::V128Q15mulrSatS
            | OperationKind::V128FloatPromote
            | OperationKind::V128FloatDemote
            | OperationKind::V128Dot
            | OperationKind::BuiltinFunctionCheckExitCode
            | OperationKind::AtomicFence => f.write_str(self.kind.as_str()),
            OperationKind::Label => write!(f, "{}", Label::from_raw(self.u1)),
            OperationKind::Br => write!(f, "{} {}", self.kind, Label::from_raw(self.u1)),
            OperationKind::BrIf => write!(
                f,
                "{} {}, {}",
                self.kind,
                Label::from_raw(self.u1),
                Label::from_raw(self.u2)
            ),
            OperationKind::BrTable => {
                let default_label = self
                    .us
                    .first()
                    .copied()
                    .map(Label::from_raw)
                    .map(|l| l.to_string())
                    .unwrap_or_default();
                let targets = self
                    .us
                    .iter()
                    .skip(1)
                    .map(|target| Label::from_raw(*target).to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                write!(f, "{} [{}] {}", self.kind, targets, default_label)
            }
            OperationKind::Call
            | OperationKind::GlobalGet
            | OperationKind::GlobalSet
            | OperationKind::MemoryInit
            | OperationKind::DataDrop
            | OperationKind::ElemDrop
            | OperationKind::RefFunc
            | OperationKind::TableGet
            | OperationKind::TableSet
            | OperationKind::TableSize
            | OperationKind::TableGrow
            | OperationKind::TableFill
            | OperationKind::TailCallReturnCall => write!(f, "{} {}", self.kind, self.u1),
            OperationKind::CallIndirect | OperationKind::TableInit | OperationKind::TableCopy => {
                write!(f, "{}: {} {}", self.kind, self.u1, self.u2)
            }
            OperationKind::Drop => {
                let range = InclusiveRange::from_u64(self.u1);
                write!(f, "{} {}..{}", self.kind, range.start, range.end)
            }
            OperationKind::Pick | OperationKind::Set => {
                write!(f, "{} {} (is_vector={})", self.kind, self.u1, self.b3)
            }
            OperationKind::Load | OperationKind::Store => write!(
                f,
                "{}.{} (align={}, offset={})",
                unsigned_type_from_raw(self.b1),
                self.kind,
                self.u1,
                self.u2
            ),
            OperationKind::Load8 | OperationKind::Load16 => write!(
                f,
                "{}.{} (align={}, offset={})",
                signed_int_from_raw(self.b1),
                self.kind,
                self.u1,
                self.u2
            ),
            OperationKind::Load32 => {
                let ty = if self.b1 == 1 { "i64" } else { "u64" };
                write!(
                    f,
                    "{}.{} (align={}, offset={})",
                    ty, self.kind, self.u1, self.u2
                )
            }
            OperationKind::Store8
            | OperationKind::Store16
            | OperationKind::Store32
            | OperationKind::AtomicMemoryNotify => {
                write!(f, "{} (align={}, offset={})", self.kind, self.u1, self.u2)
            }
            OperationKind::Eq
            | OperationKind::Ne
            | OperationKind::Add
            | OperationKind::Sub
            | OperationKind::Mul => write!(f, "{}.{}", unsigned_type_from_raw(self.b1), self.kind),
            OperationKind::Eqz
            | OperationKind::Clz
            | OperationKind::Ctz
            | OperationKind::Popcnt
            | OperationKind::And
            | OperationKind::Or
            | OperationKind::Xor
            | OperationKind::Shl
            | OperationKind::Rotl
            | OperationKind::Rotr => write!(f, "{}.{}", unsigned_int_from_raw(self.b1), self.kind),
            OperationKind::Rem | OperationKind::Shr => {
                write!(f, "{}.{}", signed_int_from_raw(self.b1), self.kind)
            }
            OperationKind::Lt
            | OperationKind::Gt
            | OperationKind::Le
            | OperationKind::Ge
            | OperationKind::Div => {
                write!(f, "{}.{}", signed_type_from_raw(self.b1), self.kind)
            }
            OperationKind::Abs
            | OperationKind::Neg
            | OperationKind::Ceil
            | OperationKind::Floor
            | OperationKind::Trunc
            | OperationKind::Nearest
            | OperationKind::Sqrt
            | OperationKind::Min
            | OperationKind::Max
            | OperationKind::Copysign => {
                write!(f, "{}.{}", float_kind_from_raw(self.b1), self.kind)
            }
            OperationKind::ConstI32 | OperationKind::ConstI64 => {
                write!(f, "{} {:#x}", self.kind, self.u1)
            }
            OperationKind::ConstF32 => {
                write!(f, "{} {}", self.kind, f32::from_bits(self.u1 as u32))
            }
            OperationKind::ConstF64 => write!(f, "{} {}", self.kind, f64::from_bits(self.u1)),
            OperationKind::ITruncFromF => write!(
                f,
                "{}.{}.{} (non_trapping={})",
                signed_int_from_raw(self.b2),
                self.kind,
                float_kind_from_raw(self.b1),
                self.b3
            ),
            OperationKind::FConvertFromI => write!(
                f,
                "{}.{}.{}",
                float_kind_from_raw(self.b2),
                self.kind,
                signed_int_from_raw(self.b1)
            ),
            OperationKind::Extend => {
                let signed = self.b1 == 1;
                let input = if signed { "i32" } else { "u32" };
                let output = if signed { "i64" } else { "u64" };
                write!(f, "{}.{}.{}", output, self.kind, input)
            }
            OperationKind::V128Const => write!(f, "{} [{:#x}, {:#x}]", self.kind, self.u1, self.u2),
            OperationKind::V128Add
            | OperationKind::V128Sub
            | OperationKind::V128AllTrue
            | OperationKind::V128BitMask
            | OperationKind::V128Shl
            | OperationKind::V128Mul
            | OperationKind::V128Div
            | OperationKind::V128Neg
            | OperationKind::V128Sqrt
            | OperationKind::V128Abs
            | OperationKind::V128Popcnt
            | OperationKind::V128AvgrU
            | OperationKind::V128Pmin
            | OperationKind::V128Pmax
            | OperationKind::V128Ceil
            | OperationKind::V128Floor
            | OperationKind::V128Trunc
            | OperationKind::V128Nearest => {
                write!(f, "{} (shape={})", self.kind, shape_from_raw(self.b1))
            }
            OperationKind::V128Load => write!(
                f,
                "{} (type={}, align={}, offset={})",
                self.kind,
                v128_load_type_from_raw(self.b1),
                self.u1,
                self.u2
            ),
            OperationKind::V128LoadLane | OperationKind::V128StoreLane => write!(
                f,
                "{} (lane_size={}, lane_index={}, align={}, offset={})",
                self.kind, self.b1, self.b2, self.u1, self.u2
            ),
            OperationKind::V128Store => {
                write!(f, "{} (align={}, offset={})", self.kind, self.u1, self.u2)
            }
            OperationKind::V128ExtractLane | OperationKind::V128ReplaceLane => write!(
                f,
                "{} (shape={}, lane_index={}, signed={})",
                self.kind,
                shape_from_raw(self.b1),
                self.b2,
                self.b3
            ),
            OperationKind::V128Splat => {
                write!(f, "{} (shape={})", self.kind, shape_from_raw(self.b1))
            }
            OperationKind::V128Shuffle => write!(f, "{} {:?}", self.kind, self.us),
            OperationKind::V128Shr
            | OperationKind::V128AddSat
            | OperationKind::V128SubSat
            | OperationKind::V128Min
            | OperationKind::V128Max
            | OperationKind::V128ExtAddPairwise
            | OperationKind::V128FConvertFromI
            | OperationKind::V128Narrow
            | OperationKind::V128ITruncSatFromF => write!(
                f,
                "{} (shape={}, signed={})",
                self.kind,
                shape_from_raw(self.b1),
                self.b3
            ),
            OperationKind::V128Cmp => {
                write!(f, "{} (cmp={})", self.kind, v128_cmp_type_from_raw(self.b1))
            }
            OperationKind::V128Extend | OperationKind::V128ExtMul => write!(
                f,
                "{} (shape={}, signed={}, use_low={})",
                self.kind,
                shape_from_raw(self.b1),
                self.b2 == 1,
                self.b3
            ),
            OperationKind::AtomicMemoryWait
            | OperationKind::AtomicLoad
            | OperationKind::AtomicLoad8
            | OperationKind::AtomicLoad16
            | OperationKind::AtomicStore
            | OperationKind::AtomicStore8
            | OperationKind::AtomicStore16
            | OperationKind::AtomicRMWCmpxchg
            | OperationKind::AtomicRMW8Cmpxchg
            | OperationKind::AtomicRMW16Cmpxchg => write!(
                f,
                "{} (type={}, align={}, offset={})",
                self.kind,
                unsigned_type_from_raw(self.b1),
                self.u1,
                self.u2
            ),
            OperationKind::AtomicRMW | OperationKind::AtomicRMW8 | OperationKind::AtomicRMW16 => {
                write!(
                    f,
                    "{} (type={}, op={}, align={}, offset={})",
                    self.kind,
                    unsigned_type_from_raw(self.b1),
                    atomic_arithmetic_op_from_raw(self.b2),
                    self.u1,
                    self.u2
                )
            }
            OperationKind::TailCallReturnCallIndirect => {
                let drop_depth = self
                    .us
                    .first()
                    .copied()
                    .map(InclusiveRange::from_u64)
                    .unwrap_or_default();
                let label = self
                    .us
                    .get(1)
                    .copied()
                    .map(Label::from_raw)
                    .unwrap_or_default();
                write!(
                    f,
                    "{} {} {} {}..{} {}",
                    self.kind, self.u1, self.u2, drop_depth.start, drop_depth.end, label
                )
            }
        }
    }
}

fn unsigned_int_from_raw(raw: u8) -> UnsignedInt {
    match raw {
        0 => UnsignedInt::I32,
        _ => UnsignedInt::I64,
    }
}

fn signed_int_from_raw(raw: u8) -> SignedInt {
    match raw {
        0 => SignedInt::Int32,
        1 => SignedInt::Int64,
        2 => SignedInt::Uint32,
        _ => SignedInt::Uint64,
    }
}

fn float_kind_from_raw(raw: u8) -> FloatKind {
    match raw {
        0 => FloatKind::F32,
        _ => FloatKind::F64,
    }
}

fn unsigned_type_from_raw(raw: u8) -> UnsignedType {
    match raw {
        0 => UnsignedType::I32,
        1 => UnsignedType::I64,
        2 => UnsignedType::F32,
        3 => UnsignedType::F64,
        4 => UnsignedType::V128,
        _ => UnsignedType::Unknown,
    }
}

fn signed_type_from_raw(raw: u8) -> SignedType {
    match raw {
        0 => SignedType::Int32,
        1 => SignedType::Uint32,
        2 => SignedType::Int64,
        3 => SignedType::Uint64,
        4 => SignedType::Float32,
        _ => SignedType::Float64,
    }
}

fn shape_from_raw(raw: u8) -> Shape {
    match raw {
        0 => Shape::I8x16,
        1 => Shape::I16x8,
        2 => Shape::I32x4,
        3 => Shape::I64x2,
        4 => Shape::F32x4,
        _ => Shape::F64x2,
    }
}

fn v128_load_type_from_raw(raw: u8) -> V128LoadType {
    match raw {
        0 => V128LoadType::Load128,
        1 => V128LoadType::Load8x8S,
        2 => V128LoadType::Load8x8U,
        3 => V128LoadType::Load16x4S,
        4 => V128LoadType::Load16x4U,
        5 => V128LoadType::Load32x2S,
        6 => V128LoadType::Load32x2U,
        7 => V128LoadType::Load8Splat,
        8 => V128LoadType::Load16Splat,
        9 => V128LoadType::Load32Splat,
        10 => V128LoadType::Load64Splat,
        11 => V128LoadType::Load32Zero,
        _ => V128LoadType::Load64Zero,
    }
}

fn v128_cmp_type_from_raw(raw: u8) -> V128CmpType {
    V128CmpType::ALL
        .get(raw as usize)
        .copied()
        .unwrap_or(V128CmpType::I8x16Eq)
}

fn atomic_arithmetic_op_from_raw(raw: u8) -> AtomicArithmeticOp {
    match raw {
        0 => AtomicArithmeticOp::Add,
        1 => AtomicArithmeticOp::Sub,
        2 => AtomicArithmeticOp::And,
        3 => AtomicArithmeticOp::Or,
        4 => AtomicArithmeticOp::Xor,
        _ => AtomicArithmeticOp::Nop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_kind_names_are_defined() {
        for kind in OperationKind::ALL {
            assert!(!kind.to_string().is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn instruction_display_is_defined_for_all_kinds() {
        for kind in OperationKind::ALL {
            let text = Instruction::new(*kind).to_string();
            assert!(!text.is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn label_roundtrip_preserves_fields() {
        for kind in LabelKind::ALL {
            let label = Label::new(*kind, 12_345);
            assert_eq!(*kind, label.kind());
            assert_eq!(12_345, label.frame_id());
        }
    }

    #[test]
    fn inclusive_range_roundtrip_preserves_bounds() {
        let range = InclusiveRange::new(-7, 42);
        assert_eq!(range, InclusiveRange::from_u64(range.as_u64()));
        assert_eq!(InclusiveRange::NOP, InclusiveRange::default());
    }

    #[test]
    fn constructors_pack_operands_like_go_model() {
        let arg = MemoryArg {
            alignment: 8,
            offset: 16,
        };
        let op = Instruction::i_trunc_from_f(FloatKind::F32, SignedInt::Uint64, true);
        assert_eq!(OperationKind::ITruncFromF, op.kind);
        assert_eq!(FloatKind::F32 as u8, op.b1);
        assert_eq!(SignedInt::Uint64 as u8, op.b2);
        assert!(op.b3);

        let tail = Instruction::tail_call_return_call_indirect(
            1,
            2,
            InclusiveRange::new(3, 4),
            Label::new(LabelKind::Return, 0),
        );
        assert_eq!(
            vec![
                InclusiveRange::new(3, 4).as_u64(),
                Label::new(LabelKind::Return, 0).into_raw()
            ],
            tail.us
        );

        let load = Instruction::load(UnsignedType::I64, arg);
        assert_eq!(UnsignedType::I64 as u8, load.b1);
        assert_eq!(8, load.u1);
        assert_eq!(16, load.u2);
    }
}
