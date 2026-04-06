use core::fmt;

use crate::backend::VReg;

use super::reg::format_vreg_sized;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ShiftOp {
    Lsl = 0,
    Lsr = 1,
    Asr = 2,
    Ror = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ExtendOp {
    Uxtb = 0,
    Uxth = 1,
    Uxtw = 2,
    Uxtx = 3,
    Sxtb = 4,
    Sxth = 5,
    Sxtw = 6,
    Sxtx = 7,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Imm12 {
    pub bits: u16,
    pub shift12: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    None,
    Reg(VReg),
    Imm12(Imm12),
    ShiftedReg { reg: VReg, op: ShiftOp, amount: u8 },
    ExtendedReg { reg: VReg, op: ExtendOp, amount: u8 },
}

impl Imm12 {
    pub const fn encode(self) -> u32 {
        ((self.shift12 as u32) << 12) | self.bits as u32
    }

    pub const fn value(self) -> u64 {
        if self.shift12 {
            (self.bits as u64) << 12
        } else {
            self.bits as u64
        }
    }
}

impl ExtendOp {
    pub const fn src_bits(self) -> u8 {
        match self {
            Self::Uxtb | Self::Sxtb => 8,
            Self::Uxth | Self::Sxth => 16,
            Self::Uxtw | Self::Sxtw => 32,
            Self::Uxtx | Self::Sxtx => 64,
        }
    }
}

pub const fn as_imm12(value: u64) -> Option<Imm12> {
    if value <= 0xfff {
        Some(Imm12 {
            bits: value as u16,
            shift12: false,
        })
    } else if value & 0xfff == 0 {
        let shifted = value >> 12;
        if shifted <= 0xfff {
            Some(Imm12 {
                bits: shifted as u16,
                shift12: true,
            })
        } else {
            None
        }
    } else {
        None
    }
}

impl Operand {
    pub const fn reg(self) -> Option<VReg> {
        match self {
            Self::Reg(reg) | Self::ShiftedReg { reg, .. } | Self::ExtendedReg { reg, .. } => {
                Some(reg)
            }
            Self::None | Self::Imm12(_) => None,
        }
    }

    pub fn format_with_size(self, size: u8) -> String {
        match self {
            Self::None => String::new(),
            Self::Reg(reg) => format_vreg_sized(reg, size),
            Self::Imm12(imm) => format!("#0x{:x}", imm.value()),
            Self::ShiftedReg { reg, op, amount } => {
                format!("{} , {op} #0x{amount:x}", format_vreg_sized(reg, size))
            }
            Self::ExtendedReg { reg, op, amount } => {
                if amount == 0 {
                    format!("{} , {op}", format_vreg_sized(reg, op.src_bits()))
                } else {
                    format!(
                        "{} , {op} #0x{amount:x}",
                        format_vreg_sized(reg, op.src_bits())
                    )
                }
            }
        }
    }
}

impl fmt::Display for ShiftOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Lsl => "lsl",
            Self::Lsr => "lsr",
            Self::Asr => "asr",
            Self::Ror => "ror",
        })
    }
}

impl fmt::Display for ExtendOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Uxtb => "uxtb",
            Self::Uxth => "uxth",
            Self::Uxtw => "uxtw",
            Self::Uxtx => "uxtx",
            Self::Sxtb => "sxtb",
            Self::Sxth => "sxth",
            Self::Sxtw => "sxtw",
            Self::Sxtx => "sxtx",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{as_imm12, ExtendOp, Imm12};

    #[test]
    fn imm12_matches_go_selection_rules() {
        assert_eq!(
            as_imm12(0),
            Some(Imm12 {
                bits: 0,
                shift12: false
            })
        );
        assert_eq!(
            as_imm12(0xfff),
            Some(Imm12 {
                bits: 0xfff,
                shift12: false
            })
        );
        assert_eq!(
            as_imm12(0xabc000),
            Some(Imm12 {
                bits: 0xabc,
                shift12: true
            })
        );
        assert_eq!(as_imm12(0x1_0000_0000), None);
        assert_eq!(as_imm12(0x1234), None);
    }

    #[test]
    fn extend_op_source_widths_match_arm64() {
        assert_eq!(ExtendOp::Uxtw.src_bits(), 32);
        assert_eq!(ExtendOp::Sxtx.src_bits(), 64);
    }
}
