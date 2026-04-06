pub mod lower;
pub mod misc;
pub mod sort_id;

use crate::ssa::{BasicBlock, Builder, Signature, SignatureId, Type, Value, Variable};
use crate::wazevoapi::ModuleContextOffsetData;
use misc::function_index_to_func_ref;
use razero_wasm::module::{self as wasm, Code, FunctionType, Module, ValueType};

pub use misc::function_index_to_func_ref as FunctionIndexToFuncRef;

pub const EXECUTION_CONTEXT_PTR_TYP: Type = Type::I64;
pub const MODULE_CONTEXT_PTR_TYP: Type = Type::I64;

#[derive(Clone, Debug, Default)]
struct LoweringState {
    values: Vec<Value>,
    control_frames: Vec<ControlFrame>,
    unreachable: bool,
    unreachable_depth: usize,
    pc: usize,
}

#[derive(Clone, Debug)]
struct ControlFrame {
    kind: ControlFrameKind,
    original_stack_len_without_param: usize,
    block: Option<BasicBlock>,
    following_block: BasicBlock,
    block_type: FunctionType,
    cloned_args: Vec<Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlFrameKind {
    Function,
    Loop,
    IfWithElse,
    IfWithoutElse,
    Block,
}

impl ControlFrame {
    fn is_loop(&self) -> bool {
        self.kind == ControlFrameKind::Loop
    }
}

pub struct Compiler<'a> {
    module: &'a Module,
    ssa_builder: Builder,
    offset: Option<ModuleContextOffsetData>,
    signatures: Vec<Signature>,
    listener_signatures: Vec<(Signature, Signature)>,
    _ensure_termination: bool,
    _fuel_enabled: bool,
    _memory_isolation_enabled: bool,
    wasm_local_to_variable: Vec<Variable>,
    wasm_local_function_index: wasm::Index,
    wasm_function_type_index: wasm::Index,
    wasm_function_typ: FunctionType,
    wasm_function_local_types: Vec<ValueType>,
    wasm_function_body: Vec<u8>,
    wasm_function_body_offset_in_code_section: u64,
    need_listener: bool,
    need_source_offset_info: bool,
    lowering_state: LoweringState,
    exec_ctx_ptr_value: Value,
    module_ctx_ptr_value: Value,
}

impl<'a> Compiler<'a> {
    pub fn new(
        module: &'a Module,
        ssa_builder: Builder,
        offset: Option<ModuleContextOffsetData>,
        ensure_termination: bool,
        listener_on: bool,
        source_info: bool,
        fuel_enabled: bool,
        memory_isolation_enabled: bool,
    ) -> Self {
        let mut compiler = Self {
            module,
            ssa_builder,
            offset,
            signatures: Vec::new(),
            listener_signatures: Vec::new(),
            _ensure_termination: ensure_termination,
            _fuel_enabled: fuel_enabled,
            _memory_isolation_enabled: memory_isolation_enabled,
            wasm_local_to_variable: Vec::new(),
            wasm_local_function_index: 0,
            wasm_function_type_index: 0,
            wasm_function_typ: FunctionType::default(),
            wasm_function_local_types: Vec::new(),
            wasm_function_body: Vec::new(),
            wasm_function_body_offset_in_code_section: 0,
            need_listener: false,
            need_source_offset_info: source_info,
            lowering_state: LoweringState::default(),
            exec_ctx_ptr_value: Value::INVALID,
            module_ctx_ptr_value: Value::INVALID,
        };
        compiler.declare_signatures(listener_on);
        compiler
    }

    fn declare_signatures(&mut self, listener_on: bool) {
        self.signatures.clear();
        self.listener_signatures.clear();

        for (index, wasm_sig) in self.module.type_section.iter().enumerate() {
            let mut sig = signature_for_wasm_function_type(wasm_sig);
            sig.id = SignatureId(index as u32);
            self.ssa_builder.declare_signature(sig.clone());
            self.signatures.push(sig);

            if listener_on {
                let (mut before, mut after) = signature_for_listener(wasm_sig);
                let base = self.module.type_section.len() as u32;
                before.id = SignatureId(index as u32 + base);
                after.id = SignatureId(index as u32 + base * 2);
                self.ssa_builder.declare_signature(before.clone());
                self.ssa_builder.declare_signature(after.clone());
                self.listener_signatures.push((before, after));
            }
        }
    }

    pub fn init(
        &mut self,
        index: wasm::Index,
        type_index: wasm::Index,
        function_type: &FunctionType,
        local_types: &[ValueType],
        body: &[u8],
        need_listener: bool,
        body_offset_in_code_section: u64,
    ) {
        let signature = self
            .signatures
            .get(type_index as usize)
            .cloned()
            .unwrap_or_else(|| panic!("missing signature for type index {type_index}"));
        self.ssa_builder.init(signature);
        self.lowering_state.reset();
        self.wasm_local_function_index = index;
        self.wasm_function_type_index = type_index;
        self.wasm_function_typ = function_type.clone();
        self.wasm_function_local_types.clear();
        self.wasm_function_local_types
            .extend_from_slice(local_types);
        self.wasm_function_body.clear();
        self.wasm_function_body.extend_from_slice(body);
        self.wasm_function_body_offset_in_code_section = body_offset_in_code_section;
        self.need_listener = need_listener;
    }

    pub fn init_with_module_function(&mut self, function_index: wasm::Index, need_listener: bool) {
        assert!(
            function_index >= self.module.import_function_count,
            "imported functions are not supported by the Rust frontend yet"
        );
        let defined_index = (function_index - self.module.import_function_count) as usize;
        let type_index = *self
            .module
            .function_section
            .get(defined_index)
            .unwrap_or_else(|| {
                panic!("missing function section entry for function {function_index}")
            });
        let code: &Code = self
            .module
            .code_section
            .get(defined_index)
            .unwrap_or_else(|| panic!("missing code section entry for function {function_index}"));
        let function_type = self
            .module
            .type_section
            .get(type_index as usize)
            .unwrap_or_else(|| panic!("missing type section entry for type {type_index}"));
        self.init(
            function_index,
            type_index,
            function_type,
            &code.local_types,
            &code.body,
            need_listener,
            code.body_offset_in_code_section,
        );
    }

    pub fn lower_to_ssa(&mut self) {
        let entry_block = self.ssa_builder.allocate_basic_block();
        self.ssa_builder.set_current_block(entry_block);

        self.exec_ctx_ptr_value = self.append_block_param(entry_block, EXECUTION_CONTEXT_PTR_TYP);
        self.module_ctx_ptr_value = self.append_block_param(entry_block, MODULE_CONTEXT_PTR_TYP);
        self.ssa_builder
            .annotate_value(self.exec_ctx_ptr_value, "exec_ctx");
        self.ssa_builder
            .annotate_value(self.module_ctx_ptr_value, "module_ctx");

        let params = self.wasm_function_typ.params.clone();
        for (i, ty) in params.into_iter().enumerate() {
            let ssa_ty = wasm_type_to_ssa_type(ty);
            let variable = self.ssa_builder.declare_variable(ssa_ty);
            let value = self.append_block_param(entry_block, ssa_ty);
            self.ssa_builder
                .define_variable(variable, value, entry_block);
            self.set_wasm_local_variable(i as wasm::Index, variable);
        }

        self.declare_wasm_locals();
        self.lower_body(entry_block);
    }

    fn append_block_param(&mut self, block: BasicBlock, ty: Type) -> Value {
        let value = self.ssa_builder.allocate_value(ty);
        self.ssa_builder.block_mut(block).add_param(value);
        value
    }

    fn local_variable(&self, index: wasm::Index) -> Variable {
        self.wasm_local_to_variable[index as usize]
    }

    fn set_wasm_local_variable(&mut self, index: wasm::Index, variable: Variable) {
        let index = index as usize;
        if index >= self.wasm_local_to_variable.len() {
            self.wasm_local_to_variable
                .resize(index + 1, Variable::default());
        }
        self.wasm_local_to_variable[index] = variable;
    }

    fn declare_wasm_locals(&mut self) {
        let local_count = self.wasm_function_typ.params.len() as wasm::Index;
        let locals = self.wasm_function_local_types.clone();
        for (i, ty) in locals.into_iter().enumerate() {
            let ssa_ty = wasm_type_to_ssa_type(ty);
            let variable = self.ssa_builder.declare_variable(ssa_ty);
            self.set_wasm_local_variable(local_count + i as wasm::Index, variable);
            self.ssa_builder.insert_zero_value(ssa_ty);
        }
    }

    fn add_block_params_from_wasm_types(&mut self, types: &[ValueType], block: BasicBlock) {
        for &ty in types {
            self.append_block_param(block, wasm_type_to_ssa_type(ty));
        }
    }

    pub fn format(&self) -> String {
        self.ssa_builder.format()
    }

    pub fn builder(&self) -> &Builder {
        &self.ssa_builder
    }

    pub fn builder_mut(&mut self) -> &mut Builder {
        &mut self.ssa_builder
    }
}

pub fn signature_for_wasm_function_type(typ: &FunctionType) -> Signature {
    let mut params = Vec::with_capacity(typ.params.len() + 2);
    params.push(EXECUTION_CONTEXT_PTR_TYP);
    params.push(MODULE_CONTEXT_PTR_TYP);
    params.extend(typ.params.iter().copied().map(wasm_type_to_ssa_type));
    let results = typ
        .results
        .iter()
        .copied()
        .map(wasm_type_to_ssa_type)
        .collect();
    Signature::new(SignatureId(0), params, results)
}

pub fn signature_for_listener(wasm_sig: &FunctionType) -> (Signature, Signature) {
    let mut before_params = Vec::with_capacity(wasm_sig.params.len() + 2);
    before_params.push(Type::I64);
    before_params.push(Type::I32);
    before_params.extend(wasm_sig.params.iter().copied().map(wasm_type_to_ssa_type));

    let mut after_params = Vec::with_capacity(wasm_sig.results.len() + 2);
    after_params.push(Type::I64);
    after_params.push(Type::I32);
    after_params.extend(wasm_sig.results.iter().copied().map(wasm_type_to_ssa_type));

    (
        Signature::new(SignatureId(0), before_params, Vec::new()),
        Signature::new(SignatureId(0), after_params, Vec::new()),
    )
}

pub fn wasm_type_to_ssa_type(value_type: ValueType) -> Type {
    match value_type {
        ValueType::I32 => Type::I32,
        ValueType::I64 | ValueType::FUNCREF | ValueType::EXTERNREF => Type::I64,
        ValueType::F32 => Type::F32,
        ValueType::F64 => Type::F64,
        ValueType::V128 => Type::V128,
        other => panic!("unsupported wasm value type: {}", other.name()),
    }
}

#[allow(non_snake_case)]
pub fn SignatureForWasmFunctionType(typ: &FunctionType) -> Signature {
    signature_for_wasm_function_type(typ)
}

#[allow(non_snake_case)]
pub fn SignatureForListener(wasm_sig: &FunctionType) -> (Signature, Signature) {
    signature_for_listener(wasm_sig)
}

#[allow(non_snake_case)]
pub fn WasmTypeToSSAType(value_type: ValueType) -> Type {
    wasm_type_to_ssa_type(value_type)
}
