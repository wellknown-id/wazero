use crate::ssa::{InstructionId, Value};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SSAValueDefinition {
    pub value: Value,
    pub instr: Option<InstructionId>,
    pub ref_count: u32,
}

impl SSAValueDefinition {
    pub const fn is_from_instr(&self) -> bool {
        self.instr.is_some()
    }
}
