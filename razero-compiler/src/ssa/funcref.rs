use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FuncRef(pub u32);

impl fmt::Display for FuncRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "f{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::FuncRef;

    #[test]
    fn func_ref_display() {
        assert_eq!(FuncRef(7).to_string(), "f7");
    }
}
