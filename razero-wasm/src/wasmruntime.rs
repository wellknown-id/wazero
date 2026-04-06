use std::error::Error as StdError;
use std::fmt::{Display, Formatter};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Error(&'static str);

pub type RuntimeError = Error;

impl Error {
    pub const fn new(text: &'static str) -> Self {
        Self(text)
    }

    pub const fn message(self) -> &'static str {
        self.0
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl StdError for Error {}

pub const ERR_RUNTIME_STACK_OVERFLOW: Error = Error::new("stack overflow");
pub const ERR_RUNTIME_INVALID_CONVERSION_TO_INTEGER: Error =
    Error::new("invalid conversion to integer");
pub const ERR_RUNTIME_INTEGER_OVERFLOW: Error = Error::new("integer overflow");
pub const ERR_RUNTIME_INTEGER_DIVIDE_BY_ZERO: Error = Error::new("integer divide by zero");
pub const ERR_RUNTIME_UNREACHABLE: Error = Error::new("unreachable");
pub const ERR_RUNTIME_OUT_OF_BOUNDS_MEMORY_ACCESS: Error =
    Error::new("out of bounds memory access");
pub const ERR_RUNTIME_INVALID_TABLE_ACCESS: Error = Error::new("invalid table access");
pub const ERR_RUNTIME_INDIRECT_CALL_TYPE_MISMATCH: Error =
    Error::new("indirect call type mismatch");
pub const ERR_RUNTIME_UNALIGNED_ATOMIC: Error = Error::new("unaligned atomic");
pub const ERR_RUNTIME_EXPECTED_SHARED_MEMORY: Error = Error::new("expected shared memory");
pub const ERR_RUNTIME_TOO_MANY_WAITERS: Error = Error::new("too many waiters");
pub const ERR_RUNTIME_FUEL_EXHAUSTED: Error = Error::new("fuel exhausted");
pub const ERR_RUNTIME_POLICY_DENIED: Error = Error::new("policy denied");
pub const ERR_RUNTIME_MEMORY_FAULT: Error = Error::new("memory fault");
pub const ERR_RUNTIME_ASYNC_YIELD: Error = Error::new("async yield");

pub const STACK_OVERFLOW: Error = ERR_RUNTIME_STACK_OVERFLOW;
pub const INVALID_CONVERSION_TO_INTEGER: Error = ERR_RUNTIME_INVALID_CONVERSION_TO_INTEGER;
pub const INTEGER_OVERFLOW: Error = ERR_RUNTIME_INTEGER_OVERFLOW;
pub const INTEGER_DIVIDE_BY_ZERO: Error = ERR_RUNTIME_INTEGER_DIVIDE_BY_ZERO;
pub const UNREACHABLE: Error = ERR_RUNTIME_UNREACHABLE;
pub const OUT_OF_BOUNDS_MEMORY_ACCESS: Error = ERR_RUNTIME_OUT_OF_BOUNDS_MEMORY_ACCESS;
pub const INVALID_TABLE_ACCESS: Error = ERR_RUNTIME_INVALID_TABLE_ACCESS;
pub const INDIRECT_CALL_TYPE_MISMATCH: Error = ERR_RUNTIME_INDIRECT_CALL_TYPE_MISMATCH;
pub const UNALIGNED_ATOMIC: Error = ERR_RUNTIME_UNALIGNED_ATOMIC;
pub const EXPECTED_SHARED_MEMORY: Error = ERR_RUNTIME_EXPECTED_SHARED_MEMORY;
pub const TOO_MANY_WAITERS: Error = ERR_RUNTIME_TOO_MANY_WAITERS;
pub const FUEL_EXHAUSTED: Error = ERR_RUNTIME_FUEL_EXHAUSTED;
pub const POLICY_DENIED: Error = ERR_RUNTIME_POLICY_DENIED;
pub const MEMORY_FAULT: Error = ERR_RUNTIME_MEMORY_FAULT;
pub const ASYNC_YIELD: Error = ERR_RUNTIME_ASYNC_YIELD;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_errors_are_named_and_displayable() {
        let cases = [
            (ERR_RUNTIME_STACK_OVERFLOW, "stack overflow"),
            (
                ERR_RUNTIME_INVALID_CONVERSION_TO_INTEGER,
                "invalid conversion to integer",
            ),
            (ERR_RUNTIME_INTEGER_OVERFLOW, "integer overflow"),
            (ERR_RUNTIME_INTEGER_DIVIDE_BY_ZERO, "integer divide by zero"),
            (ERR_RUNTIME_UNREACHABLE, "unreachable"),
            (
                ERR_RUNTIME_OUT_OF_BOUNDS_MEMORY_ACCESS,
                "out of bounds memory access",
            ),
            (ERR_RUNTIME_INVALID_TABLE_ACCESS, "invalid table access"),
            (
                ERR_RUNTIME_INDIRECT_CALL_TYPE_MISMATCH,
                "indirect call type mismatch",
            ),
            (ERR_RUNTIME_UNALIGNED_ATOMIC, "unaligned atomic"),
            (ERR_RUNTIME_EXPECTED_SHARED_MEMORY, "expected shared memory"),
            (ERR_RUNTIME_TOO_MANY_WAITERS, "too many waiters"),
            (ERR_RUNTIME_FUEL_EXHAUSTED, "fuel exhausted"),
            (ERR_RUNTIME_POLICY_DENIED, "policy denied"),
            (ERR_RUNTIME_MEMORY_FAULT, "memory fault"),
            (ERR_RUNTIME_ASYNC_YIELD, "async yield"),
        ];

        for (err, expected) in cases {
            assert_eq!(expected, err.message());
            assert_eq!(expected, err.to_string());
        }
    }

    #[test]
    fn aliases_match_named_sentinels() {
        assert_eq!(STACK_OVERFLOW, ERR_RUNTIME_STACK_OVERFLOW);
        assert_eq!(ASYNC_YIELD, ERR_RUNTIME_ASYNC_YIELD);
        assert_eq!(POLICY_DENIED, ERR_RUNTIME_POLICY_DENIED);
    }
}
