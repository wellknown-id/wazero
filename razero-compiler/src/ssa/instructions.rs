use std::fmt;

use crate::ssa::basic_block::BasicBlockId;
use crate::ssa::builder::Builder;
use crate::ssa::cmp::{FloatCmpCond, IntegerCmpCond};
use crate::ssa::funcref::FuncRef;
use crate::ssa::signature::SignatureId;
use crate::ssa::types::Type;
use crate::ssa::vs::{Value, Values};
use crate::wazevoapi::ExitCode;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstructionId(pub u32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct InstructionGroupId(pub u32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SourceOffset(pub i64);

impl SourceOffset {
    pub const UNKNOWN: Self = Self(-1);

    pub const fn valid(self) -> bool {
        self.0 != Self::UNKNOWN.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AtomicRmwOp {
    Add = 0,
    Sub,
    And,
    Or,
    Xor,
    Xchg,
}

impl fmt::Display for AtomicRmwOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Add => "add",
            Self::Sub => "sub",
            Self::And => "and",
            Self::Or => "or",
            Self::Xor => "xor",
            Self::Xchg => "xchg",
        })
    }
}

#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Opcode {
    #[default]
    Invalid = 0,
    Undefined,
    Jump,
    Brz,
    Brnz,
    BrTable,
    ExitWithCode,
    ExitIfTrueWithCode,
    Return,
    Call,
    CallIndirect,
    Splat,
    Swizzle,
    Insertlane,
    Extractlane,
    Load,
    Store,
    Uload8,
    Sload8,
    Istore8,
    Uload16,
    Sload16,
    Istore16,
    Uload32,
    Sload32,
    Istore32,
    LoadSplat,
    VZeroExtLoad,
    Iconst,
    F32const,
    F64const,
    Vconst,
    Vbor,
    Vbxor,
    Vband,
    Vbandnot,
    Vbnot,
    Vbitselect,
    Shuffle,
    Select,
    VanyTrue,
    VallTrue,
    VhighBits,
    Icmp,
    VIcmp,
    IcmpImm,
    Iadd,
    VIadd,
    VSaddSat,
    VUaddSat,
    Isub,
    VIsub,
    VSsubSat,
    VUsubSat,
    VImin,
    VUmin,
    VImax,
    VUmax,
    VAvgRound,
    VImul,
    VIneg,
    VIpopcnt,
    VIabs,
    VIshl,
    VUshr,
    VSshr,
    VFabs,
    VFmax,
    VFmin,
    VFneg,
    VFadd,
    VFsub,
    VFmul,
    VFdiv,
    VFcmp,
    VCeil,
    VFloor,
    VTrunc,
    VNearest,
    VMaxPseudo,
    VMinPseudo,
    VSqrt,
    VFcvtToUintSat,
    VFcvtToSintSat,
    VFcvtFromUint,
    VFcvtFromSint,
    Imul,
    Udiv,
    Sdiv,
    Urem,
    Srem,
    Band,
    Bor,
    Bxor,
    Bnot,
    Rotl,
    Rotr,
    Ishl,
    Ushr,
    Sshr,
    Clz,
    Ctz,
    Popcnt,
    Fcmp,
    Fadd,
    Fsub,
    Fmul,
    SqmulRoundSat,
    Fdiv,
    Sqrt,
    Fneg,
    Fabs,
    Fcopysign,
    Fmin,
    Fmax,
    Ceil,
    Floor,
    Trunc,
    Nearest,
    Bitcast,
    Ireduce,
    Snarrow,
    Unarrow,
    SwidenLow,
    SwidenHigh,
    UwidenLow,
    UwidenHigh,
    ExtIaddPairwise,
    WideningPairwiseDotProductS,
    UExtend,
    SExtend,
    Fpromote,
    FvpromoteLow,
    Fdemote,
    Fvdemote,
    FcvtToUint,
    FcvtToSint,
    FcvtToUintSat,
    FcvtToSintSat,
    FcvtFromUint,
    FcvtFromSint,
    AtomicRmw,
    AtomicCas,
    AtomicLoad,
    AtomicStore,
    Fence,
    TailCallReturnCall,
    TailCallReturnCallIndirect,
}

impl Opcode {
    pub const fn is_branching(self) -> bool {
        matches!(self, Self::Jump | Self::Brz | Self::Brnz | Self::BrTable)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideEffect {
    Strict,
    Traps,
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Instruction {
    pub id: Option<InstructionId>,
    pub opcode: Opcode,
    pub u1: u64,
    pub u2: u64,
    pub v: Value,
    pub v2: Value,
    pub v3: Value,
    pub vs: Values,
    pub typ: Type,
    pub prev: Option<InstructionId>,
    pub next: Option<InstructionId>,
    pub r_value: Value,
    pub r_values: Values,
    pub gid: InstructionGroupId,
    pub source_offset: SourceOffset,
    pub live: bool,
    pub already_lowered: bool,
}

impl Instruction {
    pub fn new() -> Self {
        Self {
            v: Value::INVALID,
            v2: Value::INVALID,
            v3: Value::INVALID,
            r_value: Value::INVALID,
            source_offset: SourceOffset::UNKNOWN,
            ..Self::default()
        }
    }

    pub fn mark_lowered(&mut self) {
        self.already_lowered = true;
    }

    pub const fn lowered(&self) -> bool {
        self.already_lowered
    }

    pub const fn returns(&self) -> (Value, &Values) {
        (self.r_value, &self.r_values)
    }

    pub const fn return_(&self) -> Value {
        self.r_value
    }

    pub const fn args(&self) -> (Value, Value, Value, &Values) {
        (self.v, self.v2, self.v3, &self.vs)
    }

    pub const fn is_branching(&self) -> bool {
        self.opcode.is_branching()
    }

    pub fn with_opcode(mut self, opcode: Opcode) -> Self {
        self.opcode = opcode;
        self
    }

    pub fn as_iconst64(mut self, v: u64) -> Self {
        self.opcode = Opcode::Iconst;
        self.u1 = v;
        self.typ = Type::I64;
        self
    }

    pub fn as_iconst32(mut self, v: u32) -> Self {
        self.opcode = Opcode::Iconst;
        self.u1 = v as u64;
        self.typ = Type::I32;
        self
    }

    pub fn as_f32const(mut self, f: f32) -> Self {
        self.opcode = Opcode::F32const;
        self.typ = Type::F32;
        self.u1 = f.to_bits() as u64;
        self
    }

    pub fn as_f64const(mut self, f: f64) -> Self {
        self.opcode = Opcode::F64const;
        self.typ = Type::F64;
        self.u1 = f.to_bits();
        self
    }

    pub fn as_vconst(mut self, lo: u64, hi: u64) -> Self {
        self.opcode = Opcode::Vconst;
        self.typ = Type::V128;
        self.u1 = lo;
        self.u2 = hi;
        self
    }

    pub fn as_iadd(mut self, x: Value, y: Value) -> Self {
        self.opcode = Opcode::Iadd;
        self.typ = x.ty();
        self.v = x;
        self.v2 = y;
        self
    }

    pub fn as_isub(mut self, x: Value, y: Value) -> Self {
        self.opcode = Opcode::Isub;
        self.typ = x.ty();
        self.v = x;
        self.v2 = y;
        self
    }

    pub fn as_imul(mut self, x: Value, y: Value) -> Self {
        self.opcode = Opcode::Imul;
        self.typ = x.ty();
        self.v = x;
        self.v2 = y;
        self
    }

    pub fn as_icmp(mut self, x: Value, y: Value, c: IntegerCmpCond) -> Self {
        self.opcode = Opcode::Icmp;
        self.typ = Type::I32;
        self.v = x;
        self.v2 = y;
        self.u1 = c as u64;
        self
    }

    pub fn as_fcmp(mut self, x: Value, y: Value, c: FloatCmpCond) -> Self {
        self.opcode = Opcode::Fcmp;
        self.typ = Type::I32;
        self.v = x;
        self.v2 = y;
        self.u1 = c as u64;
        self
    }

    pub fn as_jump(mut self, args: Values, target: BasicBlockId) -> Self {
        self.opcode = Opcode::Jump;
        self.vs = args;
        self.r_value = Value(target.0 as u64);
        self
    }

    pub fn as_brz(mut self, cond: Value, args: Values, target: BasicBlockId) -> Self {
        self.opcode = Opcode::Brz;
        self.v = cond;
        self.vs = args;
        self.r_value = Value(target.0 as u64);
        self
    }

    pub fn as_brnz(mut self, cond: Value, args: Values, target: BasicBlockId) -> Self {
        self.opcode = Opcode::Brnz;
        self.v = cond;
        self.vs = args;
        self.r_value = Value(target.0 as u64);
        self
    }

    pub fn as_br_table(mut self, index: Value, targets: Values) -> Self {
        self.opcode = Opcode::BrTable;
        self.v = index;
        self.r_values = targets;
        self
    }

    pub fn as_return(mut self, values: Values) -> Self {
        self.opcode = Opcode::Return;
        self.vs = values;
        self
    }

    pub fn as_call(mut self, func_ref: FuncRef, sig: SignatureId, args: Values) -> Self {
        self.opcode = Opcode::Call;
        self.u1 = func_ref.0 as u64;
        self.u2 = sig.0 as u64;
        self.vs = args;
        self
    }

    pub fn as_call_indirect(mut self, func_ptr: Value, sig: SignatureId, args: Values) -> Self {
        self.opcode = Opcode::CallIndirect;
        self.v = func_ptr;
        self.u1 = sig.0 as u64;
        self.vs = args;
        self
    }

    pub fn as_load(mut self, ptr: Value, offset: u32, typ: Type) -> Self {
        self.opcode = Opcode::Load;
        self.v = ptr;
        self.u1 = offset as u64;
        self.typ = typ;
        self
    }

    pub fn as_store(mut self, store_op: Opcode, value: Value, ptr: Value, offset: u32) -> Self {
        self.opcode = store_op;
        self.v = value;
        self.v2 = ptr;
        let size = match store_op {
            Opcode::Store => value.ty().bits() as u64,
            Opcode::Istore8 => 8,
            Opcode::Istore16 => 16,
            Opcode::Istore32 => 32,
            _ => panic!("invalid store opcode: {:?}", store_op),
        };
        self.u1 = offset as u64 | (size << 32);
        self
    }

    pub fn as_exit_if_true_with_code(mut self, ctx: Value, cond: Value, code: ExitCode) -> Self {
        self.opcode = Opcode::ExitIfTrueWithCode;
        self.v = ctx;
        self.v2 = cond;
        self.u1 = code.raw() as u64;
        self
    }

    pub fn as_sextend(mut self, v: Value, from: u8, to: u8) -> Self {
        self.opcode = Opcode::SExtend;
        self.v = v;
        self.u1 = (u64::from(from) << 8) | u64::from(to);
        self.typ = match to {
            64 => Type::I64,
            32 | 16 | 8 => Type::I32,
            _ => panic!("invalid sextend target width: {to}"),
        };
        self
    }

    pub fn as_uextend(mut self, v: Value, from: u8, to: u8) -> Self {
        self.opcode = Opcode::UExtend;
        self.v = v;
        self.u1 = (u64::from(from) << 8) | u64::from(to);
        self.typ = match to {
            64 => Type::I64,
            32 | 16 | 8 => Type::I32,
            _ => panic!("invalid uextend target width: {to}"),
        };
        self
    }

    pub fn call_data(&self) -> (FuncRef, SignatureId, &[Value]) {
        (
            FuncRef(self.u1 as u32),
            SignatureId(self.u2 as u32),
            self.vs.as_slice(),
        )
    }

    pub fn call_indirect_data(&self) -> (Value, SignatureId, &[Value]) {
        (self.v, SignatureId(self.u1 as u32), self.vs.as_slice())
    }

    pub fn load_data(&self) -> (Value, u32, Type) {
        (self.v, self.u1 as u32, self.typ)
    }

    pub fn store_data(&self) -> (Value, Value, u32, u8) {
        (self.v, self.v2, self.u1 as u32, (self.u1 >> 32) as u8)
    }

    pub fn exit_with_code_data(&self) -> (Value, ExitCode) {
        (self.v, ExitCode::new(self.u1 as u32))
    }

    pub fn exit_if_true_with_code_data(&self) -> (Value, Value, ExitCode) {
        (self.v, self.v2, ExitCode::new(self.u1 as u32))
    }

    pub fn iconst_data(&self) -> u64 {
        self.u1
    }

    pub fn f32const_data(&self) -> f32 {
        f32::from_bits(self.u1 as u32)
    }

    pub fn f64const_data(&self) -> f64 {
        f64::from_bits(self.u1)
    }

    pub fn vconst_data(&self) -> (u64, u64) {
        (self.u1, self.u2)
    }

    pub fn extend_data(&self) -> (u8, u8, bool) {
        if self.opcode != Opcode::SExtend && self.opcode != Opcode::UExtend {
            panic!("extend_data only available for Opcode::SExtend and Opcode::UExtend");
        }
        (
            (self.u1 >> 8) as u8,
            self.u1 as u8,
            self.opcode == Opcode::SExtend,
        )
    }

    pub fn branch_data(&self) -> (Value, &[Value], BasicBlockId) {
        let cond = match self.opcode {
            Opcode::Jump => Value::INVALID,
            Opcode::Brz | Opcode::Brnz => self.v,
            _ => panic!("branch_data only available for branch instructions"),
        };
        (
            cond,
            self.vs.as_slice(),
            BasicBlockId(self.r_value.0 as u32),
        )
    }

    pub fn br_table_data(&self) -> (Value, &[Value]) {
        assert_eq!(self.opcode, Opcode::BrTable);
        (self.v, self.r_values.as_slice())
    }

    pub fn invert_brx(&mut self) {
        self.opcode = match self.opcode {
            Opcode::Brz => Opcode::Brnz,
            Opcode::Brnz => Opcode::Brz,
            _ => panic!("invert_brx only valid for Brz/Brnz"),
        };
    }

    pub fn add_argument_branch_inst(&mut self, value: Value) {
        match self.opcode {
            Opcode::Jump | Opcode::Brz | Opcode::Brnz => self.vs.push(value),
            _ => panic!("cannot add block arguments to {:?}", self.opcode),
        }
    }

    pub fn side_effect(&self) -> SideEffect {
        match self.opcode {
            Opcode::Undefined
            | Opcode::Jump
            | Opcode::Brz
            | Opcode::Brnz
            | Opcode::BrTable
            | Opcode::Return
            | Opcode::Call
            | Opcode::CallIndirect
            | Opcode::Store
            | Opcode::Istore8
            | Opcode::Istore16
            | Opcode::Istore32
            | Opcode::ExitWithCode
            | Opcode::ExitIfTrueWithCode
            | Opcode::AtomicRmw
            | Opcode::AtomicCas
            | Opcode::AtomicLoad
            | Opcode::AtomicStore
            | Opcode::Fence
            | Opcode::TailCallReturnCall
            | Opcode::TailCallReturnCallIndirect => SideEffect::Strict,
            Opcode::Sdiv
            | Opcode::Srem
            | Opcode::Udiv
            | Opcode::Urem
            | Opcode::FcvtToSint
            | Opcode::FcvtToUint => SideEffect::Traps,
            _ => SideEffect::None,
        }
    }

    pub fn result_types(&self, builder: &Builder) -> (Option<Type>, Vec<Type>) {
        match self.opcode {
            Opcode::Invalid
            | Opcode::Undefined
            | Opcode::Jump
            | Opcode::Brz
            | Opcode::Brnz
            | Opcode::BrTable
            | Opcode::ExitWithCode
            | Opcode::ExitIfTrueWithCode
            | Opcode::Return
            | Opcode::Store
            | Opcode::Istore8
            | Opcode::Istore16
            | Opcode::Istore32
            | Opcode::AtomicStore
            | Opcode::Fence
            | Opcode::TailCallReturnCall
            | Opcode::TailCallReturnCallIndirect => (None, Vec::new()),
            Opcode::Call => builder
                .resolve_signature(SignatureId(self.u2 as u32))
                .map(|sig| {
                    sig.results
                        .split_first()
                        .map_or((None, Vec::new()), |(first, rest)| {
                            (Some(*first), rest.to_vec())
                        })
                })
                .unwrap_or((None, Vec::new())),
            Opcode::CallIndirect => builder
                .resolve_signature(SignatureId(self.u1 as u32))
                .map(|sig| {
                    sig.results
                        .split_first()
                        .map_or((None, Vec::new()), |(first, rest)| {
                            (Some(*first), rest.to_vec())
                        })
                })
                .unwrap_or((None, Vec::new())),
            _ if self.typ.is_valid() => (Some(self.typ), Vec::new()),
            _ => (None, Vec::new()),
        }
    }

    pub fn format(&self, builder: &Builder) -> String {
        let mut s = String::new();
        if self.r_value.valid() {
            s.push_str(&builder.format_value_with_type(self.r_value));
            let rest = self.r_values.as_slice();
            if !rest.is_empty() {
                s.push_str(", ");
                s.push_str(
                    &rest
                        .iter()
                        .copied()
                        .map(|v| builder.format_value_with_type(v))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            }
            s.push_str(" = ");
        }
        s.push_str(&format!("{:?}", self.opcode));
        match self.opcode {
            Opcode::Jump | Opcode::Brz | Opcode::Brnz => {
                let (cond, args, target) = self.branch_data();
                if cond.valid() {
                    s.push(' ');
                    s.push_str(&builder.format_value(cond));
                    s.push(',');
                }
                s.push(' ');
                s.push_str(&target.to_string());
                if !args.is_empty() {
                    s.push_str(", ");
                    s.push_str(
                        &args
                            .iter()
                            .copied()
                            .map(|v| builder.format_value(v))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
            }
            Opcode::BrTable => {
                let (index, targets) = self.br_table_data();
                s.push(' ');
                s.push_str(&builder.format_value(index));
                s.push_str(", [");
                s.push_str(
                    &targets
                        .iter()
                        .copied()
                        .map(|v| BasicBlockId(v.0 as u32).to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                s.push(']');
            }
            Opcode::Iconst => s.push_str(&format!(" {}", self.iconst_data())),
            Opcode::F32const => s.push_str(&format!(" {}", self.f32const_data())),
            Opcode::F64const => s.push_str(&format!(" {}", self.f64const_data())),
            Opcode::Vconst => {
                let (lo, hi) = self.vconst_data();
                s.push_str(&format!(" {lo:#x}, {hi:#x}"));
            }
            Opcode::Call => {
                let (fref, sig, args) = self.call_data();
                s.push_str(&format!(" {fref}, {sig}"));
                if !args.is_empty() {
                    s.push_str(", ");
                    s.push_str(
                        &args
                            .iter()
                            .copied()
                            .map(|v| builder.format_value(v))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
            }
            Opcode::CallIndirect => {
                let (callee, sig, args) = self.call_indirect_data();
                s.push_str(&format!(" {sig}, {}", builder.format_value(callee)));
                if !args.is_empty() {
                    s.push_str(", ");
                    s.push_str(
                        &args
                            .iter()
                            .copied()
                            .map(|v| builder.format_value(v))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
            }
            Opcode::Load
            | Opcode::Uload8
            | Opcode::Sload8
            | Opcode::Uload16
            | Opcode::Sload16
            | Opcode::Uload32
            | Opcode::Sload32 => {
                let (ptr, offset, _) = self.load_data();
                s.push_str(&format!(" {}, {offset:#x}", builder.format_value(ptr)));
            }
            Opcode::Store | Opcode::Istore8 | Opcode::Istore16 | Opcode::Istore32 => {
                let (value, ptr, offset, _) = self.store_data();
                s.push_str(&format!(
                    " {}, {}, {offset:#x}",
                    builder.format_value(value),
                    builder.format_value(ptr)
                ));
            }
            Opcode::ExitWithCode => {
                let (ctx, code) = self.exit_with_code_data();
                s.push_str(&format!(" {}, {}", builder.format_value(ctx), code));
            }
            Opcode::ExitIfTrueWithCode => {
                let (ctx, cond, code) = self.exit_if_true_with_code_data();
                s.push_str(&format!(
                    " {}, {}, {}",
                    builder.format_value(cond),
                    builder.format_value(ctx),
                    code
                ));
            }
            _ => {
                let mut args = Vec::new();
                if self.v.valid() {
                    args.push(builder.format_value(self.v));
                }
                if self.v2.valid() {
                    args.push(builder.format_value(self.v2));
                }
                if self.v3.valid() {
                    args.push(builder.format_value(self.v3));
                }
                args.extend(self.vs.iter().map(|v| builder.format_value(v)));
                if !args.is_empty() {
                    s.push(' ');
                    s.push_str(&args.join(", "));
                }
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::{Instruction, Opcode};
    use crate::ssa::{Type, Value};

    #[test]
    fn invert_conditional_branch() {
        let mut i = Instruction::new().with_opcode(Opcode::Brnz);
        i.invert_brx();
        assert_eq!(i.opcode, Opcode::Brz);
        i.invert_brx();
        assert_eq!(i.opcode, Opcode::Brnz);
    }

    #[test]
    fn extend_builders_store_metadata() {
        let v = Value(1).with_type(Type::I32);
        let sext = Instruction::new().as_sextend(v, 8, 32);
        assert_eq!(sext.extend_data(), (8, 32, true));
        assert_eq!(sext.typ, Type::I32);

        let uext = Instruction::new().as_uextend(v, 32, 64);
        assert_eq!(uext.extend_data(), (32, 64, false));
        assert_eq!(uext.typ, Type::I64);
    }
}
