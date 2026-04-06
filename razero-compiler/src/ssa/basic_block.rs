use std::collections::HashMap;
use std::fmt;

use crate::ssa::instructions::InstructionId;
use crate::ssa::vs::{Value, Values, Variable};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BasicBlockId(pub u32);

pub const BASIC_BLOCK_ID_RETURN_BLOCK: BasicBlockId = BasicBlockId(u32::MAX);
pub type BasicBlock = BasicBlockId;

impl BasicBlockId {
    pub const fn is_entry(self) -> bool {
        self.0 == 0
    }

    pub const fn is_return(self) -> bool {
        self.0 == BASIC_BLOCK_ID_RETURN_BLOCK.0
    }

    pub fn name(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for BasicBlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_return() {
            f.write_str("blk_ret")
        } else {
            write!(f, "blk{}", self.0)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BasicBlockPredecessorInfo {
    pub block: BasicBlockId,
    pub branch: InstructionId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownValue {
    pub variable: Variable,
    pub value: Value,
}

#[derive(Clone, Debug)]
pub struct BasicBlockData {
    pub id: BasicBlockId,
    pub root_instr: Option<InstructionId>,
    pub tail_instr: Option<InstructionId>,
    pub params: Values,
    pub preds: Vec<BasicBlockPredecessorInfo>,
    pub succs: Vec<BasicBlockId>,
    pub single_pred: Option<BasicBlockId>,
    pub last_definitions: HashMap<Variable, Value>,
    pub unknown_values: Vec<UnknownValue>,
    pub invalid: bool,
    pub sealed: bool,
    pub loop_header: bool,
    pub loop_nesting_forest_children: Vec<BasicBlockId>,
    pub reverse_post_order: i32,
    pub visited: i32,
    pub child: Option<BasicBlockId>,
    pub sibling: Option<BasicBlockId>,
}

impl BasicBlockData {
    pub fn new(id: BasicBlockId) -> Self {
        Self {
            id,
            root_instr: None,
            tail_instr: None,
            params: Values::new(),
            preds: Vec::new(),
            succs: Vec::new(),
            single_pred: None,
            last_definitions: HashMap::new(),
            unknown_values: Vec::new(),
            invalid: false,
            sealed: false,
            loop_header: false,
            loop_nesting_forest_children: Vec::new(),
            reverse_post_order: -1,
            visited: 0,
            child: None,
            sibling: None,
        }
    }

    pub fn add_param(&mut self, value: Value) {
        self.params.push(value);
    }

    pub fn params_len(&self) -> usize {
        self.params.len()
    }

    pub fn param(&self, index: usize) -> Value {
        self.params.as_slice()[index]
    }

    pub fn preds_len(&self) -> usize {
        self.preds.len()
    }

    pub fn succs_len(&self) -> usize {
        self.succs.len()
    }

    pub fn valid(&self) -> bool {
        !self.invalid
    }
}

impl Default for BasicBlockData {
    fn default() -> Self {
        Self::new(BasicBlockId(0))
    }
}
