pub const ENTRYPOINT_SYMBOL: &str = "entrypoint";
pub const AFTER_HOST_CALL_ENTRYPOINT_SYMBOL: &str = "after_go_function_call_entrypoint";

pub fn entry_asm_source() -> &'static str {
    include_str!("abi_entry.S")
}

#[cfg(test)]
mod tests {
    use super::{entry_asm_source, AFTER_HOST_CALL_ENTRYPOINT_SYMBOL, ENTRYPOINT_SYMBOL};

    #[test]
    fn assembly_scaffold_contains_expected_symbols() {
        let asm = entry_asm_source();
        assert!(asm.contains(ENTRYPOINT_SYMBOL));
        assert!(asm.contains(AFTER_HOST_CALL_ENTRYPOINT_SYMBOL));
    }
}
