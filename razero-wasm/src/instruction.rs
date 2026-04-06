#![doc = "Wasm opcode definitions and instruction helpers."]

pub type Opcode = u8;
pub type OpcodeMisc = u8;
pub type OpcodeVec = u8;
pub type OpcodeAtomic = u8;
pub type OpcodeTailCall = u8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Instruction {
    pub opcode: Opcode,
}

impl Instruction {
    pub const fn new(opcode: Opcode) -> Self {
        Self { opcode }
    }

    pub const fn name(self) -> &'static str {
        instruction_name(self.opcode)
    }
}

impl From<Opcode> for Instruction {
    fn from(opcode: Opcode) -> Self {
        Self::new(opcode)
    }
}

// Opcode constants mirrored from internal/wasm/instruction.go.
pub const OPCODE_UNREACHABLE: Opcode = 0x00;
pub const OPCODE_NOP: Opcode = 0x01;
pub const OPCODE_BLOCK: Opcode = 0x02;
pub const OPCODE_LOOP: Opcode = 0x03;
pub const OPCODE_IF: Opcode = 0x04;
pub const OPCODE_ELSE: Opcode = 0x05;
pub const OPCODE_END: Opcode = 0x0b;
pub const OPCODE_BR: Opcode = 0x0c;
pub const OPCODE_BR_IF: Opcode = 0x0d;
pub const OPCODE_BR_TABLE: Opcode = 0x0e;
pub const OPCODE_RETURN: Opcode = 0x0f;
pub const OPCODE_CALL: Opcode = 0x10;
pub const OPCODE_CALL_INDIRECT: Opcode = 0x11;
pub const OPCODE_DROP: Opcode = 0x1a;
pub const OPCODE_SELECT: Opcode = 0x1b;
pub const OPCODE_TYPED_SELECT: Opcode = 0x1c;
pub const OPCODE_LOCAL_GET: Opcode = 0x20;
pub const OPCODE_LOCAL_SET: Opcode = 0x21;
pub const OPCODE_LOCAL_TEE: Opcode = 0x22;
pub const OPCODE_GLOBAL_GET: Opcode = 0x23;
pub const OPCODE_GLOBAL_SET: Opcode = 0x24;
pub const OPCODE_TABLE_GET: Opcode = 0x25;
pub const OPCODE_TABLE_SET: Opcode = 0x26;
pub const OPCODE_I32_LOAD: Opcode = 0x28;
pub const OPCODE_I64_LOAD: Opcode = 0x29;
pub const OPCODE_F32_LOAD: Opcode = 0x2a;
pub const OPCODE_F64_LOAD: Opcode = 0x2b;
pub const OPCODE_I32_LOAD8_S: Opcode = 0x2c;
pub const OPCODE_I32_LOAD8_U: Opcode = 0x2d;
pub const OPCODE_I32_LOAD16_S: Opcode = 0x2e;
pub const OPCODE_I32_LOAD16_U: Opcode = 0x2f;
pub const OPCODE_I64_LOAD8_S: Opcode = 0x30;
pub const OPCODE_I64_LOAD8_U: Opcode = 0x31;
pub const OPCODE_I64_LOAD16_S: Opcode = 0x32;
pub const OPCODE_I64_LOAD16_U: Opcode = 0x33;
pub const OPCODE_I64_LOAD32_S: Opcode = 0x34;
pub const OPCODE_I64_LOAD32_U: Opcode = 0x35;
pub const OPCODE_I32_STORE: Opcode = 0x36;
pub const OPCODE_I64_STORE: Opcode = 0x37;
pub const OPCODE_F32_STORE: Opcode = 0x38;
pub const OPCODE_F64_STORE: Opcode = 0x39;
pub const OPCODE_I32_STORE8: Opcode = 0x3a;
pub const OPCODE_I32_STORE16: Opcode = 0x3b;
pub const OPCODE_I64_STORE8: Opcode = 0x3c;
pub const OPCODE_I64_STORE16: Opcode = 0x3d;
pub const OPCODE_I64_STORE32: Opcode = 0x3e;
pub const OPCODE_MEMORY_SIZE: Opcode = 0x3f;
pub const OPCODE_MEMORY_GROW: Opcode = 0x40;
pub const OPCODE_I32_CONST: Opcode = 0x41;
pub const OPCODE_I64_CONST: Opcode = 0x42;
pub const OPCODE_F32_CONST: Opcode = 0x43;
pub const OPCODE_F64_CONST: Opcode = 0x44;
pub const OPCODE_I32_EQZ: Opcode = 0x45;
pub const OPCODE_I32_EQ: Opcode = 0x46;
pub const OPCODE_I32_NE: Opcode = 0x47;
pub const OPCODE_I32_LT_S: Opcode = 0x48;
pub const OPCODE_I32_LT_U: Opcode = 0x49;
pub const OPCODE_I32_GT_S: Opcode = 0x4a;
pub const OPCODE_I32_GT_U: Opcode = 0x4b;
pub const OPCODE_I32_LE_S: Opcode = 0x4c;
pub const OPCODE_I32_LE_U: Opcode = 0x4d;
pub const OPCODE_I32_GE_S: Opcode = 0x4e;
pub const OPCODE_I32_GE_U: Opcode = 0x4f;
pub const OPCODE_I64_EQZ: Opcode = 0x50;
pub const OPCODE_I64_EQ: Opcode = 0x51;
pub const OPCODE_I64_NE: Opcode = 0x52;
pub const OPCODE_I64_LT_S: Opcode = 0x53;
pub const OPCODE_I64_LT_U: Opcode = 0x54;
pub const OPCODE_I64_GT_S: Opcode = 0x55;
pub const OPCODE_I64_GT_U: Opcode = 0x56;
pub const OPCODE_I64_LE_S: Opcode = 0x57;
pub const OPCODE_I64_LE_U: Opcode = 0x58;
pub const OPCODE_I64_GE_S: Opcode = 0x59;
pub const OPCODE_I64_GE_U: Opcode = 0x5a;
pub const OPCODE_F32_EQ: Opcode = 0x5b;
pub const OPCODE_F32_NE: Opcode = 0x5c;
pub const OPCODE_F32_LT: Opcode = 0x5d;
pub const OPCODE_F32_GT: Opcode = 0x5e;
pub const OPCODE_F32_LE: Opcode = 0x5f;
pub const OPCODE_F32_GE: Opcode = 0x60;
pub const OPCODE_F64_EQ: Opcode = 0x61;
pub const OPCODE_F64_NE: Opcode = 0x62;
pub const OPCODE_F64_LT: Opcode = 0x63;
pub const OPCODE_F64_GT: Opcode = 0x64;
pub const OPCODE_F64_LE: Opcode = 0x65;
pub const OPCODE_F64_GE: Opcode = 0x66;
pub const OPCODE_I32_CLZ: Opcode = 0x67;
pub const OPCODE_I32_CTZ: Opcode = 0x68;
pub const OPCODE_I32_POPCNT: Opcode = 0x69;
pub const OPCODE_I32_ADD: Opcode = 0x6a;
pub const OPCODE_I32_SUB: Opcode = 0x6b;
pub const OPCODE_I32_MUL: Opcode = 0x6c;
pub const OPCODE_I32_DIV_S: Opcode = 0x6d;
pub const OPCODE_I32_DIV_U: Opcode = 0x6e;
pub const OPCODE_I32_REM_S: Opcode = 0x6f;
pub const OPCODE_I32_REM_U: Opcode = 0x70;
pub const OPCODE_I32_AND: Opcode = 0x71;
pub const OPCODE_I32_OR: Opcode = 0x72;
pub const OPCODE_I32_XOR: Opcode = 0x73;
pub const OPCODE_I32_SHL: Opcode = 0x74;
pub const OPCODE_I32_SHR_S: Opcode = 0x75;
pub const OPCODE_I32_SHR_U: Opcode = 0x76;
pub const OPCODE_I32_ROTL: Opcode = 0x77;
pub const OPCODE_I32_ROTR: Opcode = 0x78;
pub const OPCODE_I64_CLZ: Opcode = 0x79;
pub const OPCODE_I64_CTZ: Opcode = 0x7a;
pub const OPCODE_I64_POPCNT: Opcode = 0x7b;
pub const OPCODE_I64_ADD: Opcode = 0x7c;
pub const OPCODE_I64_SUB: Opcode = 0x7d;
pub const OPCODE_I64_MUL: Opcode = 0x7e;
pub const OPCODE_I64_DIV_S: Opcode = 0x7f;
pub const OPCODE_I64_DIV_U: Opcode = 0x80;
pub const OPCODE_I64_REM_S: Opcode = 0x81;
pub const OPCODE_I64_REM_U: Opcode = 0x82;
pub const OPCODE_I64_AND: Opcode = 0x83;
pub const OPCODE_I64_OR: Opcode = 0x84;
pub const OPCODE_I64_XOR: Opcode = 0x85;
pub const OPCODE_I64_SHL: Opcode = 0x86;
pub const OPCODE_I64_SHR_S: Opcode = 0x87;
pub const OPCODE_I64_SHR_U: Opcode = 0x88;
pub const OPCODE_I64_ROTL: Opcode = 0x89;
pub const OPCODE_I64_ROTR: Opcode = 0x8a;
pub const OPCODE_F32_ABS: Opcode = 0x8b;
pub const OPCODE_F32_NEG: Opcode = 0x8c;
pub const OPCODE_F32_CEIL: Opcode = 0x8d;
pub const OPCODE_F32_FLOOR: Opcode = 0x8e;
pub const OPCODE_F32_TRUNC: Opcode = 0x8f;
pub const OPCODE_F32_NEAREST: Opcode = 0x90;
pub const OPCODE_F32_SQRT: Opcode = 0x91;
pub const OPCODE_F32_ADD: Opcode = 0x92;
pub const OPCODE_F32_SUB: Opcode = 0x93;
pub const OPCODE_F32_MUL: Opcode = 0x94;
pub const OPCODE_F32_DIV: Opcode = 0x95;
pub const OPCODE_F32_MIN: Opcode = 0x96;
pub const OPCODE_F32_MAX: Opcode = 0x97;
pub const OPCODE_F32_COPYSIGN: Opcode = 0x98;
pub const OPCODE_F64_ABS: Opcode = 0x99;
pub const OPCODE_F64_NEG: Opcode = 0x9a;
pub const OPCODE_F64_CEIL: Opcode = 0x9b;
pub const OPCODE_F64_FLOOR: Opcode = 0x9c;
pub const OPCODE_F64_TRUNC: Opcode = 0x9d;
pub const OPCODE_F64_NEAREST: Opcode = 0x9e;
pub const OPCODE_F64_SQRT: Opcode = 0x9f;
pub const OPCODE_F64_ADD: Opcode = 0xa0;
pub const OPCODE_F64_SUB: Opcode = 0xa1;
pub const OPCODE_F64_MUL: Opcode = 0xa2;
pub const OPCODE_F64_DIV: Opcode = 0xa3;
pub const OPCODE_F64_MIN: Opcode = 0xa4;
pub const OPCODE_F64_MAX: Opcode = 0xa5;
pub const OPCODE_F64_COPYSIGN: Opcode = 0xa6;
pub const OPCODE_I32_WRAP_I64: Opcode = 0xa7;
pub const OPCODE_I32_TRUNC_F32_S: Opcode = 0xa8;
pub const OPCODE_I32_TRUNC_F32_U: Opcode = 0xa9;
pub const OPCODE_I32_TRUNC_F64_S: Opcode = 0xaa;
pub const OPCODE_I32_TRUNC_F64_U: Opcode = 0xab;
pub const OPCODE_I64_EXTEND_I32_S: Opcode = 0xac;
pub const OPCODE_I64_EXTEND_I32_U: Opcode = 0xad;
pub const OPCODE_I64_TRUNC_F32_S: Opcode = 0xae;
pub const OPCODE_I64_TRUNC_F32_U: Opcode = 0xaf;
pub const OPCODE_I64_TRUNC_F64_S: Opcode = 0xb0;
pub const OPCODE_I64_TRUNC_F64_U: Opcode = 0xb1;
pub const OPCODE_F32_CONVERT_I32_S: Opcode = 0xb2;
pub const OPCODE_F32_CONVERT_I32_U: Opcode = 0xb3;
pub const OPCODE_F32_CONVERT_I64_S: Opcode = 0xb4;
pub const OPCODE_F32_CONVERT_I64_U: Opcode = 0xb5;
pub const OPCODE_F32_DEMOTE_F64: Opcode = 0xb6;
pub const OPCODE_F64_CONVERT_I32_S: Opcode = 0xb7;
pub const OPCODE_F64_CONVERT_I32_U: Opcode = 0xb8;
pub const OPCODE_F64_CONVERT_I64_S: Opcode = 0xb9;
pub const OPCODE_F64_CONVERT_I64_U: Opcode = 0xba;
pub const OPCODE_F64_PROMOTE_F32: Opcode = 0xbb;
pub const OPCODE_I32_REINTERPRET_F32: Opcode = 0xbc;
pub const OPCODE_I64_REINTERPRET_F64: Opcode = 0xbd;
pub const OPCODE_F32_REINTERPRET_I32: Opcode = 0xbe;
pub const OPCODE_F64_REINTERPRET_I64: Opcode = 0xbf;
pub const OPCODE_REF_NULL: Opcode = 0xd0;
pub const OPCODE_REF_IS_NULL: Opcode = 0xd1;
pub const OPCODE_REF_FUNC: Opcode = 0xd2;
pub const OPCODE_I32_EXTEND8_S: Opcode = 0xc0;
pub const OPCODE_I32_EXTEND16_S: Opcode = 0xc1;
pub const OPCODE_I64_EXTEND8_S: Opcode = 0xc2;
pub const OPCODE_I64_EXTEND16_S: Opcode = 0xc3;
pub const OPCODE_I64_EXTEND32_S: Opcode = 0xc4;
pub const OPCODE_MISC_PREFIX: Opcode = 0xfc;
pub const OPCODE_VEC_PREFIX: Opcode = 0xfd;
pub const OPCODE_ATOMIC_PREFIX: Opcode = 0xfe;

// OpcodeMisc constants mirrored from internal/wasm/instruction.go.
pub const OPCODE_MISC_I32_TRUNC_SAT_F32_S: OpcodeMisc = 0x00;
pub const OPCODE_MISC_I32_TRUNC_SAT_F32_U: OpcodeMisc = 0x01;
pub const OPCODE_MISC_I32_TRUNC_SAT_F64_S: OpcodeMisc = 0x02;
pub const OPCODE_MISC_I32_TRUNC_SAT_F64_U: OpcodeMisc = 0x03;
pub const OPCODE_MISC_I64_TRUNC_SAT_F32_S: OpcodeMisc = 0x04;
pub const OPCODE_MISC_I64_TRUNC_SAT_F32_U: OpcodeMisc = 0x05;
pub const OPCODE_MISC_I64_TRUNC_SAT_F64_S: OpcodeMisc = 0x06;
pub const OPCODE_MISC_I64_TRUNC_SAT_F64_U: OpcodeMisc = 0x07;
pub const OPCODE_MISC_MEMORY_INIT: OpcodeMisc = 0x08;
pub const OPCODE_MISC_DATA_DROP: OpcodeMisc = 0x09;
pub const OPCODE_MISC_MEMORY_COPY: OpcodeMisc = 0x0a;
pub const OPCODE_MISC_MEMORY_FILL: OpcodeMisc = 0x0b;
pub const OPCODE_MISC_TABLE_INIT: OpcodeMisc = 0x0c;
pub const OPCODE_MISC_ELEM_DROP: OpcodeMisc = 0x0d;
pub const OPCODE_MISC_TABLE_COPY: OpcodeMisc = 0x0e;
pub const OPCODE_MISC_TABLE_GROW: OpcodeMisc = 0x0f;
pub const OPCODE_MISC_TABLE_SIZE: OpcodeMisc = 0x10;
pub const OPCODE_MISC_TABLE_FILL: OpcodeMisc = 0x11;

// OpcodeVec constants mirrored from internal/wasm/instruction.go.
pub const OPCODE_VEC_V128_LOAD: OpcodeVec = 0x00;
pub const OPCODE_VEC_V128_LOAD8X8S: OpcodeVec = 0x01;
pub const OPCODE_VEC_V128_LOAD8X8U: OpcodeVec = 0x02;
pub const OPCODE_VEC_V128_LOAD16X4S: OpcodeVec = 0x03;
pub const OPCODE_VEC_V128_LOAD16X4U: OpcodeVec = 0x04;
pub const OPCODE_VEC_V128_LOAD32X2S: OpcodeVec = 0x05;
pub const OPCODE_VEC_V128_LOAD32X2U: OpcodeVec = 0x06;
pub const OPCODE_VEC_V128_LOAD8_SPLAT: OpcodeVec = 0x07;
pub const OPCODE_VEC_V128_LOAD16_SPLAT: OpcodeVec = 0x08;
pub const OPCODE_VEC_V128_LOAD32_SPLAT: OpcodeVec = 0x09;
pub const OPCODE_VEC_V128_LOAD64_SPLAT: OpcodeVec = 0x0a;
pub const OPCODE_VEC_V128_LOAD32ZERO: OpcodeVec = 0x5c;
pub const OPCODE_VEC_V128_LOAD64ZERO: OpcodeVec = 0x5d;
pub const OPCODE_VEC_V128_STORE: OpcodeVec = 0x0b;
pub const OPCODE_VEC_V128_LOAD8_LANE: OpcodeVec = 0x54;
pub const OPCODE_VEC_V128_LOAD16_LANE: OpcodeVec = 0x55;
pub const OPCODE_VEC_V128_LOAD32_LANE: OpcodeVec = 0x56;
pub const OPCODE_VEC_V128_LOAD64_LANE: OpcodeVec = 0x57;
pub const OPCODE_VEC_V128_STORE8_LANE: OpcodeVec = 0x58;
pub const OPCODE_VEC_V128_STORE16_LANE: OpcodeVec = 0x59;
pub const OPCODE_VEC_V128_STORE32_LANE: OpcodeVec = 0x5a;
pub const OPCODE_VEC_V128_STORE64_LANE: OpcodeVec = 0x5b;
pub const OPCODE_VEC_V128_CONST: OpcodeVec = 0x0c;
pub const OPCODE_VEC_V128I8X16_SHUFFLE: OpcodeVec = 0x0d;
pub const OPCODE_VEC_I8X16_EXTRACT_LANE_S: OpcodeVec = 0x15;
pub const OPCODE_VEC_I8X16_EXTRACT_LANE_U: OpcodeVec = 0x16;
pub const OPCODE_VEC_I8X16_REPLACE_LANE: OpcodeVec = 0x17;
pub const OPCODE_VEC_I16X8_EXTRACT_LANE_S: OpcodeVec = 0x18;
pub const OPCODE_VEC_I16X8_EXTRACT_LANE_U: OpcodeVec = 0x19;
pub const OPCODE_VEC_I16X8_REPLACE_LANE: OpcodeVec = 0x1a;
pub const OPCODE_VEC_I32X4_EXTRACT_LANE: OpcodeVec = 0x1b;
pub const OPCODE_VEC_I32X4_REPLACE_LANE: OpcodeVec = 0x1c;
pub const OPCODE_VEC_I64X2_EXTRACT_LANE: OpcodeVec = 0x1d;
pub const OPCODE_VEC_I64X2_REPLACE_LANE: OpcodeVec = 0x1e;
pub const OPCODE_VEC_F32X4_EXTRACT_LANE: OpcodeVec = 0x1f;
pub const OPCODE_VEC_F32X4_REPLACE_LANE: OpcodeVec = 0x20;
pub const OPCODE_VEC_F64X2_EXTRACT_LANE: OpcodeVec = 0x21;
pub const OPCODE_VEC_F64X2_REPLACE_LANE: OpcodeVec = 0x22;
pub const OPCODE_VEC_I8X16_SWIZZLE: OpcodeVec = 0x0e;
pub const OPCODE_VEC_I8X16_SPLAT: OpcodeVec = 0x0f;
pub const OPCODE_VEC_I16X8_SPLAT: OpcodeVec = 0x10;
pub const OPCODE_VEC_I32X4_SPLAT: OpcodeVec = 0x11;
pub const OPCODE_VEC_I64X2_SPLAT: OpcodeVec = 0x12;
pub const OPCODE_VEC_F32X4_SPLAT: OpcodeVec = 0x13;
pub const OPCODE_VEC_F64X2_SPLAT: OpcodeVec = 0x14;
pub const OPCODE_VEC_I8X16_EQ: OpcodeVec = 0x23;
pub const OPCODE_VEC_I8X16_NE: OpcodeVec = 0x24;
pub const OPCODE_VEC_I8X16_LT_S: OpcodeVec = 0x25;
pub const OPCODE_VEC_I8X16_LT_U: OpcodeVec = 0x26;
pub const OPCODE_VEC_I8X16_GT_S: OpcodeVec = 0x27;
pub const OPCODE_VEC_I8X16_GT_U: OpcodeVec = 0x28;
pub const OPCODE_VEC_I8X16_LE_S: OpcodeVec = 0x29;
pub const OPCODE_VEC_I8X16_LE_U: OpcodeVec = 0x2a;
pub const OPCODE_VEC_I8X16_GE_S: OpcodeVec = 0x2b;
pub const OPCODE_VEC_I8X16_GE_U: OpcodeVec = 0x2c;
pub const OPCODE_VEC_I16X8_EQ: OpcodeVec = 0x2d;
pub const OPCODE_VEC_I16X8_NE: OpcodeVec = 0x2e;
pub const OPCODE_VEC_I16X8_LT_S: OpcodeVec = 0x2f;
pub const OPCODE_VEC_I16X8_LT_U: OpcodeVec = 0x30;
pub const OPCODE_VEC_I16X8_GT_S: OpcodeVec = 0x31;
pub const OPCODE_VEC_I16X8_GT_U: OpcodeVec = 0x32;
pub const OPCODE_VEC_I16X8_LE_S: OpcodeVec = 0x33;
pub const OPCODE_VEC_I16X8_LE_U: OpcodeVec = 0x34;
pub const OPCODE_VEC_I16X8_GE_S: OpcodeVec = 0x35;
pub const OPCODE_VEC_I16X8_GE_U: OpcodeVec = 0x36;
pub const OPCODE_VEC_I32X4_EQ: OpcodeVec = 0x37;
pub const OPCODE_VEC_I32X4_NE: OpcodeVec = 0x38;
pub const OPCODE_VEC_I32X4_LT_S: OpcodeVec = 0x39;
pub const OPCODE_VEC_I32X4_LT_U: OpcodeVec = 0x3a;
pub const OPCODE_VEC_I32X4_GT_S: OpcodeVec = 0x3b;
pub const OPCODE_VEC_I32X4_GT_U: OpcodeVec = 0x3c;
pub const OPCODE_VEC_I32X4_LE_S: OpcodeVec = 0x3d;
pub const OPCODE_VEC_I32X4_LE_U: OpcodeVec = 0x3e;
pub const OPCODE_VEC_I32X4_GE_S: OpcodeVec = 0x3f;
pub const OPCODE_VEC_I32X4_GE_U: OpcodeVec = 0x40;
pub const OPCODE_VEC_I64X2_EQ: OpcodeVec = 0xd6;
pub const OPCODE_VEC_I64X2_NE: OpcodeVec = 0xd7;
pub const OPCODE_VEC_I64X2_LT_S: OpcodeVec = 0xd8;
pub const OPCODE_VEC_I64X2_GT_S: OpcodeVec = 0xd9;
pub const OPCODE_VEC_I64X2_LE_S: OpcodeVec = 0xda;
pub const OPCODE_VEC_I64X2_GE_S: OpcodeVec = 0xdb;
pub const OPCODE_VEC_F32X4_EQ: OpcodeVec = 0x41;
pub const OPCODE_VEC_F32X4_NE: OpcodeVec = 0x42;
pub const OPCODE_VEC_F32X4_LT: OpcodeVec = 0x43;
pub const OPCODE_VEC_F32X4_GT: OpcodeVec = 0x44;
pub const OPCODE_VEC_F32X4_LE: OpcodeVec = 0x45;
pub const OPCODE_VEC_F32X4_GE: OpcodeVec = 0x46;
pub const OPCODE_VEC_F64X2_EQ: OpcodeVec = 0x47;
pub const OPCODE_VEC_F64X2_NE: OpcodeVec = 0x48;
pub const OPCODE_VEC_F64X2_LT: OpcodeVec = 0x49;
pub const OPCODE_VEC_F64X2_GT: OpcodeVec = 0x4a;
pub const OPCODE_VEC_F64X2_LE: OpcodeVec = 0x4b;
pub const OPCODE_VEC_F64X2_GE: OpcodeVec = 0x4c;
pub const OPCODE_VEC_V128_NOT: OpcodeVec = 0x4d;
pub const OPCODE_VEC_V128_AND: OpcodeVec = 0x4e;
pub const OPCODE_VEC_V128_AND_NOT: OpcodeVec = 0x4f;
pub const OPCODE_VEC_V128_OR: OpcodeVec = 0x50;
pub const OPCODE_VEC_V128_XOR: OpcodeVec = 0x51;
pub const OPCODE_VEC_V128_BITSELECT: OpcodeVec = 0x52;
pub const OPCODE_VEC_V128_ANY_TRUE: OpcodeVec = 0x53;
pub const OPCODE_VEC_I8X16_ABS: OpcodeVec = 0x60;
pub const OPCODE_VEC_I8X16_NEG: OpcodeVec = 0x61;
pub const OPCODE_VEC_I8X16_POPCNT: OpcodeVec = 0x62;
pub const OPCODE_VEC_I8X16_ALL_TRUE: OpcodeVec = 0x63;
pub const OPCODE_VEC_I8X16_BIT_MASK: OpcodeVec = 0x64;
pub const OPCODE_VEC_I8X16_NARROW_I16X8_S: OpcodeVec = 0x65;
pub const OPCODE_VEC_I8X16_NARROW_I16X8_U: OpcodeVec = 0x66;
pub const OPCODE_VEC_I8X16_SHL: OpcodeVec = 0x6b;
pub const OPCODE_VEC_I8X16_SHR_S: OpcodeVec = 0x6c;
pub const OPCODE_VEC_I8X16_SHR_U: OpcodeVec = 0x6d;
pub const OPCODE_VEC_I8X16_ADD: OpcodeVec = 0x6e;
pub const OPCODE_VEC_I8X16_ADD_SAT_S: OpcodeVec = 0x6f;
pub const OPCODE_VEC_I8X16_ADD_SAT_U: OpcodeVec = 0x70;
pub const OPCODE_VEC_I8X16_SUB: OpcodeVec = 0x71;
pub const OPCODE_VEC_I8X16_SUB_SAT_S: OpcodeVec = 0x72;
pub const OPCODE_VEC_I8X16_SUB_SAT_U: OpcodeVec = 0x73;
pub const OPCODE_VEC_I8X16_MIN_S: OpcodeVec = 0x76;
pub const OPCODE_VEC_I8X16_MIN_U: OpcodeVec = 0x77;
pub const OPCODE_VEC_I8X16_MAX_S: OpcodeVec = 0x78;
pub const OPCODE_VEC_I8X16_MAX_U: OpcodeVec = 0x79;
pub const OPCODE_VEC_I8X16_AVGR_U: OpcodeVec = 0x7b;
pub const OPCODE_VEC_I16X8_EXTADD_PAIRWISE_I8X16_S: OpcodeVec = 0x7c;
pub const OPCODE_VEC_I16X8_EXTADD_PAIRWISE_I8X16_U: OpcodeVec = 0x7d;
pub const OPCODE_VEC_I16X8_ABS: OpcodeVec = 0x80;
pub const OPCODE_VEC_I16X8_NEG: OpcodeVec = 0x81;
pub const OPCODE_VEC_I16X8_Q15MULR_SAT_S: OpcodeVec = 0x82;
pub const OPCODE_VEC_I16X8_ALL_TRUE: OpcodeVec = 0x83;
pub const OPCODE_VEC_I16X8_BIT_MASK: OpcodeVec = 0x84;
pub const OPCODE_VEC_I16X8_NARROW_I32X4_S: OpcodeVec = 0x85;
pub const OPCODE_VEC_I16X8_NARROW_I32X4_U: OpcodeVec = 0x86;
pub const OPCODE_VEC_I16X8_EXTEND_LOW_I8X16_S: OpcodeVec = 0x87;
pub const OPCODE_VEC_I16X8_EXTEND_HIGH_I8X16_S: OpcodeVec = 0x88;
pub const OPCODE_VEC_I16X8_EXTEND_LOW_I8X16_U: OpcodeVec = 0x89;
pub const OPCODE_VEC_I16X8_EXTEND_HIGH_I8X16_U: OpcodeVec = 0x8a;
pub const OPCODE_VEC_I16X8_SHL: OpcodeVec = 0x8b;
pub const OPCODE_VEC_I16X8_SHR_S: OpcodeVec = 0x8c;
pub const OPCODE_VEC_I16X8_SHR_U: OpcodeVec = 0x8d;
pub const OPCODE_VEC_I16X8_ADD: OpcodeVec = 0x8e;
pub const OPCODE_VEC_I16X8_ADD_SAT_S: OpcodeVec = 0x8f;
pub const OPCODE_VEC_I16X8_ADD_SAT_U: OpcodeVec = 0x90;
pub const OPCODE_VEC_I16X8_SUB: OpcodeVec = 0x91;
pub const OPCODE_VEC_I16X8_SUB_SAT_S: OpcodeVec = 0x92;
pub const OPCODE_VEC_I16X8_SUB_SAT_U: OpcodeVec = 0x93;
pub const OPCODE_VEC_I16X8_MUL: OpcodeVec = 0x95;
pub const OPCODE_VEC_I16X8_MIN_S: OpcodeVec = 0x96;
pub const OPCODE_VEC_I16X8_MIN_U: OpcodeVec = 0x97;
pub const OPCODE_VEC_I16X8_MAX_S: OpcodeVec = 0x98;
pub const OPCODE_VEC_I16X8_MAX_U: OpcodeVec = 0x99;
pub const OPCODE_VEC_I16X8_AVGR_U: OpcodeVec = 0x9b;
pub const OPCODE_VEC_I16X8_EXT_MUL_LOW_I8X16_S: OpcodeVec = 0x9c;
pub const OPCODE_VEC_I16X8_EXT_MUL_HIGH_I8X16_S: OpcodeVec = 0x9d;
pub const OPCODE_VEC_I16X8_EXT_MUL_LOW_I8X16_U: OpcodeVec = 0x9e;
pub const OPCODE_VEC_I16X8_EXT_MUL_HIGH_I8X16_U: OpcodeVec = 0x9f;
pub const OPCODE_VEC_I32X4_EXTADD_PAIRWISE_I16X8_S: OpcodeVec = 0x7e;
pub const OPCODE_VEC_I32X4_EXTADD_PAIRWISE_I16X8_U: OpcodeVec = 0x7f;
pub const OPCODE_VEC_I32X4_ABS: OpcodeVec = 0xa0;
pub const OPCODE_VEC_I32X4_NEG: OpcodeVec = 0xa1;
pub const OPCODE_VEC_I32X4_ALL_TRUE: OpcodeVec = 0xa3;
pub const OPCODE_VEC_I32X4_BIT_MASK: OpcodeVec = 0xa4;
pub const OPCODE_VEC_I32X4_EXTEND_LOW_I16X8_S: OpcodeVec = 0xa7;
pub const OPCODE_VEC_I32X4_EXTEND_HIGH_I16X8_S: OpcodeVec = 0xa8;
pub const OPCODE_VEC_I32X4_EXTEND_LOW_I16X8_U: OpcodeVec = 0xa9;
pub const OPCODE_VEC_I32X4_EXTEND_HIGH_I16X8_U: OpcodeVec = 0xaa;
pub const OPCODE_VEC_I32X4_SHL: OpcodeVec = 0xab;
pub const OPCODE_VEC_I32X4_SHR_S: OpcodeVec = 0xac;
pub const OPCODE_VEC_I32X4_SHR_U: OpcodeVec = 0xad;
pub const OPCODE_VEC_I32X4_ADD: OpcodeVec = 0xae;
pub const OPCODE_VEC_I32X4_SUB: OpcodeVec = 0xb1;
pub const OPCODE_VEC_I32X4_MUL: OpcodeVec = 0xb5;
pub const OPCODE_VEC_I32X4_MIN_S: OpcodeVec = 0xb6;
pub const OPCODE_VEC_I32X4_MIN_U: OpcodeVec = 0xb7;
pub const OPCODE_VEC_I32X4_MAX_S: OpcodeVec = 0xb8;
pub const OPCODE_VEC_I32X4_MAX_U: OpcodeVec = 0xb9;
pub const OPCODE_VEC_I32X4_DOT_I16X8_S: OpcodeVec = 0xba;
pub const OPCODE_VEC_I32X4_EXT_MUL_LOW_I16X8_S: OpcodeVec = 0xbc;
pub const OPCODE_VEC_I32X4_EXT_MUL_HIGH_I16X8_S: OpcodeVec = 0xbd;
pub const OPCODE_VEC_I32X4_EXT_MUL_LOW_I16X8_U: OpcodeVec = 0xbe;
pub const OPCODE_VEC_I32X4_EXT_MUL_HIGH_I16X8_U: OpcodeVec = 0xbf;
pub const OPCODE_VEC_I64X2_ABS: OpcodeVec = 0xc0;
pub const OPCODE_VEC_I64X2_NEG: OpcodeVec = 0xc1;
pub const OPCODE_VEC_I64X2_ALL_TRUE: OpcodeVec = 0xc3;
pub const OPCODE_VEC_I64X2_BIT_MASK: OpcodeVec = 0xc4;
pub const OPCODE_VEC_I64X2_EXTEND_LOW_I32X4_S: OpcodeVec = 0xc7;
pub const OPCODE_VEC_I64X2_EXTEND_HIGH_I32X4_S: OpcodeVec = 0xc8;
pub const OPCODE_VEC_I64X2_EXTEND_LOW_I32X4_U: OpcodeVec = 0xc9;
pub const OPCODE_VEC_I64X2_EXTEND_HIGH_I32X4_U: OpcodeVec = 0xca;
pub const OPCODE_VEC_I64X2_SHL: OpcodeVec = 0xcb;
pub const OPCODE_VEC_I64X2_SHR_S: OpcodeVec = 0xcc;
pub const OPCODE_VEC_I64X2_SHR_U: OpcodeVec = 0xcd;
pub const OPCODE_VEC_I64X2_ADD: OpcodeVec = 0xce;
pub const OPCODE_VEC_I64X2_SUB: OpcodeVec = 0xd1;
pub const OPCODE_VEC_I64X2_MUL: OpcodeVec = 0xd5;
pub const OPCODE_VEC_I64X2_EXT_MUL_LOW_I32X4_S: OpcodeVec = 0xdc;
pub const OPCODE_VEC_I64X2_EXT_MUL_HIGH_I32X4_S: OpcodeVec = 0xdd;
pub const OPCODE_VEC_I64X2_EXT_MUL_LOW_I32X4_U: OpcodeVec = 0xde;
pub const OPCODE_VEC_I64X2_EXT_MUL_HIGH_I32X4_U: OpcodeVec = 0xdf;
pub const OPCODE_VEC_F32X4_CEIL: OpcodeVec = 0x67;
pub const OPCODE_VEC_F32X4_FLOOR: OpcodeVec = 0x68;
pub const OPCODE_VEC_F32X4_TRUNC: OpcodeVec = 0x69;
pub const OPCODE_VEC_F32X4_NEAREST: OpcodeVec = 0x6a;
pub const OPCODE_VEC_F32X4_ABS: OpcodeVec = 0xe0;
pub const OPCODE_VEC_F32X4_NEG: OpcodeVec = 0xe1;
pub const OPCODE_VEC_F32X4_SQRT: OpcodeVec = 0xe3;
pub const OPCODE_VEC_F32X4_ADD: OpcodeVec = 0xe4;
pub const OPCODE_VEC_F32X4_SUB: OpcodeVec = 0xe5;
pub const OPCODE_VEC_F32X4_MUL: OpcodeVec = 0xe6;
pub const OPCODE_VEC_F32X4_DIV: OpcodeVec = 0xe7;
pub const OPCODE_VEC_F32X4_MIN: OpcodeVec = 0xe8;
pub const OPCODE_VEC_F32X4_MAX: OpcodeVec = 0xe9;
pub const OPCODE_VEC_F32X4_PMIN: OpcodeVec = 0xea;
pub const OPCODE_VEC_F32X4_PMAX: OpcodeVec = 0xeb;
pub const OPCODE_VEC_F64X2_CEIL: OpcodeVec = 0x74;
pub const OPCODE_VEC_F64X2_FLOOR: OpcodeVec = 0x75;
pub const OPCODE_VEC_F64X2_TRUNC: OpcodeVec = 0x7a;
pub const OPCODE_VEC_F64X2_NEAREST: OpcodeVec = 0x94;
pub const OPCODE_VEC_F64X2_ABS: OpcodeVec = 0xec;
pub const OPCODE_VEC_F64X2_NEG: OpcodeVec = 0xed;
pub const OPCODE_VEC_F64X2_SQRT: OpcodeVec = 0xef;
pub const OPCODE_VEC_F64X2_ADD: OpcodeVec = 0xf0;
pub const OPCODE_VEC_F64X2_SUB: OpcodeVec = 0xf1;
pub const OPCODE_VEC_F64X2_MUL: OpcodeVec = 0xf2;
pub const OPCODE_VEC_F64X2_DIV: OpcodeVec = 0xf3;
pub const OPCODE_VEC_F64X2_MIN: OpcodeVec = 0xf4;
pub const OPCODE_VEC_F64X2_MAX: OpcodeVec = 0xf5;
pub const OPCODE_VEC_F64X2_PMIN: OpcodeVec = 0xf6;
pub const OPCODE_VEC_F64X2_PMAX: OpcodeVec = 0xf7;
pub const OPCODE_VEC_I32X4_TRUNC_SAT_F32X4_S: OpcodeVec = 0xf8;
pub const OPCODE_VEC_I32X4_TRUNC_SAT_F32X4_U: OpcodeVec = 0xf9;
pub const OPCODE_VEC_F32X4_CONVERT_I32X4_S: OpcodeVec = 0xfa;
pub const OPCODE_VEC_F32X4_CONVERT_I32X4_U: OpcodeVec = 0xfb;
pub const OPCODE_VEC_I32X4_TRUNC_SAT_F64X2_S_ZERO: OpcodeVec = 0xfc;
pub const OPCODE_VEC_I32X4_TRUNC_SAT_F64X2_U_ZERO: OpcodeVec = 0xfd;
pub const OPCODE_VEC_F64X2_CONVERT_LOW_I32X4_S: OpcodeVec = 0xfe;
pub const OPCODE_VEC_F64X2_CONVERT_LOW_I32X4_U: OpcodeVec = 0xff;
pub const OPCODE_VEC_F32X4_DEMOTE_F64X2_ZERO: OpcodeVec = 0x5e;
pub const OPCODE_VEC_F64X2_PROMOTE_LOW_F32X4_ZERO: OpcodeVec = 0x5f;

// OpcodeAtomic constants mirrored from internal/wasm/instruction.go.
pub const OPCODE_ATOMIC_MEMORY_NOTIFY: OpcodeAtomic = 0x00;
pub const OPCODE_ATOMIC_MEMORY_WAIT32: OpcodeAtomic = 0x01;
pub const OPCODE_ATOMIC_MEMORY_WAIT64: OpcodeAtomic = 0x02;
pub const OPCODE_ATOMIC_FENCE: OpcodeAtomic = 0x03;
pub const OPCODE_ATOMIC_I32_LOAD: OpcodeAtomic = 0x10;
pub const OPCODE_ATOMIC_I64_LOAD: OpcodeAtomic = 0x11;
pub const OPCODE_ATOMIC_I32_LOAD8_U: OpcodeAtomic = 0x12;
pub const OPCODE_ATOMIC_I32_LOAD16_U: OpcodeAtomic = 0x13;
pub const OPCODE_ATOMIC_I64_LOAD8_U: OpcodeAtomic = 0x14;
pub const OPCODE_ATOMIC_I64_LOAD16_U: OpcodeAtomic = 0x15;
pub const OPCODE_ATOMIC_I64_LOAD32_U: OpcodeAtomic = 0x16;
pub const OPCODE_ATOMIC_I32_STORE: OpcodeAtomic = 0x17;
pub const OPCODE_ATOMIC_I64_STORE: OpcodeAtomic = 0x18;
pub const OPCODE_ATOMIC_I32_STORE8: OpcodeAtomic = 0x19;
pub const OPCODE_ATOMIC_I32_STORE16: OpcodeAtomic = 0x1a;
pub const OPCODE_ATOMIC_I64_STORE8: OpcodeAtomic = 0x1b;
pub const OPCODE_ATOMIC_I64_STORE16: OpcodeAtomic = 0x1c;
pub const OPCODE_ATOMIC_I64_STORE32: OpcodeAtomic = 0x1d;
pub const OPCODE_ATOMIC_I32_RMW_ADD: OpcodeAtomic = 0x1e;
pub const OPCODE_ATOMIC_I64_RMW_ADD: OpcodeAtomic = 0x1f;
pub const OPCODE_ATOMIC_I32_RMW8_ADD_U: OpcodeAtomic = 0x20;
pub const OPCODE_ATOMIC_I32_RMW16_ADD_U: OpcodeAtomic = 0x21;
pub const OPCODE_ATOMIC_I64_RMW8_ADD_U: OpcodeAtomic = 0x22;
pub const OPCODE_ATOMIC_I64_RMW16_ADD_U: OpcodeAtomic = 0x23;
pub const OPCODE_ATOMIC_I64_RMW32_ADD_U: OpcodeAtomic = 0x24;
pub const OPCODE_ATOMIC_I32_RMW_SUB: OpcodeAtomic = 0x25;
pub const OPCODE_ATOMIC_I64_RMW_SUB: OpcodeAtomic = 0x26;
pub const OPCODE_ATOMIC_I32_RMW8_SUB_U: OpcodeAtomic = 0x27;
pub const OPCODE_ATOMIC_I32_RMW16_SUB_U: OpcodeAtomic = 0x28;
pub const OPCODE_ATOMIC_I64_RMW8_SUB_U: OpcodeAtomic = 0x29;
pub const OPCODE_ATOMIC_I64_RMW16_SUB_U: OpcodeAtomic = 0x2a;
pub const OPCODE_ATOMIC_I64_RMW32_SUB_U: OpcodeAtomic = 0x2b;
pub const OPCODE_ATOMIC_I32_RMW_AND: OpcodeAtomic = 0x2c;
pub const OPCODE_ATOMIC_I64_RMW_AND: OpcodeAtomic = 0x2d;
pub const OPCODE_ATOMIC_I32_RMW8_AND_U: OpcodeAtomic = 0x2e;
pub const OPCODE_ATOMIC_I32_RMW16_AND_U: OpcodeAtomic = 0x2f;
pub const OPCODE_ATOMIC_I64_RMW8_AND_U: OpcodeAtomic = 0x30;
pub const OPCODE_ATOMIC_I64_RMW16_AND_U: OpcodeAtomic = 0x31;
pub const OPCODE_ATOMIC_I64_RMW32_AND_U: OpcodeAtomic = 0x32;
pub const OPCODE_ATOMIC_I32_RMW_OR: OpcodeAtomic = 0x33;
pub const OPCODE_ATOMIC_I64_RMW_OR: OpcodeAtomic = 0x34;
pub const OPCODE_ATOMIC_I32_RMW8_OR_U: OpcodeAtomic = 0x35;
pub const OPCODE_ATOMIC_I32_RMW16_OR_U: OpcodeAtomic = 0x36;
pub const OPCODE_ATOMIC_I64_RMW8_OR_U: OpcodeAtomic = 0x37;
pub const OPCODE_ATOMIC_I64_RMW16_OR_U: OpcodeAtomic = 0x38;
pub const OPCODE_ATOMIC_I64_RMW32_OR_U: OpcodeAtomic = 0x39;
pub const OPCODE_ATOMIC_I32_RMW_XOR: OpcodeAtomic = 0x3a;
pub const OPCODE_ATOMIC_I64_RMW_XOR: OpcodeAtomic = 0x3b;
pub const OPCODE_ATOMIC_I32_RMW8_XOR_U: OpcodeAtomic = 0x3c;
pub const OPCODE_ATOMIC_I32_RMW16_XOR_U: OpcodeAtomic = 0x3d;
pub const OPCODE_ATOMIC_I64_RMW8_XOR_U: OpcodeAtomic = 0x3e;
pub const OPCODE_ATOMIC_I64_RMW16_XOR_U: OpcodeAtomic = 0x3f;
pub const OPCODE_ATOMIC_I64_RMW32_XOR_U: OpcodeAtomic = 0x40;
pub const OPCODE_ATOMIC_I32_RMW_XCHG: OpcodeAtomic = 0x41;
pub const OPCODE_ATOMIC_I64_RMW_XCHG: OpcodeAtomic = 0x42;
pub const OPCODE_ATOMIC_I32_RMW8_XCHG_U: OpcodeAtomic = 0x43;
pub const OPCODE_ATOMIC_I32_RMW16_XCHG_U: OpcodeAtomic = 0x44;
pub const OPCODE_ATOMIC_I64_RMW8_XCHG_U: OpcodeAtomic = 0x45;
pub const OPCODE_ATOMIC_I64_RMW16_XCHG_U: OpcodeAtomic = 0x46;
pub const OPCODE_ATOMIC_I64_RMW32_XCHG_U: OpcodeAtomic = 0x47;
pub const OPCODE_ATOMIC_I32_RMW_CMPXCHG: OpcodeAtomic = 0x48;
pub const OPCODE_ATOMIC_I64_RMW_CMPXCHG: OpcodeAtomic = 0x49;
pub const OPCODE_ATOMIC_I32_RMW8_CMPXCHG_U: OpcodeAtomic = 0x4a;
pub const OPCODE_ATOMIC_I32_RMW16_CMPXCHG_U: OpcodeAtomic = 0x4b;
pub const OPCODE_ATOMIC_I64_RMW8_CMPXCHG_U: OpcodeAtomic = 0x4c;
pub const OPCODE_ATOMIC_I64_RMW16_CMPXCHG_U: OpcodeAtomic = 0x4d;
pub const OPCODE_ATOMIC_I64_RMW32_CMPXCHG_U: OpcodeAtomic = 0x4e;

// OpcodeTailCall constants mirrored from internal/wasm/instruction.go.
pub const OPCODE_TAIL_CALL_RETURN_CALL: OpcodeTailCall = 0x12;
pub const OPCODE_TAIL_CALL_RETURN_CALL_INDIRECT: OpcodeTailCall = 0x13;

pub const fn instruction_name(opcode: Opcode) -> &'static str {
    match opcode {
        OPCODE_UNREACHABLE => "unreachable",
        OPCODE_NOP => "nop",
        OPCODE_BLOCK => "block",
        OPCODE_LOOP => "loop",
        OPCODE_IF => "if",
        OPCODE_ELSE => "else",
        OPCODE_END => "end",
        OPCODE_BR => "br",
        OPCODE_BR_IF => "br_if",
        OPCODE_BR_TABLE => "br_table",
        OPCODE_RETURN => "return",
        OPCODE_CALL => "call",
        OPCODE_CALL_INDIRECT => "call_indirect",
        OPCODE_DROP => "drop",
        OPCODE_SELECT => "select",
        OPCODE_TYPED_SELECT => "typed_select",
        OPCODE_LOCAL_GET => "local.get",
        OPCODE_LOCAL_SET => "local.set",
        OPCODE_LOCAL_TEE => "local.tee",
        OPCODE_GLOBAL_GET => "global.get",
        OPCODE_GLOBAL_SET => "global.set",
        OPCODE_I32_LOAD => "i32.load",
        OPCODE_I64_LOAD => "i64.load",
        OPCODE_F32_LOAD => "f32.load",
        OPCODE_F64_LOAD => "f64.load",
        OPCODE_I32_LOAD8_S => "i32.load8_s",
        OPCODE_I32_LOAD8_U => "i32.load8_u",
        OPCODE_I32_LOAD16_S => "i32.load16_s",
        OPCODE_I32_LOAD16_U => "i32.load16_u",
        OPCODE_I64_LOAD8_S => "i64.load8_s",
        OPCODE_I64_LOAD8_U => "i64.load8_u",
        OPCODE_I64_LOAD16_S => "i64.load16_s",
        OPCODE_I64_LOAD16_U => "i64.load16_u",
        OPCODE_I64_LOAD32_S => "i64.load32_s",
        OPCODE_I64_LOAD32_U => "i64.load32_u",
        OPCODE_I32_STORE => "i32.store",
        OPCODE_I64_STORE => "i64.store",
        OPCODE_F32_STORE => "f32.store",
        OPCODE_F64_STORE => "f64.store",
        OPCODE_I32_STORE8 => "i32.store8",
        OPCODE_I32_STORE16 => "i32.store16",
        OPCODE_I64_STORE8 => "i64.store8",
        OPCODE_I64_STORE16 => "i64.store16",
        OPCODE_I64_STORE32 => "i64.store32",
        OPCODE_MEMORY_SIZE => "memory.size",
        OPCODE_MEMORY_GROW => "memory.grow",
        OPCODE_I32_CONST => "i32.const",
        OPCODE_I64_CONST => "i64.const",
        OPCODE_F32_CONST => "f32.const",
        OPCODE_F64_CONST => "f64.const",
        OPCODE_I32_EQZ => "i32.eqz",
        OPCODE_I32_EQ => "i32.eq",
        OPCODE_I32_NE => "i32.ne",
        OPCODE_I32_LT_S => "i32.lt_s",
        OPCODE_I32_LT_U => "i32.lt_u",
        OPCODE_I32_GT_S => "i32.gt_s",
        OPCODE_I32_GT_U => "i32.gt_u",
        OPCODE_I32_LE_S => "i32.le_s",
        OPCODE_I32_LE_U => "i32.le_u",
        OPCODE_I32_GE_S => "i32.ge_s",
        OPCODE_I32_GE_U => "i32.ge_u",
        OPCODE_I64_EQZ => "i64.eqz",
        OPCODE_I64_EQ => "i64.eq",
        OPCODE_I64_NE => "i64.ne",
        OPCODE_I64_LT_S => "i64.lt_s",
        OPCODE_I64_LT_U => "i64.lt_u",
        OPCODE_I64_GT_S => "i64.gt_s",
        OPCODE_I64_GT_U => "i64.gt_u",
        OPCODE_I64_LE_S => "i64.le_s",
        OPCODE_I64_LE_U => "i64.le_u",
        OPCODE_I64_GE_S => "i64.ge_s",
        OPCODE_I64_GE_U => "i64.ge_u",
        OPCODE_F32_EQ => "f32.eq",
        OPCODE_F32_NE => "f32.ne",
        OPCODE_F32_LT => "f32.lt",
        OPCODE_F32_GT => "f32.gt",
        OPCODE_F32_LE => "f32.le",
        OPCODE_F32_GE => "f32.ge",
        OPCODE_F64_EQ => "f64.eq",
        OPCODE_F64_NE => "f64.ne",
        OPCODE_F64_LT => "f64.lt",
        OPCODE_F64_GT => "f64.gt",
        OPCODE_F64_LE => "f64.le",
        OPCODE_F64_GE => "f64.ge",
        OPCODE_I32_CLZ => "i32.clz",
        OPCODE_I32_CTZ => "i32.ctz",
        OPCODE_I32_POPCNT => "i32.popcnt",
        OPCODE_I32_ADD => "i32.add",
        OPCODE_I32_SUB => "i32.sub",
        OPCODE_I32_MUL => "i32.mul",
        OPCODE_I32_DIV_S => "i32.div_s",
        OPCODE_I32_DIV_U => "i32.div_u",
        OPCODE_I32_REM_S => "i32.rem_s",
        OPCODE_I32_REM_U => "i32.rem_u",
        OPCODE_I32_AND => "i32.and",
        OPCODE_I32_OR => "i32.or",
        OPCODE_I32_XOR => "i32.xor",
        OPCODE_I32_SHL => "i32.shl",
        OPCODE_I32_SHR_S => "i32.shr_s",
        OPCODE_I32_SHR_U => "i32.shr_u",
        OPCODE_I32_ROTL => "i32.rotl",
        OPCODE_I32_ROTR => "i32.rotr",
        OPCODE_I64_CLZ => "i64.clz",
        OPCODE_I64_CTZ => "i64.ctz",
        OPCODE_I64_POPCNT => "i64.popcnt",
        OPCODE_I64_ADD => "i64.add",
        OPCODE_I64_SUB => "i64.sub",
        OPCODE_I64_MUL => "i64.mul",
        OPCODE_I64_DIV_S => "i64.div_s",
        OPCODE_I64_DIV_U => "i64.div_u",
        OPCODE_I64_REM_S => "i64.rem_s",
        OPCODE_I64_REM_U => "i64.rem_u",
        OPCODE_I64_AND => "i64.and",
        OPCODE_I64_OR => "i64.or",
        OPCODE_I64_XOR => "i64.xor",
        OPCODE_I64_SHL => "i64.shl",
        OPCODE_I64_SHR_S => "i64.shr_s",
        OPCODE_I64_SHR_U => "i64.shr_u",
        OPCODE_I64_ROTL => "i64.rotl",
        OPCODE_I64_ROTR => "i64.rotr",
        OPCODE_F32_ABS => "f32.abs",
        OPCODE_F32_NEG => "f32.neg",
        OPCODE_F32_CEIL => "f32.ceil",
        OPCODE_F32_FLOOR => "f32.floor",
        OPCODE_F32_TRUNC => "f32.trunc",
        OPCODE_F32_NEAREST => "f32.nearest",
        OPCODE_F32_SQRT => "f32.sqrt",
        OPCODE_F32_ADD => "f32.add",
        OPCODE_F32_SUB => "f32.sub",
        OPCODE_F32_MUL => "f32.mul",
        OPCODE_F32_DIV => "f32.div",
        OPCODE_F32_MIN => "f32.min",
        OPCODE_F32_MAX => "f32.max",
        OPCODE_F32_COPYSIGN => "f32.copysign",
        OPCODE_F64_ABS => "f64.abs",
        OPCODE_F64_NEG => "f64.neg",
        OPCODE_F64_CEIL => "f64.ceil",
        OPCODE_F64_FLOOR => "f64.floor",
        OPCODE_F64_TRUNC => "f64.trunc",
        OPCODE_F64_NEAREST => "f64.nearest",
        OPCODE_F64_SQRT => "f64.sqrt",
        OPCODE_F64_ADD => "f64.add",
        OPCODE_F64_SUB => "f64.sub",
        OPCODE_F64_MUL => "f64.mul",
        OPCODE_F64_DIV => "f64.div",
        OPCODE_F64_MIN => "f64.min",
        OPCODE_F64_MAX => "f64.max",
        OPCODE_F64_COPYSIGN => "f64.copysign",
        OPCODE_I32_WRAP_I64 => "i32.wrap_i64",
        OPCODE_I32_TRUNC_F32_S => "i32.trunc_f32_s",
        OPCODE_I32_TRUNC_F32_U => "i32.trunc_f32_u",
        OPCODE_I32_TRUNC_F64_S => "i32.trunc_f64_s",
        OPCODE_I32_TRUNC_F64_U => "i32.trunc_f64_u",
        OPCODE_I64_EXTEND_I32_S => "i64.extend_i32_s",
        OPCODE_I64_EXTEND_I32_U => "i64.extend_i32_u",
        OPCODE_I64_TRUNC_F32_S => "i64.trunc_f32_s",
        OPCODE_I64_TRUNC_F32_U => "i64.trunc_f32_u",
        OPCODE_I64_TRUNC_F64_S => "i64.trunc_f64_s",
        OPCODE_I64_TRUNC_F64_U => "i64.trunc_f64_u",
        OPCODE_F32_CONVERT_I32_S => "f32.convert_i32_s",
        OPCODE_F32_CONVERT_I32_U => "f32.convert_i32_u",
        OPCODE_F32_CONVERT_I64_S => "f32.convert_i64_s",
        OPCODE_F32_CONVERT_I64_U => "f32.convert_i64u",
        OPCODE_F32_DEMOTE_F64 => "f32.demote_f64",
        OPCODE_F64_CONVERT_I32_S => "f64.convert_i32_s",
        OPCODE_F64_CONVERT_I32_U => "f64.convert_i32_u",
        OPCODE_F64_CONVERT_I64_S => "f64.convert_i64_s",
        OPCODE_F64_CONVERT_I64_U => "f64.convert_i64_u",
        OPCODE_F64_PROMOTE_F32 => "f64.promote_f32",
        OPCODE_I32_REINTERPRET_F32 => "i32.reinterpret_f32",
        OPCODE_I64_REINTERPRET_F64 => "i64.reinterpret_f64",
        OPCODE_F32_REINTERPRET_I32 => "f32.reinterpret_i32",
        OPCODE_F64_REINTERPRET_I64 => "f64.reinterpret_i64",
        OPCODE_REF_NULL => "ref.null",
        OPCODE_REF_IS_NULL => "ref.is_null",
        OPCODE_REF_FUNC => "ref.func",
        OPCODE_TABLE_GET => "table.get",
        OPCODE_TABLE_SET => "table.set",
        OPCODE_I32_EXTEND8_S => "i32.extend8_s",
        OPCODE_I32_EXTEND16_S => "i32.extend16_s",
        OPCODE_I64_EXTEND8_S => "i64.extend8_s",
        OPCODE_I64_EXTEND16_S => "i64.extend16_s",
        OPCODE_I64_EXTEND32_S => "i64.extend32_s",
        OPCODE_MISC_PREFIX => "misc_prefix",
        OPCODE_VEC_PREFIX => "vector_prefix",
        _ => "",
    }
}

pub const fn misc_instruction_name(opcode: OpcodeMisc) -> &'static str {
    match opcode {
        OPCODE_MISC_I32_TRUNC_SAT_F32_S => "i32.trunc_sat_f32_s",
        OPCODE_MISC_I32_TRUNC_SAT_F32_U => "i32.trunc_sat_f32_u",
        OPCODE_MISC_I32_TRUNC_SAT_F64_S => "i32.trunc_sat_f64_s",
        OPCODE_MISC_I32_TRUNC_SAT_F64_U => "i32.trunc_sat_f64_u",
        OPCODE_MISC_I64_TRUNC_SAT_F32_S => "i64.trunc_sat_f32_s",
        OPCODE_MISC_I64_TRUNC_SAT_F32_U => "i64.trunc_sat_f32_u",
        OPCODE_MISC_I64_TRUNC_SAT_F64_S => "i64.trunc_sat_f64_s",
        OPCODE_MISC_I64_TRUNC_SAT_F64_U => "i64.trunc_sat_f64_u",
        OPCODE_MISC_MEMORY_INIT => "memory.init",
        OPCODE_MISC_DATA_DROP => "data.drop",
        OPCODE_MISC_MEMORY_COPY => "memory.copy",
        OPCODE_MISC_MEMORY_FILL => "memory.fill",
        OPCODE_MISC_TABLE_INIT => "table.init",
        OPCODE_MISC_ELEM_DROP => "elem.drop",
        OPCODE_MISC_TABLE_COPY => "table.copy",
        OPCODE_MISC_TABLE_GROW => "table.grow",
        OPCODE_MISC_TABLE_SIZE => "table.size",
        OPCODE_MISC_TABLE_FILL => "table.fill",
        _ => "",
    }
}

pub const fn vector_instruction_name(opcode: OpcodeVec) -> &'static str {
    match opcode {
        OPCODE_VEC_V128_LOAD => "v128.load",
        OPCODE_VEC_V128_LOAD8X8S => "v128.load8x8_s",
        OPCODE_VEC_V128_LOAD8X8U => "v128.load8x8_u",
        OPCODE_VEC_V128_LOAD16X4S => "v128.load16x4_s",
        OPCODE_VEC_V128_LOAD16X4U => "v128.load16x4_u",
        OPCODE_VEC_V128_LOAD32X2S => "v128.load32x2_s",
        OPCODE_VEC_V128_LOAD32X2U => "v128.load32x2_u",
        OPCODE_VEC_V128_LOAD8_SPLAT => "v128.load8_splat",
        OPCODE_VEC_V128_LOAD16_SPLAT => "v128.load16_splat",
        OPCODE_VEC_V128_LOAD32_SPLAT => "v128.load32_splat",
        OPCODE_VEC_V128_LOAD64_SPLAT => "v128.load64_splat",
        OPCODE_VEC_V128_LOAD32ZERO => "v128.load32_zero",
        OPCODE_VEC_V128_LOAD64ZERO => "v128.load64_zero",
        OPCODE_VEC_V128_STORE => "v128.store",
        OPCODE_VEC_V128_LOAD8_LANE => "v128.load8_lane",
        OPCODE_VEC_V128_LOAD16_LANE => "v128.load16_lane",
        OPCODE_VEC_V128_LOAD32_LANE => "v128.load32_lane",
        OPCODE_VEC_V128_LOAD64_LANE => "v128.load64_lane",
        OPCODE_VEC_V128_STORE8_LANE => "v128.store8_lane",
        OPCODE_VEC_V128_STORE16_LANE => "v128.store16_lane",
        OPCODE_VEC_V128_STORE32_LANE => "v128.store32_lane",
        OPCODE_VEC_V128_STORE64_LANE => "v128.store64_lane",
        OPCODE_VEC_V128_CONST => "v128.const",
        OPCODE_VEC_V128I8X16_SHUFFLE => "v128.shuffle",
        OPCODE_VEC_I8X16_EXTRACT_LANE_S => "i8x16.extract_lane_s",
        OPCODE_VEC_I8X16_EXTRACT_LANE_U => "i8x16.extract_lane_u",
        OPCODE_VEC_I8X16_REPLACE_LANE => "i8x16.replace_lane",
        OPCODE_VEC_I16X8_EXTRACT_LANE_S => "i16x8.extract_lane_s",
        OPCODE_VEC_I16X8_EXTRACT_LANE_U => "i16x8.extract_lane_u",
        OPCODE_VEC_I16X8_REPLACE_LANE => "i16x8.replace_lane",
        OPCODE_VEC_I32X4_EXTRACT_LANE => "i32x4.extract_lane",
        OPCODE_VEC_I32X4_REPLACE_LANE => "i32x4.replace_lane",
        OPCODE_VEC_I64X2_EXTRACT_LANE => "i64x2.extract_lane",
        OPCODE_VEC_I64X2_REPLACE_LANE => "i64x2.replace_lane",
        OPCODE_VEC_F32X4_EXTRACT_LANE => "f32x4.extract_lane",
        OPCODE_VEC_F32X4_REPLACE_LANE => "f32x4.replace_lane",
        OPCODE_VEC_F64X2_EXTRACT_LANE => "f64x2.extract_lane",
        OPCODE_VEC_F64X2_REPLACE_LANE => "f64x2.replace_lane",
        OPCODE_VEC_I8X16_SWIZZLE => "i8x16.swizzle",
        OPCODE_VEC_I8X16_SPLAT => "i8x16.splat",
        OPCODE_VEC_I16X8_SPLAT => "i16x8.splat",
        OPCODE_VEC_I32X4_SPLAT => "i32x4.splat",
        OPCODE_VEC_I64X2_SPLAT => "i64x2.splat",
        OPCODE_VEC_F32X4_SPLAT => "f32x4.splat",
        OPCODE_VEC_F64X2_SPLAT => "f64x2.splat",
        OPCODE_VEC_I8X16_EQ => "i8x16.eq",
        OPCODE_VEC_I8X16_NE => "i8x16.ne",
        OPCODE_VEC_I8X16_LT_S => "i8x16.lt_s",
        OPCODE_VEC_I8X16_LT_U => "i8x16.lt_u",
        OPCODE_VEC_I8X16_GT_S => "i8x16.gt_s",
        OPCODE_VEC_I8X16_GT_U => "i8x16.gt_u",
        OPCODE_VEC_I8X16_LE_S => "i8x16.le_s",
        OPCODE_VEC_I8X16_LE_U => "i8x16.le_u",
        OPCODE_VEC_I8X16_GE_S => "i8x16.ge_s",
        OPCODE_VEC_I8X16_GE_U => "i8x16.ge_u",
        OPCODE_VEC_I16X8_EQ => "i16x8.eq",
        OPCODE_VEC_I16X8_NE => "i16x8.ne",
        OPCODE_VEC_I16X8_LT_S => "i16x8.lt_s",
        OPCODE_VEC_I16X8_LT_U => "i16x8.lt_u",
        OPCODE_VEC_I16X8_GT_S => "i16x8.gt_s",
        OPCODE_VEC_I16X8_GT_U => "i16x8.gt_u",
        OPCODE_VEC_I16X8_LE_S => "i16x8.le_s",
        OPCODE_VEC_I16X8_LE_U => "i16x8.le_u",
        OPCODE_VEC_I16X8_GE_S => "i16x8.ge_s",
        OPCODE_VEC_I16X8_GE_U => "i16x8.ge_u",
        OPCODE_VEC_I32X4_EQ => "i32x4.eq",
        OPCODE_VEC_I32X4_NE => "i32x4.ne",
        OPCODE_VEC_I32X4_LT_S => "i32x4.lt_s",
        OPCODE_VEC_I32X4_LT_U => "i32x4.lt_u",
        OPCODE_VEC_I32X4_GT_S => "i32x4.gt_s",
        OPCODE_VEC_I32X4_GT_U => "i32x4.gt_u",
        OPCODE_VEC_I32X4_LE_S => "i32x4.le_s",
        OPCODE_VEC_I32X4_LE_U => "i32x4.le_u",
        OPCODE_VEC_I32X4_GE_S => "i32x4.ge_s",
        OPCODE_VEC_I32X4_GE_U => "i32x4.ge_u",
        OPCODE_VEC_I64X2_EQ => "i64x2.eq",
        OPCODE_VEC_I64X2_NE => "i64x2.ne",
        OPCODE_VEC_I64X2_LT_S => "i64x2.lt",
        OPCODE_VEC_I64X2_GT_S => "i64x2.gt",
        OPCODE_VEC_I64X2_LE_S => "i64x2.le",
        OPCODE_VEC_I64X2_GE_S => "i64x2.ge",
        OPCODE_VEC_F32X4_EQ => "f32x4.eq",
        OPCODE_VEC_F32X4_NE => "f32x4.ne",
        OPCODE_VEC_F32X4_LT => "f32x4.lt",
        OPCODE_VEC_F32X4_GT => "f32x4.gt",
        OPCODE_VEC_F32X4_LE => "f32x4.le",
        OPCODE_VEC_F32X4_GE => "f32x4.ge",
        OPCODE_VEC_F64X2_EQ => "f64x2.eq",
        OPCODE_VEC_F64X2_NE => "f64x2.ne",
        OPCODE_VEC_F64X2_LT => "f64x2.lt",
        OPCODE_VEC_F64X2_GT => "f64x2.gt",
        OPCODE_VEC_F64X2_LE => "f64x2.le",
        OPCODE_VEC_F64X2_GE => "f64x2.ge",
        OPCODE_VEC_V128_NOT => "v128.not",
        OPCODE_VEC_V128_AND => "v128.and",
        OPCODE_VEC_V128_AND_NOT => "v128.andnot",
        OPCODE_VEC_V128_OR => "v128.or",
        OPCODE_VEC_V128_XOR => "v128.xor",
        OPCODE_VEC_V128_BITSELECT => "v128.bitselect",
        OPCODE_VEC_V128_ANY_TRUE => "v128.any_true",
        OPCODE_VEC_I8X16_ABS => "i8x16.abs",
        OPCODE_VEC_I8X16_NEG => "i8x16.neg",
        OPCODE_VEC_I8X16_POPCNT => "i8x16.popcnt",
        OPCODE_VEC_I8X16_ALL_TRUE => "i8x16.all_true",
        OPCODE_VEC_I8X16_BIT_MASK => "i8x16.bitmask",
        OPCODE_VEC_I8X16_NARROW_I16X8_S => "i8x16.narrow_i16x8_s",
        OPCODE_VEC_I8X16_NARROW_I16X8_U => "i8x16.narrow_i16x8_u",
        OPCODE_VEC_I8X16_SHL => "i8x16.shl",
        OPCODE_VEC_I8X16_SHR_S => "i8x16.shr_s",
        OPCODE_VEC_I8X16_SHR_U => "i8x16.shr_u",
        OPCODE_VEC_I8X16_ADD => "i8x16.add",
        OPCODE_VEC_I8X16_ADD_SAT_S => "i8x16.add_sat_s",
        OPCODE_VEC_I8X16_ADD_SAT_U => "i8x16.add_sat_u",
        OPCODE_VEC_I8X16_SUB => "i8x16.sub",
        OPCODE_VEC_I8X16_SUB_SAT_S => "i8x16.sub_s",
        OPCODE_VEC_I8X16_SUB_SAT_U => "i8x16.sub_u",
        OPCODE_VEC_I8X16_MIN_S => "i8x16.min_s",
        OPCODE_VEC_I8X16_MIN_U => "i8x16.min_u",
        OPCODE_VEC_I8X16_MAX_S => "i8x16.max_s",
        OPCODE_VEC_I8X16_MAX_U => "i8x16.max_u",
        OPCODE_VEC_I8X16_AVGR_U => "i8x16.avgr_u",
        OPCODE_VEC_I16X8_EXTADD_PAIRWISE_I8X16_S => "i16x8.extadd_pairwise_i8x16_s",
        OPCODE_VEC_I16X8_EXTADD_PAIRWISE_I8X16_U => "i16x8.extadd_pairwise_i8x16_u",
        OPCODE_VEC_I16X8_ABS => "i16x8.abs",
        OPCODE_VEC_I16X8_NEG => "i16x8.neg",
        OPCODE_VEC_I16X8_Q15MULR_SAT_S => "i16x8.q15mulr_sat_s",
        OPCODE_VEC_I16X8_ALL_TRUE => "i16x8.all_true",
        OPCODE_VEC_I16X8_BIT_MASK => "i16x8.bitmask",
        OPCODE_VEC_I16X8_NARROW_I32X4_S => "i16x8.narrow_i32x4_s",
        OPCODE_VEC_I16X8_NARROW_I32X4_U => "i16x8.narrow_i32x4_u",
        OPCODE_VEC_I16X8_EXTEND_LOW_I8X16_S => "i16x8.extend_low_i8x16_s",
        OPCODE_VEC_I16X8_EXTEND_HIGH_I8X16_S => "i16x8.extend_high_i8x16_s",
        OPCODE_VEC_I16X8_EXTEND_LOW_I8X16_U => "i16x8.extend_low_i8x16_u",
        OPCODE_VEC_I16X8_EXTEND_HIGH_I8X16_U => "i16x8.extend_high_i8x16_u",
        OPCODE_VEC_I16X8_SHL => "i16x8.shl",
        OPCODE_VEC_I16X8_SHR_S => "i16x8.shr_s",
        OPCODE_VEC_I16X8_SHR_U => "i16x8.shr_u",
        OPCODE_VEC_I16X8_ADD => "i16x8.add",
        OPCODE_VEC_I16X8_ADD_SAT_S => "i16x8.add_sat_s",
        OPCODE_VEC_I16X8_ADD_SAT_U => "i16x8.add_sat_u",
        OPCODE_VEC_I16X8_SUB => "i16x8.sub",
        OPCODE_VEC_I16X8_SUB_SAT_S => "i16x8.sub_sat_s",
        OPCODE_VEC_I16X8_SUB_SAT_U => "i16x8.sub_sat_u",
        OPCODE_VEC_I16X8_MUL => "i16x8.mul",
        OPCODE_VEC_I16X8_MIN_S => "i16x8.min_s",
        OPCODE_VEC_I16X8_MIN_U => "i16x8.min_u",
        OPCODE_VEC_I16X8_MAX_S => "i16x8.max_s",
        OPCODE_VEC_I16X8_MAX_U => "i16x8.max_u",
        OPCODE_VEC_I16X8_AVGR_U => "i16x8.avgr_u",
        OPCODE_VEC_I16X8_EXT_MUL_LOW_I8X16_S => "i16x8.extmul_low_i8x16_s",
        OPCODE_VEC_I16X8_EXT_MUL_HIGH_I8X16_S => "i16x8.extmul_high_i8x16_s",
        OPCODE_VEC_I16X8_EXT_MUL_LOW_I8X16_U => "i16x8.extmul_low_i8x16_u",
        OPCODE_VEC_I16X8_EXT_MUL_HIGH_I8X16_U => "i16x8.extmul_high_i8x16_u",
        OPCODE_VEC_I32X4_EXTADD_PAIRWISE_I16X8_S => "i32x4.extadd_pairwise_i16x8_s",
        OPCODE_VEC_I32X4_EXTADD_PAIRWISE_I16X8_U => "i32x4.extadd_pairwise_i16x8_u",
        OPCODE_VEC_I32X4_ABS => "i32x4.abs",
        OPCODE_VEC_I32X4_NEG => "i32x4.neg",
        OPCODE_VEC_I32X4_ALL_TRUE => "i32x4.all_true",
        OPCODE_VEC_I32X4_BIT_MASK => "i32x4.bitmask",
        OPCODE_VEC_I32X4_EXTEND_LOW_I16X8_S => "i32x4.extend_low_i16x8_s",
        OPCODE_VEC_I32X4_EXTEND_HIGH_I16X8_S => "i32x4.extend_high_i16x8_s",
        OPCODE_VEC_I32X4_EXTEND_LOW_I16X8_U => "i32x4.extend_low_i16x8_u",
        OPCODE_VEC_I32X4_EXTEND_HIGH_I16X8_U => "i32x4.extend_high_i16x8_u",
        OPCODE_VEC_I32X4_SHL => "i32x4.shl",
        OPCODE_VEC_I32X4_SHR_S => "i32x4.shr_s",
        OPCODE_VEC_I32X4_SHR_U => "i32x4.shr_u",
        OPCODE_VEC_I32X4_ADD => "i32x4.add",
        OPCODE_VEC_I32X4_SUB => "i32x4.sub",
        OPCODE_VEC_I32X4_MUL => "i32x4.mul",
        OPCODE_VEC_I32X4_MIN_S => "i32x4.min_s",
        OPCODE_VEC_I32X4_MIN_U => "i32x4.min_u",
        OPCODE_VEC_I32X4_MAX_S => "i32x4.max_s",
        OPCODE_VEC_I32X4_MAX_U => "i32x4.max_u",
        OPCODE_VEC_I32X4_DOT_I16X8_S => "i32x4.dot_i16x8_s",
        OPCODE_VEC_I32X4_EXT_MUL_LOW_I16X8_S => "i32x4.extmul_low_i16x8_s",
        OPCODE_VEC_I32X4_EXT_MUL_HIGH_I16X8_S => "i32x4.extmul_high_i16x8_s",
        OPCODE_VEC_I32X4_EXT_MUL_LOW_I16X8_U => "i32x4.extmul_low_i16x8_u",
        OPCODE_VEC_I32X4_EXT_MUL_HIGH_I16X8_U => "i32x4.extmul_high_i16x8_u",
        OPCODE_VEC_I64X2_ABS => "i64x2.abs",
        OPCODE_VEC_I64X2_NEG => "i64x2.neg",
        OPCODE_VEC_I64X2_ALL_TRUE => "i64x2.all_true",
        OPCODE_VEC_I64X2_BIT_MASK => "i64x2.bitmask",
        OPCODE_VEC_I64X2_EXTEND_LOW_I32X4_S => "i64x2.extend_low_i32x4_s",
        OPCODE_VEC_I64X2_EXTEND_HIGH_I32X4_S => "i64x2.extend_high_i32x4_s",
        OPCODE_VEC_I64X2_EXTEND_LOW_I32X4_U => "i64x2.extend_low_i32x4_u",
        OPCODE_VEC_I64X2_EXTEND_HIGH_I32X4_U => "i64x2.extend_high_i32x4_u",
        OPCODE_VEC_I64X2_SHL => "i64x2.shl",
        OPCODE_VEC_I64X2_SHR_S => "i64x2.shr_s",
        OPCODE_VEC_I64X2_SHR_U => "i64x2.shr_u",
        OPCODE_VEC_I64X2_ADD => "i64x2.add",
        OPCODE_VEC_I64X2_SUB => "i64x2.sub",
        OPCODE_VEC_I64X2_MUL => "i64x2.mul",
        OPCODE_VEC_I64X2_EXT_MUL_LOW_I32X4_S => "i64x2.extmul_low_i32x4_s",
        OPCODE_VEC_I64X2_EXT_MUL_HIGH_I32X4_S => "i64x2.extmul_high_i32x4_s",
        OPCODE_VEC_I64X2_EXT_MUL_LOW_I32X4_U => "i64x2.extmul_low_i32x4_u",
        OPCODE_VEC_I64X2_EXT_MUL_HIGH_I32X4_U => "i64x2.extmul_high_i32x4_u",
        OPCODE_VEC_F32X4_CEIL => "f32x4.ceil",
        OPCODE_VEC_F32X4_FLOOR => "f32x4.floor",
        OPCODE_VEC_F32X4_TRUNC => "f32x4.trunc",
        OPCODE_VEC_F32X4_NEAREST => "f32x4.nearest",
        OPCODE_VEC_F32X4_ABS => "f32x4.abs",
        OPCODE_VEC_F32X4_NEG => "f32x4.neg",
        OPCODE_VEC_F32X4_SQRT => "f32x4.sqrt",
        OPCODE_VEC_F32X4_ADD => "f32x4.add",
        OPCODE_VEC_F32X4_SUB => "f32x4.sub",
        OPCODE_VEC_F32X4_MUL => "f32x4.mul",
        OPCODE_VEC_F32X4_DIV => "f32x4.div",
        OPCODE_VEC_F32X4_MIN => "f32x4.min",
        OPCODE_VEC_F32X4_MAX => "f32x4.max",
        OPCODE_VEC_F32X4_PMIN => "f32x4.pmin",
        OPCODE_VEC_F32X4_PMAX => "f32x4.pmax",
        OPCODE_VEC_F64X2_CEIL => "f64x2.ceil",
        OPCODE_VEC_F64X2_FLOOR => "f64x2.floor",
        OPCODE_VEC_F64X2_TRUNC => "f64x2.trunc",
        OPCODE_VEC_F64X2_NEAREST => "f64x2.nearest",
        OPCODE_VEC_F64X2_ABS => "f64x2.abs",
        OPCODE_VEC_F64X2_NEG => "f64x2.neg",
        OPCODE_VEC_F64X2_SQRT => "f64x2.sqrt",
        OPCODE_VEC_F64X2_ADD => "f64x2.add",
        OPCODE_VEC_F64X2_SUB => "f64x2.sub",
        OPCODE_VEC_F64X2_MUL => "f64x2.mul",
        OPCODE_VEC_F64X2_DIV => "f64x2.div",
        OPCODE_VEC_F64X2_MIN => "f64x2.min",
        OPCODE_VEC_F64X2_MAX => "f64x2.max",
        OPCODE_VEC_F64X2_PMIN => "f64x2.pmin",
        OPCODE_VEC_F64X2_PMAX => "f64x2.pmax",
        OPCODE_VEC_I32X4_TRUNC_SAT_F32X4_S => "i32x4.trunc_sat_f32x4_s",
        OPCODE_VEC_I32X4_TRUNC_SAT_F32X4_U => "i32x4.trunc_sat_f32x4_u",
        OPCODE_VEC_F32X4_CONVERT_I32X4_S => "f32x4.convert_i32x4_s",
        OPCODE_VEC_F32X4_CONVERT_I32X4_U => "f32x4.convert_i32x4_u",
        OPCODE_VEC_I32X4_TRUNC_SAT_F64X2_S_ZERO => "i32x4.trunc_sat_f64x2_s_zero",
        OPCODE_VEC_I32X4_TRUNC_SAT_F64X2_U_ZERO => "i32x4.trunc_sat_f64x2_u_zero",
        OPCODE_VEC_F64X2_CONVERT_LOW_I32X4_S => "f64x2.convert_low_i32x4_s",
        OPCODE_VEC_F64X2_CONVERT_LOW_I32X4_U => "f64x2.convert_low_i32x4_u",
        OPCODE_VEC_F32X4_DEMOTE_F64X2_ZERO => "f32x4.demote_f64x2_zero",
        OPCODE_VEC_F64X2_PROMOTE_LOW_F32X4_ZERO => "f64x2.promote_low_f32x4",
        _ => "",
    }
}

pub const fn atomic_instruction_name(opcode: OpcodeAtomic) -> &'static str {
    match opcode {
        OPCODE_ATOMIC_MEMORY_NOTIFY => "memory.atomic.notify",
        OPCODE_ATOMIC_MEMORY_WAIT32 => "memory.atomic.wait32",
        OPCODE_ATOMIC_MEMORY_WAIT64 => "memory.atomic.wait64",
        OPCODE_ATOMIC_FENCE => "atomic.fence",
        OPCODE_ATOMIC_I32_LOAD => "i32.atomic.load",
        OPCODE_ATOMIC_I64_LOAD => "i64.atomic.load",
        OPCODE_ATOMIC_I32_LOAD8_U => "i32.atomic.load8_u",
        OPCODE_ATOMIC_I32_LOAD16_U => "i32.atomic.load16_u",
        OPCODE_ATOMIC_I64_LOAD8_U => "i64.atomic.load8_u",
        OPCODE_ATOMIC_I64_LOAD16_U => "i64.atomic.load16_u",
        OPCODE_ATOMIC_I64_LOAD32_U => "i64.atomic.load32_u",
        OPCODE_ATOMIC_I32_STORE => "i32.atomic.store",
        OPCODE_ATOMIC_I64_STORE => "i64.atomic.store",
        OPCODE_ATOMIC_I32_STORE8 => "i32.atomic.store8",
        OPCODE_ATOMIC_I32_STORE16 => "i32.atomic.store16",
        OPCODE_ATOMIC_I64_STORE8 => "i64.atomic.store8",
        OPCODE_ATOMIC_I64_STORE16 => "i64.atomic.store16",
        OPCODE_ATOMIC_I64_STORE32 => "i64.atomic.store32",
        OPCODE_ATOMIC_I32_RMW_ADD => "i32.atomic.rmw.add",
        OPCODE_ATOMIC_I64_RMW_ADD => "i64.atomic.rmw.add",
        OPCODE_ATOMIC_I32_RMW8_ADD_U => "i32.atomic.rmw8.add_u",
        OPCODE_ATOMIC_I32_RMW16_ADD_U => "i32.atomic.rmw16.add_u",
        OPCODE_ATOMIC_I64_RMW8_ADD_U => "i64.atomic.rmw8.add_u",
        OPCODE_ATOMIC_I64_RMW16_ADD_U => "i64.atomic.rmw16.add_u",
        OPCODE_ATOMIC_I64_RMW32_ADD_U => "i64.atomic.rmw32.add_u",
        OPCODE_ATOMIC_I32_RMW_SUB => "i32.atomic.rmw.sub",
        OPCODE_ATOMIC_I64_RMW_SUB => "i64.atomic.rmw.sub",
        OPCODE_ATOMIC_I32_RMW8_SUB_U => "i32.atomic.rmw8.sub_u",
        OPCODE_ATOMIC_I32_RMW16_SUB_U => "i32.atomic.rmw16.sub_u",
        OPCODE_ATOMIC_I64_RMW8_SUB_U => "i64.atomic.rmw8.sub_u",
        OPCODE_ATOMIC_I64_RMW16_SUB_U => "i64.atomic.rmw16.sub_u",
        OPCODE_ATOMIC_I64_RMW32_SUB_U => "i64.atomic.rmw32.sub_u",
        OPCODE_ATOMIC_I32_RMW_AND => "i32.atomic.rmw.and",
        OPCODE_ATOMIC_I64_RMW_AND => "i64.atomic.rmw.and",
        OPCODE_ATOMIC_I32_RMW8_AND_U => "i32.atomic.rmw8.and_u",
        OPCODE_ATOMIC_I32_RMW16_AND_U => "i32.atomic.rmw16.and_u",
        OPCODE_ATOMIC_I64_RMW8_AND_U => "i64.atomic.rmw8.and_u",
        OPCODE_ATOMIC_I64_RMW16_AND_U => "i64.atomic.rmw16.and_u",
        OPCODE_ATOMIC_I64_RMW32_AND_U => "i64.atomic.rmw32.and_u",
        OPCODE_ATOMIC_I32_RMW_OR => "i32.atomic.rmw.or",
        OPCODE_ATOMIC_I64_RMW_OR => "i64.atomic.rmw.or",
        OPCODE_ATOMIC_I32_RMW8_OR_U => "i32.atomic.rmw8.or_u",
        OPCODE_ATOMIC_I32_RMW16_OR_U => "i32.atomic.rmw16.or_u",
        OPCODE_ATOMIC_I64_RMW8_OR_U => "i64.atomic.rmw8.or_u",
        OPCODE_ATOMIC_I64_RMW16_OR_U => "i64.atomic.rmw16.or_u",
        OPCODE_ATOMIC_I64_RMW32_OR_U => "i64.atomic.rmw32.or_u",
        OPCODE_ATOMIC_I32_RMW_XOR => "i32.atomic.rmw.xor",
        OPCODE_ATOMIC_I64_RMW_XOR => "i64.atomic.rmw.xor",
        OPCODE_ATOMIC_I32_RMW8_XOR_U => "i32.atomic.rmw8.xor_u",
        OPCODE_ATOMIC_I32_RMW16_XOR_U => "i32.atomic.rmw16.xor_u",
        OPCODE_ATOMIC_I64_RMW8_XOR_U => "i64.atomic.rmw8.xor_u",
        OPCODE_ATOMIC_I64_RMW16_XOR_U => "i64.atomic.rmw16.xor_u",
        OPCODE_ATOMIC_I64_RMW32_XOR_U => "i64.atomic.rmw32.xor_u",
        OPCODE_ATOMIC_I32_RMW_XCHG => "i32.atomic.rmw.xchg",
        OPCODE_ATOMIC_I64_RMW_XCHG => "i64.atomic.rmw.xchg",
        OPCODE_ATOMIC_I32_RMW8_XCHG_U => "i32.atomic.rmw8.xchg_u",
        OPCODE_ATOMIC_I32_RMW16_XCHG_U => "i32.atomic.rmw16.xchg_u",
        OPCODE_ATOMIC_I64_RMW8_XCHG_U => "i64.atomic.rmw8.xchg_u",
        OPCODE_ATOMIC_I64_RMW16_XCHG_U => "i64.atomic.rmw16.xchg_u",
        OPCODE_ATOMIC_I64_RMW32_XCHG_U => "i64.atomic.rmw32.xchg_u",
        OPCODE_ATOMIC_I32_RMW_CMPXCHG => "i32.atomic.rmw.cmpxchg",
        OPCODE_ATOMIC_I64_RMW_CMPXCHG => "i64.atomic.rmw.cmpxchg",
        OPCODE_ATOMIC_I32_RMW8_CMPXCHG_U => "i32.atomic.rmw8.cmpxchg_u",
        OPCODE_ATOMIC_I32_RMW16_CMPXCHG_U => "i32.atomic.rmw16.cmpxchg_u",
        OPCODE_ATOMIC_I64_RMW8_CMPXCHG_U => "i64.atomic.rmw8.cmpxchg_u",
        OPCODE_ATOMIC_I64_RMW16_CMPXCHG_U => "i64.atomic.rmw16.cmpxchg_u",
        OPCODE_ATOMIC_I64_RMW32_CMPXCHG_U => "i64.atomic.rmw32.cmpxchg_u",
        _ => "",
    }
}

pub const fn tail_call_instruction_name(opcode: OpcodeTailCall) -> &'static str {
    match opcode {
        OPCODE_TAIL_CALL_RETURN_CALL => "return_call",
        OPCODE_TAIL_CALL_RETURN_CALL_INDIRECT => "return_call_indirect",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_name_matches_core_opcodes() {
        assert_eq!(instruction_name(OPCODE_UNREACHABLE), "unreachable");
        assert_eq!(instruction_name(OPCODE_TYPED_SELECT), "typed_select");
        assert_eq!(instruction_name(OPCODE_MEMORY_GROW), "memory.grow");
        assert_eq!(instruction_name(OPCODE_REF_NULL), "ref.null");
        assert_eq!(instruction_name(OPCODE_MISC_PREFIX), "misc_prefix");
        assert_eq!(instruction_name(OPCODE_VEC_PREFIX), "vector_prefix");
        assert_eq!(instruction_name(OPCODE_ATOMIC_PREFIX), "");
        assert_eq!(instruction_name(0xff), "");
    }

    #[test]
    fn prefixed_instruction_names_match() {
        assert_eq!(
            misc_instruction_name(OPCODE_MISC_MEMORY_COPY),
            "memory.copy"
        );
        assert_eq!(misc_instruction_name(OPCODE_MISC_TABLE_FILL), "table.fill");
        assert_eq!(vector_instruction_name(OPCODE_VEC_V128_LOAD), "v128.load");
        assert_eq!(
            vector_instruction_name(OPCODE_VEC_I8X16_SWIZZLE),
            "i8x16.swizzle"
        );
        assert_eq!(atomic_instruction_name(OPCODE_ATOMIC_FENCE), "atomic.fence");
        assert_eq!(
            atomic_instruction_name(OPCODE_ATOMIC_I64_RMW32_CMPXCHG_U),
            "i64.atomic.rmw32.cmpxchg_u"
        );
        assert_eq!(
            tail_call_instruction_name(OPCODE_TAIL_CALL_RETURN_CALL),
            "return_call"
        );
        assert_eq!(
            tail_call_instruction_name(OPCODE_TAIL_CALL_RETURN_CALL_INDIRECT),
            "return_call_indirect"
        );
    }

    #[test]
    fn unknown_prefixed_instruction_names_are_empty() {
        assert_eq!(misc_instruction_name(0xff), "");
        assert_eq!(atomic_instruction_name(0xff), "");
        assert_eq!(tail_call_instruction_name(0xff), "");

        let unknown_vec = (0..=u8::MAX)
            .find(|opcode| vector_instruction_name(*opcode).is_empty())
            .expect("expected at least one undefined vector opcode");
        assert_eq!(vector_instruction_name(unknown_vec), "");
    }

    #[test]
    fn instruction_helper_uses_opcode_name() {
        let instruction = Instruction::new(OPCODE_CALL_INDIRECT);
        assert_eq!(instruction.opcode, OPCODE_CALL_INDIRECT);
        assert_eq!(instruction.name(), "call_indirect");
    }
}
