use core::fmt;

use crate::backend::VReg;

use super::reg::{format_vreg_sized, vreg_for_real_reg, RBP};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Label(pub u32);

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "L{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddressMode {
    ImmReg {
        imm32: u32,
        base: VReg,
    },
    ImmRbp {
        imm32: u32,
    },
    RegRegShift {
        imm32: u32,
        base: VReg,
        index: VReg,
        shift: u8,
    },
    RipRel {
        label: Label,
    },
}

impl AddressMode {
    pub fn imm_reg(imm32: u32, base: VReg) -> Self {
        Self::ImmReg { imm32, base }
    }

    pub fn imm_rbp(imm32: u32) -> Self {
        Self::ImmRbp { imm32 }
    }

    pub fn reg_reg_shift(imm32: u32, base: VReg, index: VReg, shift: u8) -> Self {
        assert!(shift <= 3, "invalid shift");
        Self::RegRegShift {
            imm32,
            base,
            index,
            shift,
        }
    }

    pub fn rip_rel(label: Label) -> Self {
        Self::RipRel { label }
    }

    pub fn uses(&self, out: &mut Vec<VReg>) {
        match self {
            Self::ImmReg { base, .. } => out.push(*base),
            Self::RegRegShift { base, index, .. } => {
                out.push(*base);
                out.push(*index);
            }
            Self::ImmRbp { .. } | Self::RipRel { .. } => {}
        }
    }

    pub const fn nregs(&self) -> usize {
        match self {
            Self::ImmReg { .. } => 1,
            Self::RegRegShift { .. } => 2,
            Self::ImmRbp { .. } | Self::RipRel { .. } => 0,
        }
    }

    pub fn assign_use(&mut self, index: usize, reg: VReg) {
        match self {
            Self::ImmReg { base, .. } => {
                assert_eq!(index, 0, "invalid amode assignment");
                *base = reg;
            }
            Self::RegRegShift {
                base, index: idx, ..
            } => match index {
                0 => *base = reg,
                1 => *idx = reg,
                _ => panic!("invalid amode assignment"),
            },
            Self::ImmRbp { .. } | Self::RipRel { .. } => panic!("invalid amode assignment"),
        }
    }

    fn fmt_with_rbp(&self, rbp: VReg, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ImmReg { imm32, base } => {
                if *imm32 == 0 {
                    write!(f, "({})", format_vreg_sized(*base, true))
                } else {
                    write!(f, "{}({})", *imm32 as i32, format_vreg_sized(*base, true))
                }
            }
            Self::ImmRbp { imm32 } => {
                if *imm32 == 0 {
                    write!(f, "({})", format_vreg_sized(rbp, true))
                } else {
                    write!(f, "{}({})", *imm32 as i32, format_vreg_sized(rbp, true))
                }
            }
            Self::RegRegShift {
                imm32,
                base,
                index,
                shift,
            } => {
                let scale = 1 << shift;
                if *imm32 == 0 {
                    write!(
                        f,
                        "({},{},{scale})",
                        format_vreg_sized(*base, true),
                        format_vreg_sized(*index, true)
                    )
                } else {
                    write!(
                        f,
                        "{}({},{},{scale})",
                        *imm32 as i32,
                        format_vreg_sized(*base, true),
                        format_vreg_sized(*index, true)
                    )
                }
            }
            Self::RipRel { label } => write!(f, "{label}(%rip)"),
        }
    }
}

impl fmt::Display for AddressMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_with_rbp(vreg_for_real_reg(RBP), f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operand {
    Reg(VReg),
    Mem(AddressMode),
    Imm32(u32),
    Label(Label),
}

impl Operand {
    pub fn reg(reg: VReg) -> Self {
        Self::Reg(reg)
    }

    pub fn mem(amode: AddressMode) -> Self {
        Self::Mem(amode)
    }

    pub fn imm32(imm32: u32) -> Self {
        Self::Imm32(imm32)
    }

    pub fn label(label: Label) -> Self {
        Self::Label(label)
    }

    pub fn format(&self, is_64: bool) -> String {
        match self {
            Self::Reg(reg) => format_vreg_sized(*reg, is_64),
            Self::Mem(amode) => amode.to_string(),
            Self::Imm32(imm32) => format!("${}", *imm32 as i32),
            Self::Label(label) => label.to_string(),
        }
    }

    pub fn uses(&self, out: &mut Vec<VReg>) {
        match self {
            Self::Reg(reg) => out.push(*reg),
            Self::Mem(amode) => amode.uses(out),
            Self::Imm32(_) | Self::Label(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AddressMode, Label, Operand};
    use crate::backend::{RegType, VReg};

    #[test]
    fn address_modes_match_go_string_forms() {
        let rax = VReg::from_real_reg(1, RegType::Int);
        let rcx = VReg::from_real_reg(2, RegType::Int);
        assert_eq!(AddressMode::imm_reg(123, rax).to_string(), "123(%rax)");
        assert_eq!(
            AddressMode::reg_reg_shift(12, rax, rcx, 2).to_string(),
            "12(%rax,%rcx,4)"
        );
        assert_eq!(AddressMode::rip_rel(Label(7)).to_string(), "L7(%rip)");
    }

    #[test]
    fn operand_format_matches_go_immediates() {
        assert_eq!(Operand::imm32((-126i32) as u32).format(false), "$-126");
    }
}
