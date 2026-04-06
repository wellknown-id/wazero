#![doc = "Interpreter formatting helpers."]

use crate::operations::{Instruction, OperationKind};

pub fn format_operation(operation: &Instruction) -> String {
    operation.to_string()
}

pub fn format_program(ops: &[Instruction]) -> String {
    let mut rendered = String::from(".entrypoint\n");
    for op in ops {
        if op.kind != OperationKind::Label {
            rendered.push('\t');
        }
        rendered.push_str(&format_operation(op));
        rendered.push('\n');
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::format_program;
    use crate::operations::{Instruction, Label, LabelKind};

    #[test]
    fn matches_go_style_program_rendering() {
        let label = Label::new(LabelKind::Header, 7);
        let program = vec![
            Instruction::label(label),
            Instruction::const_i32(42),
            Instruction::br(Label::new(LabelKind::Return, 0)),
        ];

        assert_eq!(
            ".entrypoint\n.L7\n\tConstI32 0x2a\n\tBr .return\n",
            format_program(&program)
        );
    }
}
