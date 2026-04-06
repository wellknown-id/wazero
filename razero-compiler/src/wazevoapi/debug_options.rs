//! Centralized debugging and deterministic-compilation helpers.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

pub const FRONT_END_LOGGING_ENABLED: bool = false;
pub const SSA_LOGGING_ENABLED: bool = false;
pub const REG_ALLOC_LOGGING_ENABLED: bool = false;

pub const PRINT_SSA: bool = false;
pub const PRINT_OPTIMIZED_SSA: bool = false;
pub const PRINT_SSA_TO_BACKEND_IR_LOWERING: bool = false;
pub const PRINT_REGISTER_ALLOCATED: bool = false;
pub const PRINT_FINALIZED_MACHINE_CODE: bool = false;
pub const PRINT_MACHINE_CODE_HEX_PER_FUNCTION_UNMODIFIED: bool = false;
pub const PRINT_MACHINE_CODE_HEX_PER_FUNCTION_DISASSEMBLABLE: bool = false;
pub const PRINT_MACHINE_CODE_HEX_PER_FUNCTION: bool = PRINT_MACHINE_CODE_HEX_PER_FUNCTION_UNMODIFIED
    || PRINT_MACHINE_CODE_HEX_PER_FUNCTION_DISASSEMBLABLE;
pub const PRINT_TARGET: isize = -1;

pub const SSA_VALIDATION_ENABLED: bool = false;

pub const STACK_GUARD_CHECK_ENABLED: bool = false;
pub const STACK_GUARD_CHECK_GUARD_PAGE_SIZE: usize = 8096;

pub const DETERMINISTIC_COMPILATION_VERIFIER_ENABLED: bool = false;
pub const DETERMINISTIC_COMPILATION_VERIFYING_ITER: usize = 5;

pub const NEED_FUNCTION_NAME_IN_CONTEXT: bool = PRINT_SSA
    || PRINT_OPTIMIZED_SSA
    || PRINT_SSA_TO_BACKEND_IR_LOWERING
    || PRINT_REGISTER_ALLOCATED
    || PRINT_FINALIZED_MACHINE_CODE
    || PRINT_MACHINE_CODE_HEX_PER_FUNCTION
    || DETERMINISTIC_COMPILATION_VERIFIER_ENABLED
    || super::perfmap::PERF_MAP_ENABLED;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CurrentFunction {
    index: usize,
    name: String,
}

impl CurrentFunction {
    pub fn new(index: usize, name: impl Into<String>) -> Self {
        Self {
            index,
            name: name.into(),
        }
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

pub fn print_enabled_index(current_function_index: usize) -> bool {
    PRINT_TARGET == -1 || current_function_index == PRINT_TARGET as usize
}

pub fn check_stack_guard_page(stack: &[u8]) {
    assert!(
        stack.len() >= STACK_GUARD_CHECK_GUARD_PAGE_SIZE,
        "stack shorter than guard page: {} < {}",
        stack.len(),
        STACK_GUARD_CHECK_GUARD_PAGE_SIZE
    );

    for index in 0..STACK_GUARD_CHECK_GUARD_PAGE_SIZE {
        if stack[index] != 0 {
            panic!(
                "BUG: stack guard page is corrupted:\n\tguard_page={}\n\tstack={}",
                hex_encode(&stack[..STACK_GUARD_CHECK_GUARD_PAGE_SIZE]),
                hex_encode(&stack[STACK_GUARD_CHECK_GUARD_PAGE_SIZE..])
            );
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeterministicCompilationError {
    pub function_name: String,
    pub scope: String,
    pub old_value: String,
    pub new_value: String,
}

impl fmt::Display for DeterministicCompilationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BUG: Deterministic compilation failed for function{} at scope=\"{}\".\n\n---------- [old] ----------\n{}\n\n---------- [new] ----------\n{}\n",
            self.function_name, self.scope, self.old_value, self.new_value
        )
    }
}

impl Error for DeterministicCompilationError {}

#[derive(Debug)]
pub struct DeterministicCompilationVerifier {
    initial_compilation_done: bool,
    maybe_randomized_indexes: Vec<usize>,
    rng: XorShift64,
    values: HashMap<String, String>,
}

impl DeterministicCompilationVerifier {
    pub fn new(local_functions: usize) -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);
        Self::with_seed(local_functions, seed)
    }

    pub fn randomize_indexes(&mut self) -> &[usize] {
        if !self.initial_compilation_done {
            self.initial_compilation_done = true;
            return &self.maybe_randomized_indexes;
        }

        let len = self.maybe_randomized_indexes.len();
        for i in (1..len).rev() {
            let j = self.rng.next_index(i + 1);
            self.maybe_randomized_indexes.swap(i, j);
        }
        &self.maybe_randomized_indexes
    }

    pub fn verify_or_set(
        &mut self,
        function_name: &str,
        scope: &str,
        new_value: impl Into<String>,
    ) -> Result<(), DeterministicCompilationError> {
        let new_value = new_value.into();
        let key = format!("{function_name}: {scope}");
        if let Some(old_value) = self.values.get(&key) {
            if old_value != &new_value {
                return Err(DeterministicCompilationError {
                    function_name: function_name.to_string(),
                    scope: scope.to_string(),
                    old_value: old_value.clone(),
                    new_value,
                });
            }
            return Ok(());
        }

        self.values.insert(key, new_value);
        Ok(())
    }

    fn with_seed(local_functions: usize, seed: u64) -> Self {
        Self {
            initial_compilation_done: false,
            maybe_randomized_indexes: (0..local_functions).collect(),
            rng: XorShift64::new(seed),
            values: HashMap::new(),
        }
    }
}

#[derive(Debug)]
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next_u64(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }

    fn next_index(&mut self, upper_bound: usize) -> usize {
        (self.next_u64() as usize) % upper_bound
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let high = byte >> 4;
        let low = byte & 0x0f;
        out.push(char::from_digit(high.into(), 16).unwrap());
        out.push(char::from_digit(low.into(), 16).unwrap());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{check_stack_guard_page, print_enabled_index, DeterministicCompilationVerifier};

    #[test]
    fn print_target_disabled_enables_all_indexes() {
        assert!(print_enabled_index(0));
        assert!(print_enabled_index(99));
    }

    #[test]
    fn stack_guard_page_accepts_zeroed_guard() {
        let stack = vec![0; super::STACK_GUARD_CHECK_GUARD_PAGE_SIZE + 32];
        check_stack_guard_page(&stack);
    }

    #[test]
    fn deterministic_verifier_uses_order_then_shuffle() {
        let mut verifier = DeterministicCompilationVerifier::with_seed(6, 1);
        assert_eq!(verifier.randomize_indexes(), &[0, 1, 2, 3, 4, 5]);

        let shuffled = verifier.randomize_indexes().to_vec();
        assert_eq!(shuffled.len(), 6);
        assert_ne!(shuffled, vec![0, 1, 2, 3, 4, 5]);
        let mut sorted = shuffled.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn deterministic_verifier_detects_mismatch() {
        let mut verifier = DeterministicCompilationVerifier::with_seed(1, 7);
        verifier.verify_or_set("f", "scope", "value-a").unwrap();
        let err = verifier.verify_or_set("f", "scope", "value-b").unwrap_err();
        assert_eq!(err.function_name, "f");
        assert_eq!(err.scope, "scope");
        assert_eq!(err.old_value, "value-a");
        assert_eq!(err.new_value, "value-b");
    }
}
