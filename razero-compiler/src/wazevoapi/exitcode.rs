//! Compiler exit codes and helpers.

use core::fmt;

pub const EXIT_CODE_MASK: u32 = 0xff;
pub const EXIT_CODE_MAX: u32 = 27;

#[derive(Copy, Clone, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct ExitCode(u32);

impl ExitCode {
    pub const OK: Self = Self(0);
    pub const GROW_STACK: Self = Self(1);
    pub const GROW_MEMORY: Self = Self(2);
    pub const UNREACHABLE: Self = Self(3);
    pub const MEMORY_OUT_OF_BOUNDS: Self = Self(4);
    pub const CALL_GO_MODULE_FUNCTION: Self = Self(5);
    pub const CALL_GO_FUNCTION: Self = Self(6);
    pub const TABLE_OUT_OF_BOUNDS: Self = Self(7);
    pub const INDIRECT_CALL_NULL_POINTER: Self = Self(8);
    pub const INDIRECT_CALL_TYPE_MISMATCH: Self = Self(9);
    pub const INTEGER_DIVISION_BY_ZERO: Self = Self(10);
    pub const INTEGER_OVERFLOW: Self = Self(11);
    pub const INVALID_CONVERSION_TO_INTEGER: Self = Self(12);
    pub const CHECK_MODULE_EXIT_CODE: Self = Self(13);
    pub const CALL_LISTENER_BEFORE: Self = Self(14);
    pub const CALL_LISTENER_AFTER: Self = Self(15);
    pub const CALL_GO_MODULE_FUNCTION_WITH_LISTENER: Self = Self(16);
    pub const CALL_GO_FUNCTION_WITH_LISTENER: Self = Self(17);
    pub const TABLE_GROW: Self = Self(18);
    pub const REF_FUNC: Self = Self(19);
    pub const MEMORY_WAIT32: Self = Self(20);
    pub const MEMORY_WAIT64: Self = Self(21);
    pub const MEMORY_NOTIFY: Self = Self(22);
    pub const UNALIGNED_ATOMIC: Self = Self(23);
    pub const FUEL_EXHAUSTED: Self = Self(24);
    pub const POLICY_DENIED: Self = Self(25);
    pub const MEMORY_FAULT: Self = Self(26);

    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    pub fn as_str(self) -> &'static str {
        match self.0 {
            0 => "ok",
            1 => "grow_stack",
            2 => "grow_memory",
            3 => "unreachable",
            4 => "memory_out_of_bounds",
            5 => "call_go_module_function",
            6 => "call_go_function",
            7 => "table_out_of_bounds",
            8 => "indirect_call_null_pointer",
            9 => "indirect_call_type_mismatch",
            10 => "integer_division_by_zero",
            11 => "integer_overflow",
            12 => "invalid_conversion_to_integer",
            13 => "check_module_exit_code",
            14 => "call_listener_before",
            15 => "call_listener_after",
            16 => "call_go_module_function_with_listener",
            17 => "call_go_function_with_listener",
            18 => "table_grow",
            19 => "ref_func",
            20 => "memory_wait32",
            21 => "memory_wait64",
            22 => "memory_notify",
            23 => "unaligned_atomic",
            24 => "fuel_exhausted",
            25 => "policy_denied",
            26 => "memory_fault",
            _ => panic!("unknown exit code {}", self.0),
        }
    }

    pub const fn call_go_module_function_with_index(index: usize, with_listener: bool) -> Self {
        if with_listener {
            Self(Self::CALL_GO_MODULE_FUNCTION_WITH_LISTENER.0 | ((index as u32) << 8))
        } else {
            Self(Self::CALL_GO_MODULE_FUNCTION.0 | ((index as u32) << 8))
        }
    }

    pub const fn call_go_function_with_index(index: usize, with_listener: bool) -> Self {
        if with_listener {
            Self(Self::CALL_GO_FUNCTION_WITH_LISTENER.0 | ((index as u32) << 8))
        } else {
            Self(Self::CALL_GO_FUNCTION.0 | ((index as u32) << 8))
        }
    }
}

pub const fn go_function_index_from_exit_code(exit_code: ExitCode) -> usize {
    (exit_code.raw() >> 8) as usize
}

impl fmt::Display for ExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for ExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ExitCode({})", self)
    }
}

#[cfg(test)]
mod tests {
    use super::{go_function_index_from_exit_code, ExitCode, EXIT_CODE_MASK, EXIT_CODE_MAX};

    #[test]
    fn exit_code_fits_within_byte() {
        assert!(EXIT_CODE_MAX < EXIT_CODE_MASK);
    }

    #[test]
    fn exit_code_strings_match_go_names() {
        assert_eq!(ExitCode::OK.to_string(), "ok");
        assert_eq!(ExitCode::UNALIGNED_ATOMIC.to_string(), "unaligned_atomic");
        assert_eq!(ExitCode::MEMORY_FAULT.to_string(), "memory_fault");
    }

    #[test]
    fn indexed_exit_codes_encode_function_index() {
        let code = ExitCode::call_go_module_function_with_index(42, true);
        assert_eq!(
            code.raw() & EXIT_CODE_MASK,
            ExitCode::CALL_GO_MODULE_FUNCTION_WITH_LISTENER.raw()
        );
        assert_eq!(go_function_index_from_exit_code(code), 42);

        let code = ExitCode::call_go_function_with_index(7, false);
        assert_eq!(
            code.raw() & EXIT_CODE_MASK,
            ExitCode::CALL_GO_FUNCTION.raw()
        );
        assert_eq!(go_function_index_from_exit_code(code), 7);
    }
}
