use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum IntegerCmpCond {
    Invalid = 0,
    Equal,
    NotEqual,
    SignedLessThan,
    SignedGreaterThanOrEqual,
    SignedGreaterThan,
    SignedLessThanOrEqual,
    UnsignedLessThan,
    UnsignedGreaterThanOrEqual,
    UnsignedGreaterThan,
    UnsignedLessThanOrEqual,
}

impl IntegerCmpCond {
    pub const fn is_signed(self) -> bool {
        matches!(
            self,
            Self::SignedLessThan
                | Self::SignedGreaterThanOrEqual
                | Self::SignedGreaterThan
                | Self::SignedLessThanOrEqual
        )
    }
}

impl fmt::Display for IntegerCmpCond {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Equal => "eq",
            Self::NotEqual => "neq",
            Self::SignedLessThan => "lt_s",
            Self::SignedGreaterThanOrEqual => "ge_s",
            Self::SignedGreaterThan => "gt_s",
            Self::SignedLessThanOrEqual => "le_s",
            Self::UnsignedLessThan => "lt_u",
            Self::UnsignedGreaterThanOrEqual => "ge_u",
            Self::UnsignedGreaterThan => "gt_u",
            Self::UnsignedLessThanOrEqual => "le_u",
            Self::Invalid => panic!("invalid integer comparison condition"),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FloatCmpCond {
    Invalid = 0,
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
}

impl fmt::Display for FloatCmpCond {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Equal => "eq",
            Self::NotEqual => "neq",
            Self::LessThan => "lt",
            Self::LessThanOrEqual => "le",
            Self::GreaterThan => "gt",
            Self::GreaterThanOrEqual => "ge",
            Self::Invalid => panic!("invalid float comparison condition"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{FloatCmpCond, IntegerCmpCond};

    #[test]
    fn integer_cmp_cond_helpers() {
        assert_eq!(IntegerCmpCond::UnsignedLessThan.to_string(), "lt_u");
        assert!(IntegerCmpCond::SignedGreaterThan.is_signed());
        assert!(!IntegerCmpCond::Equal.is_signed());
    }

    #[test]
    fn float_cmp_cond_display() {
        assert_eq!(FloatCmpCond::GreaterThanOrEqual.to_string(), "ge");
    }
}
