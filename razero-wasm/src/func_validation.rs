#![doc = "Function-body validation helpers."]

use std::error::Error;
use std::fmt::{Display, Formatter};

use crate::instruction::OPCODE_END;
use crate::module::Code;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionValidationError {
    EmptyBody,
    MissingEnd,
}

impl Display for FunctionValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyBody => f.write_str("function body cannot be empty"),
            Self::MissingEnd => f.write_str("expr not end with OpcodeEnd"),
        }
    }
}

impl Error for FunctionValidationError {}

pub fn validate_wasm_function(code: &Code) -> Result<(), FunctionValidationError> {
    if code.is_host_function() {
        return Ok(());
    }
    match code.body.last().copied() {
        None => Err(FunctionValidationError::EmptyBody),
        Some(OPCODE_END) => Ok(()),
        Some(_) => Err(FunctionValidationError::MissingEnd),
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_wasm_function, FunctionValidationError};
    use crate::instruction::OPCODE_END;
    use crate::module::{Code, CodeBody};

    #[test]
    fn validates_host_and_wasm_bodies() {
        assert_eq!(
            Err(FunctionValidationError::EmptyBody),
            validate_wasm_function(&Code::default())
        );
        assert_eq!(
            Ok(()),
            validate_wasm_function(&Code {
                body: vec![0x00, OPCODE_END],
                ..Code::default()
            })
        );
        assert_eq!(
            Ok(()),
            validate_wasm_function(&Code {
                body_kind: CodeBody::Host,
                ..Code::default()
            })
        );
    }
}
