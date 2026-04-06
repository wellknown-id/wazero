use std::fmt;

use crate::ssa::types::Type;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SignatureId(pub u32);

impl fmt::Display for SignatureId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sig{}", self.0)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signature {
    pub id: SignatureId,
    pub params: Vec<Type>,
    pub results: Vec<Type>,
    pub used: bool,
}

impl Signature {
    pub fn new(id: SignatureId, params: Vec<Type>, results: Vec<Type>) -> Self {
        Self {
            id,
            params,
            results,
            used: false,
        }
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.id)?;
        if self.params.is_empty() {
            f.write_str("v")?;
        } else {
            for param in &self.params {
                write!(f, "{param}")?;
            }
        }
        f.write_str("_")?;
        if self.results.is_empty() {
            f.write_str("v")
        } else {
            for result in &self.results {
                write!(f, "{result}")?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Signature, SignatureId};
    use crate::ssa::types::Type;

    #[test]
    fn signature_display_matches_go() {
        let sig = Signature::new(SignatureId(3), vec![Type::I32, Type::I64], vec![Type::F32]);
        assert_eq!(sig.to_string(), "sig3: i32i64_f32");
        assert_eq!(
            Signature::new(SignatureId(0), vec![], vec![]).to_string(),
            "sig0: v_v"
        );
    }
}
