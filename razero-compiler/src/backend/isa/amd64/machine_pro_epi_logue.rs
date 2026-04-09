use super::instr::Amd64Instr;
use super::machine::Amd64Machine;
use super::operands::Operand;
use super::reg::{vreg_for_real_reg, RBP, RSP};

pub fn append_prologue(machine: &mut Amd64Machine) {
    machine.push(Amd64Instr::push64(Operand::reg(vreg_for_real_reg(RBP))));
    machine.push(Amd64Instr::mov_rr(
        vreg_for_real_reg(RSP),
        vreg_for_real_reg(RBP),
        true,
    ));
}

pub fn append_epilogue(machine: &mut Amd64Machine) {
    machine.push(Amd64Instr::mov_rr(
        vreg_for_real_reg(RBP),
        vreg_for_real_reg(RSP),
        true,
    ));
    machine.push(Amd64Instr::pop64(vreg_for_real_reg(RBP)));
    machine.push(Amd64Instr::ret());
}

#[cfg(test)]
mod tests {
    use super::{append_epilogue, append_prologue};
    use crate::backend::isa::amd64::machine::Amd64Machine;
    use crate::backend::machine::Machine;
    use crate::ssa::BasicBlockId;

    #[test]
    fn prologue_and_epilogue_emit_expected_shapes() {
        let mut m = Amd64Machine::new();
        m.start_lowering_function(BasicBlockId(0));
        m.start_block(BasicBlockId(0));
        append_prologue(&mut m);
        append_epilogue(&mut m);
        let text = m.format();
        assert!(text.contains("pushq %rbp"));
        assert!(text.contains("movq %rsp, %rbp"));
        assert!(text.contains("movq %rbp, %rsp"));
        assert!(text.contains("popq %rbp"));
    }
}
