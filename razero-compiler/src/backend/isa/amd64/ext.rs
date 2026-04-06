use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExtMode {
    BL,
    BQ,
    WL,
    WQ,
    LQ,
}

impl ExtMode {
    pub const fn src_size(self) -> u8 {
        match self {
            Self::BL | Self::BQ => 1,
            Self::WL | Self::WQ => 2,
            Self::LQ => 4,
        }
    }

    pub const fn dst_size(self) -> u8 {
        match self {
            Self::BL | Self::WL => 4,
            Self::BQ | Self::WQ | Self::LQ => 8,
        }
    }
}

impl fmt::Display for ExtMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BL => "bl",
            Self::BQ => "bq",
            Self::WL => "wl",
            Self::WQ => "wq",
            Self::LQ => "lq",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ExtMode;

    #[test]
    fn ext_modes_report_sizes() {
        assert_eq!(ExtMode::BL.src_size(), 1);
        assert_eq!(ExtMode::WL.dst_size(), 4);
        assert_eq!(ExtMode::LQ.dst_size(), 8);
        assert_eq!(ExtMode::BQ.to_string(), "bq");
    }
}
