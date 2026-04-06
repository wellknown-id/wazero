use core::fmt;

use crate::backend::{RegType, VReg};

use super::lower_instr_operands::ExtendOp;
use super::reg::format_vreg_sized;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AddressModeKind {
    RegScaledExtended = 0,
    RegScaled,
    RegExtended,
    RegReg,
    RegSignedImm9,
    RegUnsignedImm12,
    PostIndex,
    PreIndex,
    ArgStackSpace,
    ResultStackSpace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressMode {
    pub kind: AddressModeKind,
    pub rn: VReg,
    pub rm: VReg,
    pub ext_op: ExtendOp,
    pub imm: i64,
}

impl AddressMode {
    pub const fn reg_unsigned_imm12(rn: VReg, imm: i64) -> Self {
        Self {
            kind: AddressModeKind::RegUnsignedImm12,
            rn,
            rm: VReg::INVALID,
            ext_op: ExtendOp::Uxtx,
            imm,
        }
    }

    pub const fn reg_signed_imm9(rn: VReg, imm: i64) -> Self {
        Self {
            kind: AddressModeKind::RegSignedImm9,
            rn,
            rm: VReg::INVALID,
            ext_op: ExtendOp::Uxtx,
            imm,
        }
    }

    pub const fn reg_reg(rn: VReg, rm: VReg) -> Self {
        Self {
            kind: AddressModeKind::RegReg,
            rn,
            rm,
            ext_op: ExtendOp::Uxtx,
            imm: 0,
        }
    }

    pub fn format(self, dst_size_bits: u8) -> String {
        assert_eq!(self.rn.reg_type(), RegType::Int, "arm64 base registers must be integer");
        let base = format_vreg_sized(self.rn, 64);
        match self.kind {
            AddressModeKind::RegScaledExtended => format!(
                "[{base}, {}, {} #0x{:x}]",
                format_vreg_sized(self.rm, self.index_reg_bits()),
                self.ext_op,
                self.size_in_bits_to_shift_amount(dst_size_bits)
            ),
            AddressModeKind::RegScaled => format!(
                "[{base}, {}, lsl #0x{:x}]",
                format_vreg_sized(self.rm, self.index_reg_bits()),
                self.size_in_bits_to_shift_amount(dst_size_bits)
            ),
            AddressModeKind::RegExtended => format!(
                "[{base}, {}, {}]",
                format_vreg_sized(self.rm, self.index_reg_bits()),
                self.ext_op
            ),
            AddressModeKind::RegReg => {
                format!("[{base}, {}]", format_vreg_sized(self.rm, self.index_reg_bits()))
            }
            AddressModeKind::RegSignedImm9 | AddressModeKind::RegUnsignedImm12 => {
                if self.imm == 0 {
                    format!("[{base}]")
                } else {
                    format!("[{base}, {}]", format_signed_hex_imm(self.imm))
                }
            }
            AddressModeKind::PostIndex => format!("[{base}], {}", format_signed_hex_imm(self.imm)),
            AddressModeKind::PreIndex => format!("[{base}, {}]!", format_signed_hex_imm(self.imm)),
            AddressModeKind::ArgStackSpace => format!("[#arg_space, {}]", format_signed_hex_imm(self.imm)),
            AddressModeKind::ResultStackSpace => format!("[#ret_space, {}]", format_signed_hex_imm(self.imm)),
        }
    }

    pub const fn index_reg_bits(self) -> u8 {
        let bits = self.ext_op.src_bits();
        assert!(bits == 32 || bits == 64);
        bits
    }

    pub const fn size_in_bits_to_shift_amount(self, size_in_bits: u8) -> u8 {
        match size_in_bits {
            8 => 0,
            16 => 1,
            32 => 2,
            64 => 3,
            _ => 0,
        }
    }
}

fn format_signed_hex_imm(imm: i64) -> String {
    if imm < 0 {
        format!("#0x-{val:x}", val = imm.wrapping_neg())
    } else {
        format!("#0x{imm:x}")
    }
}

pub const fn offset_fits_reg_unsigned_imm12(dst_size_bits: u8, offset: i64) -> bool {
    let divisor = (dst_size_bits / 8) as i64;
    offset > 0 && offset % divisor == 0 && offset / divisor < 4096
}

pub const fn offset_fits_reg_signed_imm9(offset: i64) -> bool {
    offset >= -256 && offset <= 255
}

pub fn resolve_address_mode_for_offset(
    offset: i64,
    dst_bits: u8,
    rn: VReg,
    index_reg: VReg,
) -> AddressMode {
    if offset_fits_reg_unsigned_imm12(dst_bits, offset) {
        AddressMode::reg_unsigned_imm12(rn, offset)
    } else if offset_fits_reg_signed_imm9(offset) {
        AddressMode::reg_signed_imm9(rn, offset)
    } else {
        AddressMode::reg_reg(rn, index_reg)
    }
}

impl fmt::Display for AddressMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format(64))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        offset_fits_reg_signed_imm9, offset_fits_reg_unsigned_imm12, resolve_address_mode_for_offset,
        AddressMode, AddressModeKind,
    };
    use crate::backend::isa::arm64::lower_instr_operands::ExtendOp;
    use crate::backend::isa::arm64::reg::{SP, X1, X2};
    use crate::backend::{RegType, VReg};

    #[test]
    fn address_mode_format_matches_go_examples() {
        let sp = VReg::from_real_reg(SP, RegType::Int);
        let x1 = VReg::from_real_reg(X1, RegType::Int);
        let x2 = VReg::from_real_reg(X2, RegType::Int);
        assert_eq!(AddressMode::reg_unsigned_imm12(sp, 0).format(64), "[sp]");
        assert_eq!(AddressMode::reg_signed_imm9(sp, -16).format(64), "[sp, #0x-10]");
        assert_eq!(AddressMode::reg_reg(x1, x2).format(64), "[x1, x2]");
        let scaled = AddressMode {
            kind: AddressModeKind::RegScaledExtended,
            rn: x1,
            rm: x2,
            ext_op: ExtendOp::Sxtw,
            imm: 0,
        };
        assert_eq!(scaled.format(64), "[x1, w2, sxtw #0x3]");
    }

    #[test]
    fn immediate_fit_helpers_match_go_rules() {
        assert!(offset_fits_reg_unsigned_imm12(64, 8));
        assert!(offset_fits_reg_unsigned_imm12(64, 4095 * 8));
        assert!(!offset_fits_reg_unsigned_imm12(64, 7));
        assert!(offset_fits_reg_signed_imm9(-256));
        assert!(offset_fits_reg_signed_imm9(255));
        assert!(!offset_fits_reg_signed_imm9(-257));
    }

    #[test]
    fn resolve_offset_chooses_best_addressing_form() {
        let base = VReg::from_real_reg(X1, RegType::Int);
        let scratch = VReg(128).set_reg_type(RegType::Int);
        assert_eq!(
            resolve_address_mode_for_offset(32, 64, base, scratch).kind,
            AddressModeKind::RegUnsignedImm12
        );
        assert_eq!(
            resolve_address_mode_for_offset(-8, 64, base, scratch).kind,
            AddressModeKind::RegSignedImm9
        );
        assert_eq!(
            resolve_address_mode_for_offset(1 << 20, 64, base, scratch).kind,
            AddressModeKind::RegReg
        );
    }
}
