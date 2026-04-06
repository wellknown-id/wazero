use std::ptr::NonNull;

use crate::ssa::{
    BasicBlock, Builder, FuncRef, InstructionGroupId, InstructionId, Opcode, Signature,
    SourceOffset, Type, Value, ValueInfo,
};

use super::abi::FunctionAbi;
use super::machine::{BackendError, Machine};
use super::vdef::SSAValueDefinition;
use super::{RegType, VReg, VRegId, VREG_ID_NON_RESERVED_BEGIN};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelocationInfo {
    pub offset: i64,
    pub func_ref: FuncRef,
    pub is_tail_call: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceOffsetInfo {
    pub source_offset: SourceOffset,
    pub executable_offset: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompilationOutput {
    pub code: Vec<u8>,
    pub relocations: Vec<RelocationInfo>,
}

pub trait CompilerContext {
    fn ssa_builder(&self) -> &Builder;
    fn format(&self) -> String;
    fn allocate_vreg(&mut self, ty: Type) -> VReg;
    fn value_definition(&self, value: Value) -> SSAValueDefinition;
    fn v_reg_of(&self, value: Value) -> VReg;
    fn type_of(&self, vreg: VReg) -> Type;
    fn match_instr(&self, def: SSAValueDefinition, opcode: Opcode) -> bool;
    fn match_instr_one_of(&self, def: SSAValueDefinition, opcodes: &[Opcode]) -> Opcode;
    fn add_relocation_info(&mut self, func_ref: FuncRef, is_tail_call: bool);
    fn add_source_offset_info(&mut self, executable_offset: i64, source_offset: SourceOffset);
    fn source_offset_info(&self) -> &[SourceOffsetInfo];
    fn emit_byte(&mut self, b: u8);
    fn emit4_bytes(&mut self, b: u32);
    fn emit8_bytes(&mut self, b: u64);
    fn buf(&self) -> &[u8];
    fn buf_mut(&mut self) -> &mut Vec<u8>;
    fn get_function_abi(&mut self, sig: &Signature) -> &FunctionAbi;
}

#[derive(Debug)]
pub struct Compiler<M: Machine> {
    pub(crate) machine: M,
    pub(crate) current_gid: InstructionGroupId,
    pub(crate) ssa_builder: Builder,
    pub(crate) next_vreg_id: VRegId,
    pub(crate) ssa_value_to_vregs: Vec<VReg>,
    pub(crate) ssa_values_info: Vec<ValueInfo>,
    pub(crate) return_vregs: Vec<VReg>,
    pub(crate) var_edges: Vec<[VReg; 2]>,
    pub(crate) var_edge_types: Vec<Type>,
    pub(crate) const_edges: Vec<(InstructionId, VReg)>,
    pub(crate) v_reg_set: Vec<bool>,
    pub(crate) v_reg_ids: Vec<VRegId>,
    pub(crate) temp_regs: Vec<VReg>,
    pub(crate) tmp_vals: Vec<Value>,
    pub(crate) ssa_type_of_vreg_id: Vec<Type>,
    pub(crate) buf: Vec<u8>,
    pub(crate) relocations: Vec<RelocationInfo>,
    pub(crate) source_offsets: Vec<SourceOffsetInfo>,
    pub(crate) abis: Vec<FunctionAbi>,
    pub(crate) arg_result_ints: Vec<u8>,
    pub(crate) arg_result_floats: Vec<u8>,
}

impl<M: Machine + 'static> Compiler<M> {
    fn ensure_ssa_value_slot(&mut self, value: Value) {
        let index = value.id().0 as usize;
        if self.ssa_value_to_vregs.len() <= index {
            self.ssa_value_to_vregs.resize(index + 1, VReg::INVALID);
        }
    }

    pub fn new(machine: M, builder: Builder) -> Box<Self> {
        let (arg_result_ints, arg_result_floats) = {
            let (ints, floats) = machine.args_results_regs();
            (ints.to_vec(), floats.to_vec())
        };
        let mut compiler = Box::new(Self {
            machine,
            current_gid: InstructionGroupId(0),
            ssa_builder: builder,
            next_vreg_id: VREG_ID_NON_RESERVED_BEGIN,
            ssa_value_to_vregs: Vec::new(),
            ssa_values_info: Vec::new(),
            return_vregs: Vec::new(),
            var_edges: Vec::new(),
            var_edge_types: Vec::new(),
            const_edges: Vec::new(),
            v_reg_set: Vec::new(),
            v_reg_ids: Vec::new(),
            temp_regs: Vec::new(),
            tmp_vals: Vec::new(),
            ssa_type_of_vreg_id: Vec::new(),
            buf: Vec::new(),
            relocations: Vec::new(),
            source_offsets: Vec::new(),
            abis: Vec::new(),
            arg_result_ints,
            arg_result_floats,
        });

        let ptr = NonNull::from(compiler.as_mut() as &mut dyn CompilerContext);
        compiler.machine.set_compiler(ptr);
        compiler
    }

    pub fn compile(&mut self) -> Result<CompilationOutput, BackendError> {
        self.lower();
        self.reg_alloc();
        self.finalize()?;
        Ok(CompilationOutput {
            code: self.buf.clone(),
            relocations: self.relocations.clone(),
        })
    }

    pub fn lower(&mut self) {
        self.assign_virtual_registers();
        let sig = self
            .ssa_builder
            .signature()
            .expect("backend lowering requires an SSA signature")
            .clone();
        let abi = self.get_function_abi(&sig).clone();
        self.machine.set_current_abi(abi);
        self.machine
            .start_lowering_function(self.ssa_builder.block_id_max());
        self.lower_blocks();
    }

    pub fn reg_alloc(&mut self) {
        self.machine.reg_alloc();
    }

    pub fn finalize(&mut self) -> Result<(), BackendError> {
        self.machine.post_reg_alloc();
        self.machine.encode()
    }

    pub fn init(&mut self) {
        self.current_gid = InstructionGroupId(0);
        self.next_vreg_id = VREG_ID_NON_RESERVED_BEGIN;
        self.return_vregs.clear();
        self.machine.reset();
        self.var_edges.clear();
        self.var_edge_types.clear();
        self.const_edges.clear();
        self.buf.clear();
        self.source_offsets.clear();
        self.relocations.clear();
    }

    pub fn disable_stack_check(&mut self) {
        self.machine.disable_stack_check();
    }

    pub fn set_current_group_id(&mut self, gid: InstructionGroupId) {
        self.current_gid = gid;
    }

    pub(crate) fn reverse_post_order_blocks(&mut self) -> Vec<BasicBlock> {
        let mut blocks = Vec::new();
        let mut next = self.ssa_builder.block_iterator_reverse_post_order_begin();
        while let Some(block) = next {
            blocks.push(block);
            next = self.ssa_builder.block_iterator_reverse_post_order_next();
        }
        blocks
    }

    pub(crate) fn assign_virtual_registers(&mut self) {
        self.ssa_values_info = self.ssa_builder.values_info().to_vec();
        self.return_vregs.clear();

        let blocks = self.reverse_post_order_blocks();
        for blk in blocks {
            let params = self.ssa_builder.block(blk).params.as_slice().to_vec();
            for param in params {
                let vreg = self.allocate_vreg(param.ty());
                self.ensure_ssa_value_slot(param);
                self.ssa_value_to_vregs[param.id().0 as usize] = vreg;
                self.ssa_type_of_vreg_id.resize(
                    self.ssa_type_of_vreg_id.len().max(vreg.id() as usize + 1),
                    Type::Invalid,
                );
                self.ssa_type_of_vreg_id[vreg.id() as usize] = param.ty();
            }

            let mut cur = self.ssa_builder.block(blk).root_instr;
            while let Some(instr_id) = cur {
                let instr = self.ssa_builder.instruction(instr_id).clone();
                let (ret, rest) = instr.returns();
                if ret.valid() {
                    let vreg = self.allocate_vreg(ret.ty());
                    self.ensure_ssa_value_slot(ret);
                    self.ssa_value_to_vregs[ret.id().0 as usize] = vreg;
                    self.ssa_type_of_vreg_id[vreg.id() as usize] = ret.ty();
                }
                for value in rest.iter() {
                    let vreg = self.allocate_vreg(value.ty());
                    self.ensure_ssa_value_slot(value);
                    self.ssa_value_to_vregs[value.id().0 as usize] = vreg;
                    self.ssa_type_of_vreg_id[vreg.id() as usize] = value.ty();
                }
                cur = instr.next;
            }
        }

        let return_params = self
            .ssa_builder
            .block(self.ssa_builder.return_block())
            .params
            .as_slice()
            .to_vec();
        for param in return_params {
            let vreg = self.allocate_vreg(param.ty());
            self.return_vregs.push(vreg);
            self.ssa_type_of_vreg_id[vreg.id() as usize] = param.ty();
        }
    }

    pub fn machine(&self) -> &M {
        &self.machine
    }

    pub fn machine_mut(&mut self) -> &mut M {
        &mut self.machine
    }
}

impl<M: Machine> CompilerContext for Compiler<M> {
    fn ssa_builder(&self) -> &Builder {
        &self.ssa_builder
    }

    fn format(&self) -> String {
        self.machine.format()
    }

    fn allocate_vreg(&mut self, ty: Type) -> VReg {
        let reg = VReg(self.next_vreg_id as u64).set_reg_type(RegType::of(ty));
        let id = reg.id() as usize;
        if self.ssa_type_of_vreg_id.len() <= id {
            self.ssa_type_of_vreg_id.resize(id + 1, Type::Invalid);
        }
        self.ssa_type_of_vreg_id[id] = ty;
        self.next_vreg_id += 1;
        reg
    }

    fn value_definition(&self, value: Value) -> SSAValueDefinition {
        SSAValueDefinition {
            value,
            instr: self
                .ssa_builder
                .instruction_of_value(value)
                .and_then(|instr| instr.id),
            ref_count: self
                .ssa_values_info
                .get(value.id().0 as usize)
                .map_or(0, |info| info.ref_count),
        }
    }

    fn v_reg_of(&self, value: Value) -> VReg {
        self.ssa_value_to_vregs[value.id().0 as usize]
    }

    fn type_of(&self, vreg: VReg) -> Type {
        self.ssa_type_of_vreg_id
            .get(vreg.id() as usize)
            .copied()
            .unwrap_or(Type::Invalid)
    }

    fn match_instr(&self, def: SSAValueDefinition, opcode: Opcode) -> bool {
        let Some(instr_id) = def.instr else {
            return false;
        };
        let instr = self.ssa_builder.instruction(instr_id);
        instr.opcode == opcode && instr.gid == self.current_gid && def.ref_count < 2
    }

    fn match_instr_one_of(&self, def: SSAValueDefinition, opcodes: &[Opcode]) -> Opcode {
        let Some(instr_id) = def.instr else {
            return Opcode::Invalid;
        };
        let instr = self.ssa_builder.instruction(instr_id);
        if instr.gid != self.current_gid || def.ref_count >= 2 {
            return Opcode::Invalid;
        }
        if opcodes.contains(&instr.opcode) {
            instr.opcode
        } else {
            Opcode::Invalid
        }
    }

    fn add_relocation_info(&mut self, func_ref: FuncRef, is_tail_call: bool) {
        self.relocations.push(RelocationInfo {
            offset: self.buf.len() as i64,
            func_ref,
            is_tail_call,
        });
    }

    fn add_source_offset_info(&mut self, executable_offset: i64, source_offset: SourceOffset) {
        self.source_offsets.push(SourceOffsetInfo {
            source_offset,
            executable_offset,
        });
        self.machine
            .add_source_offset_info(executable_offset, source_offset);
    }

    fn source_offset_info(&self) -> &[SourceOffsetInfo] {
        &self.source_offsets
    }

    fn emit_byte(&mut self, b: u8) {
        self.buf.push(b);
    }

    fn emit4_bytes(&mut self, b: u32) {
        self.buf.extend_from_slice(&b.to_le_bytes());
    }

    fn emit8_bytes(&mut self, b: u64) {
        self.buf.extend_from_slice(&b.to_le_bytes());
    }

    fn buf(&self) -> &[u8] {
        &self.buf
    }

    fn buf_mut(&mut self) -> &mut Vec<u8> {
        &mut self.buf
    }

    fn get_function_abi(&mut self, sig: &Signature) -> &FunctionAbi {
        let index = sig.id.0 as usize;
        if self.abis.len() <= index {
            self.abis.resize(index + 1, FunctionAbi::default());
        }
        if !self.abis[index].initialized {
            self.abis[index].init(sig, &self.arg_result_ints, &self.arg_result_floats);
        }
        &self.abis[index]
    }
}

#[cfg(test)]
mod tests {
    use std::ptr::NonNull;

    use super::{Compiler, CompilerContext};
    use crate::backend::{BackendError, FunctionAbi, Machine, RegType, RelocationInfo, VReg};
    use crate::ssa::{
        BasicBlock, BasicBlockId, Builder, Instruction, InstructionGroupId, Opcode, Signature,
        SignatureId, SourceOffset, Type, Value,
    };
    use crate::wazevoapi::ExitCode;

    #[derive(Default, Debug)]
    struct MockMachine {
        arg_result_ints: Vec<u8>,
        arg_result_floats: Vec<u8>,
    }

    impl Machine for MockMachine {
        fn start_lowering_function(&mut self, _max_block_id: BasicBlockId) {}
        fn link_adjacent_blocks(&mut self, _prev: BasicBlock, _next: BasicBlock) {}
        fn start_block(&mut self, _block: BasicBlock) {}
        fn end_block(&mut self) {}
        fn flush_pending_instructions(&mut self) {}
        fn disable_stack_check(&mut self) {}
        fn set_current_abi(&mut self, _abi: FunctionAbi) {}
        fn set_compiler(&mut self, _compiler: NonNull<dyn CompilerContext>) {}
        fn lower_single_branch(&mut self, _branch: &Instruction) {}
        fn lower_conditional_branch(&mut self, _branch: &Instruction) {}
        fn lower_instr(&mut self, _instruction: &Instruction) {}
        fn reset(&mut self) {}
        fn insert_move(&mut self, _dst: VReg, _src: VReg, _ty: Type) {}
        fn insert_return(&mut self) {}
        fn insert_load_constant_block_arg(&mut self, _instr: &Instruction, _dst: VReg) {}
        fn format(&self) -> String {
            "mock".into()
        }
        fn reg_alloc(&mut self) {}
        fn post_reg_alloc(&mut self) {}
        fn resolve_relocations(
            &mut self,
            _ref_to_binary_offset: &[i32],
            _imported_fns: usize,
            _executable: &mut [u8],
            _relocations: &[RelocationInfo],
            _call_trampoline_island_offsets: &[i32],
        ) {
        }
        fn encode(&mut self) -> Result<(), BackendError> {
            Ok(())
        }
        fn compile_host_function_trampoline(
            &mut self,
            _exit_code: ExitCode,
            _sig: &Signature,
            _need_module_context_ptr: bool,
        ) -> Vec<u8> {
            Vec::new()
        }
        fn compile_stack_grow_call_sequence(&mut self) -> Vec<u8> {
            Vec::new()
        }
        fn compile_entry_preamble(
            &mut self,
            _signature: &Signature,
            _use_host_stack: bool,
        ) -> Vec<u8> {
            Vec::new()
        }
        fn lower_params(&mut self, _params: &[Value]) {}
        fn lower_returns(&mut self, _returns: &[Value]) {}
        fn args_results_regs(&self) -> (&[u8], &[u8]) {
            (&self.arg_result_ints, &self.arg_result_floats)
        }
        fn call_trampoline_island_info(
            &self,
            _num_functions: usize,
        ) -> Result<(usize, usize), BackendError> {
            Ok((0, 0))
        }
        fn add_source_offset_info(
            &mut self,
            _executable_offset: i64,
            _source_offset: SourceOffset,
        ) {
        }
    }

    #[test]
    fn compiler_caches_abi_and_appends_little_endian_bytes() {
        let mut builder = Builder::new();
        builder.init(Signature::new(
            SignatureId(2),
            vec![Type::I32, Type::F64],
            vec![Type::I64],
        ));

        let mut compiler = Compiler::new(
            MockMachine {
                arg_result_ints: vec![1, 2],
                arg_result_floats: vec![11],
            },
            builder,
        );

        let sig = compiler.ssa_builder().signature().unwrap().clone();
        let abi_ptr = compiler.get_function_abi(&sig) as *const _;
        let abi_ptr_again = compiler.get_function_abi(&sig) as *const _;
        assert_eq!(abi_ptr, abi_ptr_again);

        compiler.emit_byte(0xaa);
        compiler.emit4_bytes(0x1122_3344);
        compiler.emit8_bytes(0x0102_0304_0506_0708);
        assert_eq!(
            compiler.buf(),
            &[0xaa, 0x44, 0x33, 0x22, 0x11, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
    }

    #[test]
    fn compiler_matches_single_use_instruction_in_current_group() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let entry = builder.allocate_basic_block();
        builder.set_current_block(entry);
        builder.reverse_post_ordered_blocks.push(entry);

        let lhs = builder.allocate_value(Type::I32);
        let rhs = builder.allocate_value(Type::I32);
        let mut instr = builder.allocate_instruction().as_iadd(lhs, rhs);
        instr.gid = InstructionGroupId(9);
        let instr_id = builder.insert_instruction(instr);
        let ret_id = builder.instruction(instr_id).return_().id().0 as usize;
        builder.values_info.resize(ret_id + 1, Default::default());
        builder.values_info[ret_id].ref_count = 1;

        let compiler = Compiler::new(MockMachine::default(), builder);
        let def = compiler.value_definition(compiler.ssa_builder().instruction(instr_id).return_());
        assert!(def.is_from_instr());
        assert_eq!(
            compiler.type_of(VReg(0).set_reg_type(RegType::Int)),
            Type::Invalid
        );

        let mut compiler = compiler;
        compiler.set_current_group_id(InstructionGroupId(9));
        compiler.ssa_values_info = compiler.ssa_builder.values_info().to_vec();
        assert!(compiler.match_instr(def, Opcode::Iadd));
        assert_eq!(
            compiler.match_instr_one_of(def, &[Opcode::Isub, Opcode::Iadd]),
            Opcode::Iadd
        );
    }

    #[test]
    fn assign_virtual_registers_covers_params_results_and_return_block() {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let entry = builder.allocate_basic_block();
        builder.set_current_block(entry);
        builder.reverse_post_ordered_blocks.push(entry);

        let param = builder.allocate_value(Type::I64);
        builder.block_mut(entry).add_param(param);
        let add = builder.allocate_instruction().as_iadd(param, param);
        let add_id = builder.insert_instruction(add);
        let ret_param = builder.allocate_value(Type::I64);
        builder
            .block_mut(builder.return_block())
            .add_param(ret_param);

        let mut compiler = Compiler::new(MockMachine::default(), builder);
        compiler.assign_virtual_registers();

        let add_ret = compiler.ssa_builder.instruction(add_id).return_();
        assert!(compiler.v_reg_of(param).valid());
        assert!(compiler.v_reg_of(add_ret).valid());
        assert_eq!(compiler.return_vregs.len(), 1);
        assert_eq!(compiler.type_of(compiler.v_reg_of(param)), Type::I64);
    }
}
