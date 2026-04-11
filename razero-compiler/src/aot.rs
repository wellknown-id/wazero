#![doc = "AOT-exportable compiler metadata.\n\nThe versioned Rust AOT packaging contract is documented in `../AOT_PACKAGING_ABI.md`."]

use std::fmt::{Display, Formatter};
use std::io::{Cursor, Read};
use std::mem::size_of;

use razero_wasm::module::{
    ElementMode, Export, ExternType, GlobalType, Import, ImportDesc, Index, Memory, Module,
    ModuleId, RefType, Table, ValueType,
};

use crate::backend::RelocationInfo;
use crate::call_engine::ExecutionContext;
use crate::wazevoapi::offsetdata::{
    EXECUTION_CONTEXT_OFFSET_CALLER_MODULE_CONTEXT_PTR,
    EXECUTION_CONTEXT_OFFSET_CHECK_MODULE_EXIT_CODE_TRAMPOLINE_ADDRESS,
    EXECUTION_CONTEXT_OFFSET_EXIT_CODE_OFFSET,
    EXECUTION_CONTEXT_OFFSET_FRAME_POINTER_BEFORE_GO_CALL, EXECUTION_CONTEXT_OFFSET_FUEL,
    EXECUTION_CONTEXT_OFFSET_GO_CALL_RETURN_ADDRESS,
    EXECUTION_CONTEXT_OFFSET_GO_FUNCTION_CALL_CALLEE_MODULE_CONTEXT_OPAQUE,
    EXECUTION_CONTEXT_OFFSET_GO_RETURN_ADDRESS, EXECUTION_CONTEXT_OFFSET_MEMMOVE_ADDRESS,
    EXECUTION_CONTEXT_OFFSET_MEMORY_GROW_TRAMPOLINE_ADDRESS,
    EXECUTION_CONTEXT_OFFSET_MEMORY_NOTIFY_TRAMPOLINE_ADDRESS,
    EXECUTION_CONTEXT_OFFSET_MEMORY_WAIT32_TRAMPOLINE_ADDRESS,
    EXECUTION_CONTEXT_OFFSET_MEMORY_WAIT64_TRAMPOLINE_ADDRESS,
    EXECUTION_CONTEXT_OFFSET_ORIGINAL_FRAME_POINTER,
    EXECUTION_CONTEXT_OFFSET_ORIGINAL_STACK_POINTER,
    EXECUTION_CONTEXT_OFFSET_REF_FUNC_TRAMPOLINE_ADDRESS,
    EXECUTION_CONTEXT_OFFSET_SAVED_REGISTERS_BEGIN, EXECUTION_CONTEXT_OFFSET_STACK_BOTTOM_PTR,
    EXECUTION_CONTEXT_OFFSET_STACK_GROW_CALL_TRAMPOLINE_ADDRESS,
    EXECUTION_CONTEXT_OFFSET_STACK_GROW_REQUIRED_SIZE,
    EXECUTION_CONTEXT_OFFSET_STACK_POINTER_BEFORE_GO_CALL,
    EXECUTION_CONTEXT_OFFSET_TABLE_GROW_TRAMPOLINE_ADDRESS,
};
use crate::wazevoapi::{ExitCode, ModuleContextOffsetData};

/// Magic header for the serialized Razero AOT metadata sidecar.
pub const AOT_METADATA_MAGIC: &[u8; 8] = b"RAZEROAT";
/// Version of the execution-context layout embedded in [`AotExecutionContextMetadata`].
pub const EXECUTION_CONTEXT_ABI_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AotTargetArchitecture {
    X86_64,
    Aarch64,
    Unknown,
}

impl AotTargetArchitecture {
    pub const fn current() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            Self::X86_64
        }
        #[cfg(target_arch = "aarch64")]
        {
            Self::Aarch64
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Self::Unknown
        }
    }

    const fn to_byte(self) -> u8 {
        match self {
            Self::X86_64 => 1,
            Self::Aarch64 => 2,
            Self::Unknown => 255,
        }
    }

    const fn from_byte(value: u8) -> Self {
        match value {
            1 => Self::X86_64,
            2 => Self::Aarch64,
            _ => Self::Unknown,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AotTargetOperatingSystem {
    Linux,
    MacOs,
    Windows,
    Unknown,
}

impl AotTargetOperatingSystem {
    pub const fn current() -> Self {
        #[cfg(target_os = "linux")]
        {
            Self::Linux
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Self::Unknown
        }
    }

    const fn to_byte(self) -> u8 {
        match self {
            Self::Linux => 1,
            Self::MacOs => 2,
            Self::Windows => 3,
            Self::Unknown => 255,
        }
    }

    const fn from_byte(value: u8) -> Self {
        match value {
            1 => Self::Linux,
            2 => Self::MacOs,
            3 => Self::Windows,
            _ => Self::Unknown,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::MacOs => "macos",
            Self::Windows => "windows",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AotTarget {
    pub architecture: AotTargetArchitecture,
    pub operating_system: AotTargetOperatingSystem,
}

impl Default for AotTarget {
    fn default() -> Self {
        Self::current()
    }
}

impl AotTarget {
    pub const fn current() -> Self {
        Self {
            architecture: AotTargetArchitecture::current(),
            operating_system: AotTargetOperatingSystem::current(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AotFunctionMetadata {
    pub local_function_index: Index,
    pub wasm_function_index: Index,
    pub type_index: Index,
    pub executable_offset: usize,
    pub executable_len: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AotRelocationMetadata {
    pub source_wasm_function_index: Index,
    pub target_function_index: Index,
    pub executable_offset: i64,
    pub is_tail_call: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AotSourceMapEntry {
    pub wasm_binary_offset: u64,
    pub executable_offset: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AotFunctionTypeMetadata {
    pub params: Vec<ValueType>,
    pub results: Vec<ValueType>,
    pub param_num_in_u64: usize,
    pub result_num_in_u64: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AotGlobalTypeMetadata {
    pub val_type: ValueType,
    pub mutable: bool,
}

impl From<GlobalType> for AotGlobalTypeMetadata {
    fn from(value: GlobalType) -> Self {
        Self {
            val_type: value.val_type,
            mutable: value.mutable,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AotMemoryMetadata {
    pub min: u32,
    pub cap: u32,
    pub max: u32,
    pub is_max_encoded: bool,
    pub is_shared: bool,
}

impl From<&Memory> for AotMemoryMetadata {
    fn from(value: &Memory) -> Self {
        Self {
            min: value.min,
            cap: value.cap,
            max: value.max,
            is_max_encoded: value.is_max_encoded,
            is_shared: value.is_shared,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AotTableMetadata {
    pub min: u32,
    pub max: Option<u32>,
    pub ty: RefType,
}

impl From<&Table> for AotTableMetadata {
    fn from(value: &Table) -> Self {
        Self {
            min: value.min,
            max: value.max,
            ty: value.ty,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AotImportDescMetadata {
    Func(Index),
    Table(AotTableMetadata),
    Memory(AotMemoryMetadata),
    Global(AotGlobalTypeMetadata),
}

impl Default for AotImportDescMetadata {
    fn default() -> Self {
        Self::Func(0)
    }
}

impl AotImportDescMetadata {
    fn extern_type(&self) -> ExternType {
        match self {
            Self::Func(_) => ExternType::FUNC,
            Self::Table(_) => ExternType::TABLE,
            Self::Memory(_) => ExternType::MEMORY,
            Self::Global(_) => ExternType::GLOBAL,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AotImportMetadata {
    pub ty: ExternType,
    pub module: String,
    pub name: String,
    pub desc: AotImportDescMetadata,
    pub index_per_type: Index,
}

impl From<&Import> for AotImportMetadata {
    fn from(value: &Import) -> Self {
        Self {
            ty: value.ty,
            module: value.module.clone(),
            name: value.name.clone(),
            desc: match &value.desc {
                ImportDesc::Func(type_index) => AotImportDescMetadata::Func(*type_index),
                ImportDesc::Table(table) => AotImportDescMetadata::Table(table.into()),
                ImportDesc::Memory(memory) => AotImportDescMetadata::Memory(memory.into()),
                ImportDesc::Global(global) => AotImportDescMetadata::Global((*global).into()),
            },
            index_per_type: value.index_per_type,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AotExportMetadata {
    pub ty: ExternType,
    pub name: String,
    pub index: Index,
}

impl From<&Export> for AotExportMetadata {
    fn from(value: &Export) -> Self {
        Self {
            ty: value.ty,
            name: value.name.clone(),
            index: value.index,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AotDataSegmentMetadata {
    pub offset_expression: Vec<u8>,
    pub init: Vec<u8>,
    pub passive: bool,
}

impl From<&razero_wasm::module::DataSegment> for AotDataSegmentMetadata {
    fn from(value: &razero_wasm::module::DataSegment) -> Self {
        Self {
            offset_expression: value.offset_expression.data.clone(),
            init: value.init.clone(),
            passive: value.passive,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AotGlobalInitializerMetadata {
    pub init_expression: Vec<u8>,
}

impl From<&razero_wasm::module::Global> for AotGlobalInitializerMetadata {
    fn from(value: &razero_wasm::module::Global) -> Self {
        Self {
            init_expression: value.init.data.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AotElementSegmentMetadata {
    pub offset_expression: Vec<u8>,
    pub table_index: Index,
    pub init_expressions: Vec<Vec<u8>>,
    pub ty: RefType,
    pub mode: ElementMode,
}

impl From<&razero_wasm::module::ElementSegment> for AotElementSegmentMetadata {
    fn from(value: &razero_wasm::module::ElementSegment) -> Self {
        Self {
            offset_expression: value.offset_expr.data.clone(),
            table_index: value.table_index,
            init_expressions: value.init.iter().map(|expr| expr.data.clone()).collect(),
            ty: value.ty,
            mode: value.mode,
        }
    }
}

/// Versioned layout information for the per-module context consumed by linked AOT code.
///
/// These byte offsets are part of the documented packaging ABI. Negative offsets indicate that
/// the corresponding region is absent for the current module shape.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AotModuleContextMetadata {
    pub total_size: usize,
    pub module_instance_offset: i32,
    pub local_memory_begin: i32,
    pub imported_memory_begin: i32,
    pub imported_functions_begin: i32,
    pub globals_begin: i32,
    pub type_ids_1st_element: i32,
    pub tables_begin: i32,
    pub before_listener_trampolines_1st_element: i32,
    pub after_listener_trampolines_1st_element: i32,
    pub data_instances_1st_element: i32,
    pub element_instances_1st_element: i32,
}

impl From<ModuleContextOffsetData> for AotModuleContextMetadata {
    fn from(value: ModuleContextOffsetData) -> Self {
        Self {
            total_size: value.total_size,
            module_instance_offset: value.module_instance_offset.raw(),
            local_memory_begin: value.local_memory_begin.raw(),
            imported_memory_begin: value.imported_memory_begin.raw(),
            imported_functions_begin: value.imported_functions_begin.raw(),
            globals_begin: value.globals_begin.raw(),
            type_ids_1st_element: value.type_ids_1st_element.raw(),
            tables_begin: value.tables_begin.raw(),
            before_listener_trampolines_1st_element: value
                .before_listener_trampolines_1st_element
                .raw(),
            after_listener_trampolines_1st_element: value
                .after_listener_trampolines_1st_element
                .raw(),
            data_instances_1st_element: value.data_instances_1st_element.raw(),
            element_instances_1st_element: value.element_instances_1st_element.raw(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AotModuleShapeMetadata {
    pub enabled_features: u64,
    pub import_function_count: Index,
    pub import_global_count: Index,
    pub import_memory_count: Index,
    pub import_table_count: Index,
    pub local_function_count: u32,
    pub local_global_count: u32,
    pub local_table_count: u32,
    pub has_local_memory: bool,
    pub has_any_memory: bool,
    pub has_start_section: bool,
    pub data_segment_count: u32,
    pub element_segment_count: u32,
    pub is_host_module: bool,
}

/// Versioned layout information for the runtime execution context used by linked AOT code.
///
/// Incompatible layout changes require bumping [`EXECUTION_CONTEXT_ABI_VERSION`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AotExecutionContextMetadata {
    pub abi_version: u32,
    pub size: usize,
    pub exit_code_offset: i32,
    pub caller_module_context_ptr_offset: i32,
    pub original_frame_pointer_offset: i32,
    pub original_stack_pointer_offset: i32,
    pub go_return_address_offset: i32,
    pub stack_bottom_ptr_offset: i32,
    pub go_call_return_address_offset: i32,
    pub stack_pointer_before_go_call_offset: i32,
    pub stack_grow_required_size_offset: i32,
    pub memory_grow_trampoline_address_offset: i32,
    pub stack_grow_call_trampoline_address_offset: i32,
    pub check_module_exit_code_trampoline_address_offset: i32,
    pub saved_registers_offset: i32,
    pub go_function_call_callee_module_context_opaque_offset: i32,
    pub table_grow_trampoline_address_offset: i32,
    pub ref_func_trampoline_address_offset: i32,
    pub memmove_address_offset: i32,
    pub frame_pointer_before_go_call_offset: i32,
    pub memory_wait32_trampoline_address_offset: i32,
    pub memory_wait64_trampoline_address_offset: i32,
    pub memory_notify_trampoline_address_offset: i32,
    pub fuel_offset: i32,
}

impl AotExecutionContextMetadata {
    pub fn current() -> Self {
        Self {
            abi_version: EXECUTION_CONTEXT_ABI_VERSION,
            size: size_of::<ExecutionContext>(),
            exit_code_offset: EXECUTION_CONTEXT_OFFSET_EXIT_CODE_OFFSET.raw(),
            caller_module_context_ptr_offset: EXECUTION_CONTEXT_OFFSET_CALLER_MODULE_CONTEXT_PTR
                .raw(),
            original_frame_pointer_offset: EXECUTION_CONTEXT_OFFSET_ORIGINAL_FRAME_POINTER.raw(),
            original_stack_pointer_offset: EXECUTION_CONTEXT_OFFSET_ORIGINAL_STACK_POINTER.raw(),
            go_return_address_offset: EXECUTION_CONTEXT_OFFSET_GO_RETURN_ADDRESS.raw(),
            stack_bottom_ptr_offset: EXECUTION_CONTEXT_OFFSET_STACK_BOTTOM_PTR.raw(),
            go_call_return_address_offset: EXECUTION_CONTEXT_OFFSET_GO_CALL_RETURN_ADDRESS.raw(),
            stack_pointer_before_go_call_offset:
                EXECUTION_CONTEXT_OFFSET_STACK_POINTER_BEFORE_GO_CALL.raw(),
            stack_grow_required_size_offset: EXECUTION_CONTEXT_OFFSET_STACK_GROW_REQUIRED_SIZE
                .raw(),
            memory_grow_trampoline_address_offset:
                EXECUTION_CONTEXT_OFFSET_MEMORY_GROW_TRAMPOLINE_ADDRESS.raw(),
            stack_grow_call_trampoline_address_offset:
                EXECUTION_CONTEXT_OFFSET_STACK_GROW_CALL_TRAMPOLINE_ADDRESS.raw(),
            check_module_exit_code_trampoline_address_offset:
                EXECUTION_CONTEXT_OFFSET_CHECK_MODULE_EXIT_CODE_TRAMPOLINE_ADDRESS.raw(),
            saved_registers_offset: EXECUTION_CONTEXT_OFFSET_SAVED_REGISTERS_BEGIN.raw(),
            go_function_call_callee_module_context_opaque_offset:
                EXECUTION_CONTEXT_OFFSET_GO_FUNCTION_CALL_CALLEE_MODULE_CONTEXT_OPAQUE.raw(),
            table_grow_trampoline_address_offset:
                EXECUTION_CONTEXT_OFFSET_TABLE_GROW_TRAMPOLINE_ADDRESS.raw(),
            ref_func_trampoline_address_offset:
                EXECUTION_CONTEXT_OFFSET_REF_FUNC_TRAMPOLINE_ADDRESS.raw(),
            memmove_address_offset: EXECUTION_CONTEXT_OFFSET_MEMMOVE_ADDRESS.raw(),
            frame_pointer_before_go_call_offset:
                EXECUTION_CONTEXT_OFFSET_FRAME_POINTER_BEFORE_GO_CALL.raw(),
            memory_wait32_trampoline_address_offset:
                EXECUTION_CONTEXT_OFFSET_MEMORY_WAIT32_TRAMPOLINE_ADDRESS.raw(),
            memory_wait64_trampoline_address_offset:
                EXECUTION_CONTEXT_OFFSET_MEMORY_WAIT64_TRAMPOLINE_ADDRESS.raw(),
            memory_notify_trampoline_address_offset:
                EXECUTION_CONTEXT_OFFSET_MEMORY_NOTIFY_TRAMPOLINE_ADDRESS.raw(),
            fuel_offset: EXECUTION_CONTEXT_OFFSET_FUEL.raw(),
        }
    }
}

impl Default for AotExecutionContextMetadata {
    fn default() -> Self {
        Self::current()
    }
}

/// Stable helper identifiers embedded in the AOT metadata sidecar.
///
/// Numeric assignments are part of the serialized packaging ABI and must remain append-only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AotHelperId {
    MemoryGrow,
    StackGrow,
    CheckModuleExitCode,
    TableGrow,
    RefFunc,
    Memmove,
    MemoryWait32,
    MemoryWait64,
    MemoryNotify,
}

impl AotHelperId {
    const fn to_byte(self) -> u8 {
        match self {
            Self::MemoryGrow => 1,
            Self::StackGrow => 2,
            Self::CheckModuleExitCode => 3,
            Self::TableGrow => 4,
            Self::RefFunc => 5,
            Self::Memmove => 6,
            Self::MemoryWait32 => 7,
            Self::MemoryWait64 => 8,
            Self::MemoryNotify => 9,
        }
    }

    const fn from_byte(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::MemoryGrow),
            2 => Some(Self::StackGrow),
            3 => Some(Self::CheckModuleExitCode),
            4 => Some(Self::TableGrow),
            5 => Some(Self::RefFunc),
            6 => Some(Self::Memmove),
            7 => Some(Self::MemoryWait32),
            8 => Some(Self::MemoryWait64),
            9 => Some(Self::MemoryNotify),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AotHelperMetadata {
    pub id: AotHelperId,
    pub execution_context_offset: i32,
    pub exit_code: Option<ExitCode>,
}

fn current_helper_metadata() -> Vec<AotHelperMetadata> {
    vec![
        AotHelperMetadata {
            id: AotHelperId::MemoryGrow,
            execution_context_offset: EXECUTION_CONTEXT_OFFSET_MEMORY_GROW_TRAMPOLINE_ADDRESS.raw(),
            exit_code: Some(ExitCode::GROW_MEMORY),
        },
        AotHelperMetadata {
            id: AotHelperId::StackGrow,
            execution_context_offset: EXECUTION_CONTEXT_OFFSET_STACK_GROW_CALL_TRAMPOLINE_ADDRESS
                .raw(),
            exit_code: None,
        },
        AotHelperMetadata {
            id: AotHelperId::CheckModuleExitCode,
            execution_context_offset:
                EXECUTION_CONTEXT_OFFSET_CHECK_MODULE_EXIT_CODE_TRAMPOLINE_ADDRESS.raw(),
            exit_code: Some(ExitCode::CHECK_MODULE_EXIT_CODE),
        },
        AotHelperMetadata {
            id: AotHelperId::TableGrow,
            execution_context_offset: EXECUTION_CONTEXT_OFFSET_TABLE_GROW_TRAMPOLINE_ADDRESS.raw(),
            exit_code: Some(ExitCode::TABLE_GROW),
        },
        AotHelperMetadata {
            id: AotHelperId::RefFunc,
            execution_context_offset: EXECUTION_CONTEXT_OFFSET_REF_FUNC_TRAMPOLINE_ADDRESS.raw(),
            exit_code: Some(ExitCode::REF_FUNC),
        },
        AotHelperMetadata {
            id: AotHelperId::Memmove,
            execution_context_offset: EXECUTION_CONTEXT_OFFSET_MEMMOVE_ADDRESS.raw(),
            exit_code: None,
        },
        AotHelperMetadata {
            id: AotHelperId::MemoryWait32,
            execution_context_offset: EXECUTION_CONTEXT_OFFSET_MEMORY_WAIT32_TRAMPOLINE_ADDRESS
                .raw(),
            exit_code: Some(ExitCode::MEMORY_WAIT32),
        },
        AotHelperMetadata {
            id: AotHelperId::MemoryWait64,
            execution_context_offset: EXECUTION_CONTEXT_OFFSET_MEMORY_WAIT64_TRAMPOLINE_ADDRESS
                .raw(),
            exit_code: Some(ExitCode::MEMORY_WAIT64),
        },
        AotHelperMetadata {
            id: AotHelperId::MemoryNotify,
            execution_context_offset: EXECUTION_CONTEXT_OFFSET_MEMORY_NOTIFY_TRAMPOLINE_ADDRESS
                .raw(),
            exit_code: Some(ExitCode::MEMORY_NOTIFY),
        },
    ]
}

/// Serialized sidecar schema paired with ELF relocatable output from `emit_relocatable_object()`.
///
/// This metadata is part of the supported Rust AOT packaging contract used by linker and
/// runtime-support code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AotCompiledMetadata {
    pub target: AotTarget,
    pub module_id: ModuleId,
    pub import_function_count: Index,
    pub entry_preamble_offsets: Vec<usize>,
    pub types: Vec<AotFunctionTypeMetadata>,
    pub imports: Vec<AotImportMetadata>,
    pub exports: Vec<AotExportMetadata>,
    pub start_function_index: Option<Index>,
    pub data_segments: Vec<AotDataSegmentMetadata>,
    pub global_initializers: Vec<AotGlobalInitializerMetadata>,
    pub element_segments: Vec<AotElementSegmentMetadata>,
    pub memory: Option<AotMemoryMetadata>,
    pub tables: Vec<AotTableMetadata>,
    pub globals: Vec<AotGlobalTypeMetadata>,
    pub module_shape: AotModuleShapeMetadata,
    pub functions: Vec<AotFunctionMetadata>,
    pub relocations: Vec<AotRelocationMetadata>,
    pub module_context: AotModuleContextMetadata,
    pub execution_context: AotExecutionContextMetadata,
    pub helpers: Vec<AotHelperMetadata>,
    pub source_map: Vec<AotSourceMapEntry>,
    pub ensure_termination: bool,
    pub memory_isolation_enabled: bool,
}

impl Default for AotCompiledMetadata {
    fn default() -> Self {
        Self {
            target: AotTarget::default(),
            module_id: [0; 32],
            import_function_count: 0,
            entry_preamble_offsets: Vec::new(),
            types: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            start_function_index: None,
            data_segments: Vec::new(),
            global_initializers: Vec::new(),
            element_segments: Vec::new(),
            memory: None,
            tables: Vec::new(),
            globals: Vec::new(),
            module_shape: AotModuleShapeMetadata::default(),
            functions: Vec::new(),
            relocations: Vec::new(),
            module_context: AotModuleContextMetadata::default(),
            execution_context: AotExecutionContextMetadata::current(),
            helpers: current_helper_metadata(),
            source_map: Vec::new(),
            ensure_termination: false,
            memory_isolation_enabled: false,
        }
    }
}

impl AotCompiledMetadata {
    pub fn new(
        module: &Module,
        entry_preamble_offsets: Vec<usize>,
        functions: Vec<AotFunctionMetadata>,
        relocations: Vec<AotRelocationMetadata>,
        module_context: ModuleContextOffsetData,
        source_map: Vec<AotSourceMapEntry>,
        memory_isolation_enabled: bool,
    ) -> Self {
        Self {
            target: AotTarget::current(),
            module_id: module.id,
            import_function_count: module.import_function_count,
            entry_preamble_offsets,
            types: module
                .type_section
                .iter()
                .map(|ty| AotFunctionTypeMetadata {
                    params: ty.params.clone(),
                    results: ty.results.clone(),
                    param_num_in_u64: ty.param_num_in_u64,
                    result_num_in_u64: ty.result_num_in_u64,
                })
                .collect(),
            imports: module
                .import_section
                .iter()
                .map(AotImportMetadata::from)
                .collect(),
            exports: module
                .export_section
                .iter()
                .map(AotExportMetadata::from)
                .collect(),
            start_function_index: module.start_section,
            data_segments: module
                .data_section
                .iter()
                .map(AotDataSegmentMetadata::from)
                .collect(),
            global_initializers: module
                .global_section
                .iter()
                .map(AotGlobalInitializerMetadata::from)
                .collect(),
            element_segments: module
                .element_section
                .iter()
                .map(AotElementSegmentMetadata::from)
                .collect(),
            memory: module.memory_section.as_ref().map(AotMemoryMetadata::from),
            tables: module
                .table_section
                .iter()
                .map(AotTableMetadata::from)
                .collect(),
            globals: module
                .global_section
                .iter()
                .map(|global| global.ty.into())
                .collect(),
            module_shape: AotModuleShapeMetadata {
                enabled_features: module.enabled_features.bits(),
                import_function_count: module.import_function_count,
                import_global_count: module.import_global_count,
                import_memory_count: module.import_memory_count,
                import_table_count: module.import_table_count,
                local_function_count: module.function_section.len() as u32,
                local_global_count: module.global_section.len() as u32,
                local_table_count: module.table_section.len() as u32,
                has_local_memory: module.memory_section.is_some(),
                has_any_memory: module.memory_section.is_some() || module.import_memory_count > 0,
                has_start_section: module.start_section.is_some(),
                data_segment_count: module.data_section.len() as u32,
                element_segment_count: module.element_section.len() as u32,
                is_host_module: module.is_host_module,
            },
            functions,
            relocations,
            module_context: module_context.into(),
            execution_context: AotExecutionContextMetadata::current(),
            helpers: current_helper_metadata(),
            source_map,
            ensure_termination: module.ensure_termination,
            memory_isolation_enabled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AotMetadataError {
    InvalidHeader(String),
    Io(String),
}

impl Display for AotMetadataError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHeader(message) | Self::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for AotMetadataError {}

pub fn relocations_for_function(
    wasm_function_index: Index,
    function_offset: usize,
    relocations: &[RelocationInfo],
) -> Vec<AotRelocationMetadata> {
    relocations
        .iter()
        .map(|relocation| AotRelocationMetadata {
            source_wasm_function_index: wasm_function_index,
            target_function_index: relocation.func_ref.0,
            executable_offset: relocation.offset + function_offset as i64,
            is_tail_call: relocation.is_tail_call,
        })
        .collect()
}

/// Serializes the stable Razero AOT metadata sidecar.
pub fn serialize_aot_metadata(metadata: &AotCompiledMetadata) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(AOT_METADATA_MAGIC);
    buf.push(metadata.target.architecture.to_byte());
    buf.push(metadata.target.operating_system.to_byte());
    buf.extend_from_slice(&metadata.module_id);
    buf.extend_from_slice(&metadata.import_function_count.to_le_bytes());
    buf.push(u8::from(metadata.ensure_termination));
    buf.push(u8::from(metadata.memory_isolation_enabled));

    write_usize_slice(&mut buf, &metadata.entry_preamble_offsets);
    buf.extend_from_slice(&(metadata.types.len() as u32).to_le_bytes());
    for ty in &metadata.types {
        write_value_types(&mut buf, &ty.params);
        write_value_types(&mut buf, &ty.results);
        buf.extend_from_slice(&(ty.param_num_in_u64 as u64).to_le_bytes());
        buf.extend_from_slice(&(ty.result_num_in_u64 as u64).to_le_bytes());
    }

    buf.extend_from_slice(&metadata.module_shape.enabled_features.to_le_bytes());
    buf.extend_from_slice(&metadata.module_shape.import_function_count.to_le_bytes());
    buf.extend_from_slice(&metadata.module_shape.import_global_count.to_le_bytes());
    buf.extend_from_slice(&metadata.module_shape.import_memory_count.to_le_bytes());
    buf.extend_from_slice(&metadata.module_shape.import_table_count.to_le_bytes());
    buf.extend_from_slice(&metadata.module_shape.local_function_count.to_le_bytes());
    buf.extend_from_slice(&metadata.module_shape.local_global_count.to_le_bytes());
    buf.extend_from_slice(&metadata.module_shape.local_table_count.to_le_bytes());
    buf.push(u8::from(metadata.module_shape.has_local_memory));
    buf.push(u8::from(metadata.module_shape.has_any_memory));
    buf.push(u8::from(metadata.module_shape.has_start_section));
    buf.extend_from_slice(&metadata.module_shape.data_segment_count.to_le_bytes());
    buf.extend_from_slice(&metadata.module_shape.element_segment_count.to_le_bytes());
    buf.push(u8::from(metadata.module_shape.is_host_module));

    buf.extend_from_slice(&(metadata.functions.len() as u32).to_le_bytes());
    for function in &metadata.functions {
        buf.extend_from_slice(&function.local_function_index.to_le_bytes());
        buf.extend_from_slice(&function.wasm_function_index.to_le_bytes());
        buf.extend_from_slice(&function.type_index.to_le_bytes());
        buf.extend_from_slice(&(function.executable_offset as u64).to_le_bytes());
        buf.extend_from_slice(&(function.executable_len as u64).to_le_bytes());
    }

    buf.extend_from_slice(&(metadata.relocations.len() as u32).to_le_bytes());
    for relocation in &metadata.relocations {
        buf.extend_from_slice(&relocation.source_wasm_function_index.to_le_bytes());
        buf.extend_from_slice(&relocation.target_function_index.to_le_bytes());
        buf.extend_from_slice(&relocation.executable_offset.to_le_bytes());
        buf.push(u8::from(relocation.is_tail_call));
    }

    buf.extend_from_slice(&(metadata.module_context.total_size as u64).to_le_bytes());
    for raw in [
        metadata.module_context.module_instance_offset,
        metadata.module_context.local_memory_begin,
        metadata.module_context.imported_memory_begin,
        metadata.module_context.imported_functions_begin,
        metadata.module_context.globals_begin,
        metadata.module_context.type_ids_1st_element,
        metadata.module_context.tables_begin,
        metadata
            .module_context
            .before_listener_trampolines_1st_element,
        metadata
            .module_context
            .after_listener_trampolines_1st_element,
        metadata.module_context.data_instances_1st_element,
        metadata.module_context.element_instances_1st_element,
    ] {
        buf.extend_from_slice(&raw.to_le_bytes());
    }

    buf.extend_from_slice(&(metadata.source_map.len() as u64).to_le_bytes());
    for entry in &metadata.source_map {
        buf.extend_from_slice(&entry.wasm_binary_offset.to_le_bytes());
        buf.extend_from_slice(&(entry.executable_offset as u64).to_le_bytes());
    }

    buf.extend_from_slice(&(metadata.imports.len() as u32).to_le_bytes());
    for import in &metadata.imports {
        buf.push(import.ty.0);
        write_string(&mut buf, &import.module);
        write_string(&mut buf, &import.name);
        buf.extend_from_slice(&import.index_per_type.to_le_bytes());
        write_import_desc(&mut buf, &import.desc);
    }

    match &metadata.memory {
        Some(memory) => {
            buf.push(1);
            write_memory_metadata(&mut buf, memory);
        }
        None => buf.push(0),
    }

    buf.extend_from_slice(&(metadata.tables.len() as u32).to_le_bytes());
    for table in &metadata.tables {
        write_table_metadata(&mut buf, table);
    }

    buf.extend_from_slice(&(metadata.globals.len() as u32).to_le_bytes());
    for global in &metadata.globals {
        write_global_type_metadata(&mut buf, global);
    }

    buf.extend_from_slice(&(metadata.exports.len() as u32).to_le_bytes());
    for export in &metadata.exports {
        buf.push(export.ty.0);
        write_string(&mut buf, &export.name);
        buf.extend_from_slice(&export.index.to_le_bytes());
    }
    match metadata.start_function_index {
        Some(index) => {
            buf.push(1);
            buf.extend_from_slice(&index.to_le_bytes());
        }
        None => buf.push(0),
    }

    buf.extend_from_slice(&metadata.execution_context.abi_version.to_le_bytes());
    buf.extend_from_slice(&(metadata.execution_context.size as u64).to_le_bytes());
    for raw in [
        metadata.execution_context.exit_code_offset,
        metadata.execution_context.caller_module_context_ptr_offset,
        metadata.execution_context.original_frame_pointer_offset,
        metadata.execution_context.original_stack_pointer_offset,
        metadata.execution_context.go_return_address_offset,
        metadata.execution_context.stack_bottom_ptr_offset,
        metadata.execution_context.go_call_return_address_offset,
        metadata
            .execution_context
            .stack_pointer_before_go_call_offset,
        metadata.execution_context.stack_grow_required_size_offset,
        metadata
            .execution_context
            .memory_grow_trampoline_address_offset,
        metadata
            .execution_context
            .stack_grow_call_trampoline_address_offset,
        metadata
            .execution_context
            .check_module_exit_code_trampoline_address_offset,
        metadata.execution_context.saved_registers_offset,
        metadata
            .execution_context
            .go_function_call_callee_module_context_opaque_offset,
        metadata
            .execution_context
            .table_grow_trampoline_address_offset,
        metadata
            .execution_context
            .ref_func_trampoline_address_offset,
        metadata.execution_context.memmove_address_offset,
        metadata
            .execution_context
            .frame_pointer_before_go_call_offset,
        metadata
            .execution_context
            .memory_wait32_trampoline_address_offset,
        metadata
            .execution_context
            .memory_wait64_trampoline_address_offset,
        metadata
            .execution_context
            .memory_notify_trampoline_address_offset,
        metadata.execution_context.fuel_offset,
    ] {
        buf.extend_from_slice(&raw.to_le_bytes());
    }

    buf.extend_from_slice(&(metadata.helpers.len() as u32).to_le_bytes());
    for helper in &metadata.helpers {
        buf.push(helper.id.to_byte());
        buf.extend_from_slice(&helper.execution_context_offset.to_le_bytes());
        match helper.exit_code {
            Some(exit_code) => {
                buf.push(1);
                buf.extend_from_slice(&exit_code.raw().to_le_bytes());
            }
            None => {
                buf.push(0);
                buf.extend_from_slice(&0u32.to_le_bytes());
            }
        }
    }

    buf.extend_from_slice(&(metadata.data_segments.len() as u32).to_le_bytes());
    for data in &metadata.data_segments {
        buf.push(u8::from(data.passive));
        write_bytes(&mut buf, &data.offset_expression);
        write_bytes(&mut buf, &data.init);
    }

    buf.extend_from_slice(&(metadata.global_initializers.len() as u32).to_le_bytes());
    for global in &metadata.global_initializers {
        write_bytes(&mut buf, &global.init_expression);
    }

    buf.extend_from_slice(&(metadata.element_segments.len() as u32).to_le_bytes());
    for element in &metadata.element_segments {
        write_bytes(&mut buf, &element.offset_expression);
        buf.extend_from_slice(&element.table_index.to_le_bytes());
        buf.push(element.ty.0);
        buf.push(match element.mode {
            ElementMode::Active => 0,
            ElementMode::Passive => 1,
            ElementMode::Declarative => 2,
        });
        buf.extend_from_slice(&(element.init_expressions.len() as u32).to_le_bytes());
        for init in &element.init_expressions {
            write_bytes(&mut buf, init);
        }
    }

    buf
}

/// Deserializes the stable Razero AOT metadata sidecar.
pub fn deserialize_aot_metadata(bytes: &[u8]) -> Result<AotCompiledMetadata, AotMetadataError> {
    let mut cursor = Cursor::new(bytes);
    let mut magic = [0u8; 8];
    read_exact(
        &mut cursor,
        &mut magic,
        "aot metadata: invalid header length",
    )?;
    if &magic != AOT_METADATA_MAGIC {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: invalid magic number".to_string(),
        ));
    }

    let architecture = AotTargetArchitecture::from_byte(read_u8(&mut cursor)?);
    let operating_system = AotTargetOperatingSystem::from_byte(read_u8(&mut cursor)?);

    let mut module_id = [0u8; 32];
    read_exact(
        &mut cursor,
        &mut module_id,
        "aot metadata: invalid module id",
    )?;
    let import_function_count = read_u32(&mut cursor)?;
    let ensure_termination = read_u8(&mut cursor)? != 0;
    let memory_isolation_enabled = read_u8(&mut cursor)? != 0;

    let entry_preamble_offsets = read_usize_vec(&mut cursor, "entry preamble")?;

    let type_len = read_u32(&mut cursor)? as u64;
    let type_len = checked_vec_len(
        &cursor,
        type_len,
        32,
        "aot metadata: invalid type metadata length",
    )?;
    let mut types = Vec::with_capacity(type_len);
    for _ in 0..type_len {
        types.push(AotFunctionTypeMetadata {
            params: read_value_types(&mut cursor, "type params")?,
            results: read_value_types(&mut cursor, "type results")?,
            param_num_in_u64: read_u64(&mut cursor)? as usize,
            result_num_in_u64: read_u64(&mut cursor)? as usize,
        });
    }

    let module_shape = AotModuleShapeMetadata {
        enabled_features: read_u64(&mut cursor)?,
        import_function_count: read_u32(&mut cursor)?,
        import_global_count: read_u32(&mut cursor)?,
        import_memory_count: read_u32(&mut cursor)?,
        import_table_count: read_u32(&mut cursor)?,
        local_function_count: read_u32(&mut cursor)?,
        local_global_count: read_u32(&mut cursor)?,
        local_table_count: read_u32(&mut cursor)?,
        has_local_memory: read_u8(&mut cursor)? != 0,
        has_any_memory: read_u8(&mut cursor)? != 0,
        has_start_section: read_u8(&mut cursor)? != 0,
        data_segment_count: read_u32(&mut cursor)?,
        element_segment_count: read_u32(&mut cursor)?,
        is_host_module: read_u8(&mut cursor)? != 0,
    };

    let function_len = read_u32(&mut cursor)? as u64;
    let function_len = checked_vec_len(
        &cursor,
        function_len,
        28,
        "aot metadata: invalid function metadata length",
    )?;
    let mut functions = Vec::with_capacity(function_len);
    for _ in 0..function_len {
        functions.push(AotFunctionMetadata {
            local_function_index: read_u32(&mut cursor)?,
            wasm_function_index: read_u32(&mut cursor)?,
            type_index: read_u32(&mut cursor)?,
            executable_offset: read_u64(&mut cursor)? as usize,
            executable_len: read_u64(&mut cursor)? as usize,
        });
    }

    let relocation_len = read_u32(&mut cursor)? as u64;
    let relocation_len = checked_vec_len(
        &cursor,
        relocation_len,
        17,
        "aot metadata: invalid relocation metadata length",
    )?;
    let mut relocations = Vec::with_capacity(relocation_len);
    for _ in 0..relocation_len {
        relocations.push(AotRelocationMetadata {
            source_wasm_function_index: read_u32(&mut cursor)?,
            target_function_index: read_u32(&mut cursor)?,
            executable_offset: read_i64(&mut cursor)?,
            is_tail_call: read_u8(&mut cursor)? != 0,
        });
    }

    let module_context = AotModuleContextMetadata {
        total_size: read_u64(&mut cursor)? as usize,
        module_instance_offset: read_i32(&mut cursor)?,
        local_memory_begin: read_i32(&mut cursor)?,
        imported_memory_begin: read_i32(&mut cursor)?,
        imported_functions_begin: read_i32(&mut cursor)?,
        globals_begin: read_i32(&mut cursor)?,
        type_ids_1st_element: read_i32(&mut cursor)?,
        tables_begin: read_i32(&mut cursor)?,
        before_listener_trampolines_1st_element: read_i32(&mut cursor)?,
        after_listener_trampolines_1st_element: read_i32(&mut cursor)?,
        data_instances_1st_element: read_i32(&mut cursor)?,
        element_instances_1st_element: read_i32(&mut cursor)?,
    };

    let source_map_len = read_u64(&mut cursor)?;
    let source_map_len = checked_vec_len(
        &cursor,
        source_map_len,
        16,
        "aot metadata: invalid source map length",
    )?;
    let mut source_map = Vec::with_capacity(source_map_len);
    for _ in 0..source_map_len {
        source_map.push(AotSourceMapEntry {
            wasm_binary_offset: read_u64(&mut cursor)?,
            executable_offset: read_u64(&mut cursor)? as usize,
        });
    }

    let (
        imports,
        memory,
        tables,
        globals,
        exports,
        start_function_index,
        execution_context,
        helpers,
        data_segments,
        global_initializers,
        element_segments,
    ) = if remaining(&cursor) == 0 {
        (
            Vec::new(),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            AotExecutionContextMetadata::current(),
            current_helper_metadata(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    } else {
        let import_len = read_u32(&mut cursor)? as u64;
        let import_len = checked_vec_len(
            &cursor,
            import_len,
            11,
            "aot metadata: invalid import metadata length",
        )?;
        let mut imports = Vec::with_capacity(import_len);
        for _ in 0..import_len {
            let ty = ExternType(read_u8(&mut cursor)?);
            let module = read_string(&mut cursor, "import module")?;
            let name = read_string(&mut cursor, "import name")?;
            let index_per_type = read_u32(&mut cursor)?;
            let desc = read_import_desc(&mut cursor)?;
            if ty != desc.extern_type() {
                return Err(AotMetadataError::InvalidHeader(
                    "aot metadata: inconsistent import descriptor".to_string(),
                ));
            }
            imports.push(AotImportMetadata {
                ty,
                module,
                name,
                desc,
                index_per_type,
            });
        }

        let memory = match read_u8(&mut cursor)? {
            0 => None,
            1 => Some(read_memory_metadata(&mut cursor)?),
            _ => {
                return Err(AotMetadataError::InvalidHeader(
                    "aot metadata: invalid memory metadata flag".to_string(),
                ));
            }
        };

        let table_len = read_u32(&mut cursor)? as u64;
        let table_len = checked_vec_len(
            &cursor,
            table_len,
            10,
            "aot metadata: invalid table metadata length",
        )?;
        let mut tables = Vec::with_capacity(table_len);
        for _ in 0..table_len {
            tables.push(read_table_metadata(&mut cursor)?);
        }

        let global_len = read_u32(&mut cursor)? as u64;
        let global_len = checked_vec_len(
            &cursor,
            global_len,
            2,
            "aot metadata: invalid global metadata length",
        )?;
        let mut globals = Vec::with_capacity(global_len);
        for _ in 0..global_len {
            globals.push(read_global_type_metadata(&mut cursor)?);
        }

        let (
            exports,
            start_function_index,
            execution_context,
            helpers,
            data_segments,
            global_initializers,
            element_segments,
        ) = if remaining(&cursor) == 0 {
            (
                Vec::new(),
                None,
                AotExecutionContextMetadata::current(),
                current_helper_metadata(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        } else {
            let export_len = read_u32(&mut cursor)? as u64;
            let export_len = checked_vec_len(
                &cursor,
                export_len,
                9,
                "aot metadata: invalid export metadata length",
            )?;
            let mut exports = Vec::with_capacity(export_len);
            for _ in 0..export_len {
                let ty = match read_u8(&mut cursor)? {
                    0 => ExternType::FUNC,
                    1 => ExternType::TABLE,
                    2 => ExternType::MEMORY,
                    3 => ExternType::GLOBAL,
                    _ => {
                        return Err(AotMetadataError::InvalidHeader(
                            "aot metadata: invalid export type".to_string(),
                        ))
                    }
                };
                exports.push(AotExportMetadata {
                    ty,
                    name: read_string(&mut cursor, "export name")?,
                    index: read_u32(&mut cursor)?,
                });
            }

            let start_function_index = match read_u8(&mut cursor)? {
                0 => None,
                1 => Some(read_u32(&mut cursor)?),
                _ => {
                    return Err(AotMetadataError::InvalidHeader(
                        "aot metadata: invalid start metadata flag".to_string(),
                    ))
                }
            };

            let (execution_context, helpers, data_segments, global_initializers, element_segments) =
                if remaining(&cursor) == 0 {
                    (
                        AotExecutionContextMetadata::current(),
                        current_helper_metadata(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                    )
                } else {
                    let execution_context = AotExecutionContextMetadata {
                        abi_version: read_u32(&mut cursor)?,
                        size: read_u64(&mut cursor)? as usize,
                        exit_code_offset: read_i32(&mut cursor)?,
                        caller_module_context_ptr_offset: read_i32(&mut cursor)?,
                        original_frame_pointer_offset: read_i32(&mut cursor)?,
                        original_stack_pointer_offset: read_i32(&mut cursor)?,
                        go_return_address_offset: read_i32(&mut cursor)?,
                        stack_bottom_ptr_offset: read_i32(&mut cursor)?,
                        go_call_return_address_offset: read_i32(&mut cursor)?,
                        stack_pointer_before_go_call_offset: read_i32(&mut cursor)?,
                        stack_grow_required_size_offset: read_i32(&mut cursor)?,
                        memory_grow_trampoline_address_offset: read_i32(&mut cursor)?,
                        stack_grow_call_trampoline_address_offset: read_i32(&mut cursor)?,
                        check_module_exit_code_trampoline_address_offset: read_i32(&mut cursor)?,
                        saved_registers_offset: read_i32(&mut cursor)?,
                        go_function_call_callee_module_context_opaque_offset: read_i32(
                            &mut cursor,
                        )?,
                        table_grow_trampoline_address_offset: read_i32(&mut cursor)?,
                        ref_func_trampoline_address_offset: read_i32(&mut cursor)?,
                        memmove_address_offset: read_i32(&mut cursor)?,
                        frame_pointer_before_go_call_offset: read_i32(&mut cursor)?,
                        memory_wait32_trampoline_address_offset: read_i32(&mut cursor)?,
                        memory_wait64_trampoline_address_offset: read_i32(&mut cursor)?,
                        memory_notify_trampoline_address_offset: read_i32(&mut cursor)?,
                        fuel_offset: read_i32(&mut cursor)?,
                    };

                    let helper_len = read_u32(&mut cursor)? as u64;
                    let helper_len = checked_vec_len(
                        &cursor,
                        helper_len,
                        10,
                        "aot metadata: invalid helper metadata length",
                    )?;
                    let mut helpers = Vec::with_capacity(helper_len);
                    for _ in 0..helper_len {
                        let id =
                            AotHelperId::from_byte(read_u8(&mut cursor)?).ok_or_else(|| {
                                AotMetadataError::InvalidHeader(
                                    "aot metadata: invalid helper id".to_string(),
                                )
                            })?;
                        let execution_context_offset = read_i32(&mut cursor)?;
                        let exit_code = match read_u8(&mut cursor)? {
                            0 => {
                                let _ = read_u32(&mut cursor)?;
                                None
                            }
                            1 => Some(ExitCode::new(read_u32(&mut cursor)?)),
                            _ => {
                                return Err(AotMetadataError::InvalidHeader(
                                    "aot metadata: invalid helper exit-code flag".to_string(),
                                ))
                            }
                        };
                        helpers.push(AotHelperMetadata {
                            id,
                            execution_context_offset,
                            exit_code,
                        });
                    }
                    let data_segments = if remaining(&cursor) == 0 {
                        Vec::new()
                    } else {
                        let len = read_u32(&mut cursor)? as u64;
                        let len = checked_vec_len(
                            &cursor,
                            len,
                            9,
                            "aot metadata: invalid data segment length",
                        )?;
                        let mut data_segments = Vec::with_capacity(len);
                        for _ in 0..len {
                            let passive = match read_u8(&mut cursor)? {
                                0 => false,
                                1 => true,
                                _ => {
                                    return Err(AotMetadataError::InvalidHeader(
                                        "aot metadata: invalid data segment passive flag"
                                            .to_string(),
                                    ));
                                }
                            };
                            data_segments.push(AotDataSegmentMetadata {
                                passive,
                                offset_expression: read_bytes(
                                    &mut cursor,
                                    "data offset expression",
                                )?,
                                init: read_bytes(&mut cursor, "data init")?,
                            });
                        }
                        data_segments
                    };
                    let global_initializers = if remaining(&cursor) == 0 {
                        Vec::new()
                    } else {
                        let len = read_u32(&mut cursor)? as u64;
                        let len = checked_vec_len(
                            &cursor,
                            len,
                            4,
                            "aot metadata: invalid global initializer length",
                        )?;
                        let mut globals = Vec::with_capacity(len);
                        for _ in 0..len {
                            globals.push(AotGlobalInitializerMetadata {
                                init_expression: read_bytes(&mut cursor, "global init")?,
                            });
                        }
                        globals
                    };
                    let element_segments = if remaining(&cursor) == 0 {
                        Vec::new()
                    } else {
                        let len = read_u32(&mut cursor)? as u64;
                        let len = checked_vec_len(
                            &cursor,
                            len,
                            10,
                            "aot metadata: invalid element segment length",
                        )?;
                        let mut elements = Vec::with_capacity(len);
                        for _ in 0..len {
                            let offset_expression =
                                read_bytes(&mut cursor, "element offset expression")?;
                            let table_index = read_u32(&mut cursor)?;
                            let ty = match read_u8(&mut cursor)? {
                                0x70 => RefType::FUNCREF,
                                0x6f => RefType::EXTERNREF,
                                _ => {
                                    return Err(AotMetadataError::InvalidHeader(
                                        "aot metadata: invalid element ref type".to_string(),
                                    ));
                                }
                            };
                            let mode = match read_u8(&mut cursor)? {
                                0 => ElementMode::Active,
                                1 => ElementMode::Passive,
                                2 => ElementMode::Declarative,
                                _ => {
                                    return Err(AotMetadataError::InvalidHeader(
                                        "aot metadata: invalid element mode".to_string(),
                                    ))
                                }
                            };
                            let init_len = read_u32(&mut cursor)? as u64;
                            let init_len = checked_vec_len(
                                &cursor,
                                init_len,
                                4,
                                "aot metadata: invalid element init length",
                            )?;
                            let mut init_expressions = Vec::with_capacity(init_len);
                            for _ in 0..init_len {
                                init_expressions
                                    .push(read_bytes(&mut cursor, "element init expression")?);
                            }
                            elements.push(AotElementSegmentMetadata {
                                offset_expression,
                                table_index,
                                init_expressions,
                                ty,
                                mode,
                            });
                        }
                        elements
                    };
                    (
                        execution_context,
                        helpers,
                        data_segments,
                        global_initializers,
                        element_segments,
                    )
                };

            (
                exports,
                start_function_index,
                execution_context,
                helpers,
                data_segments,
                global_initializers,
                element_segments,
            )
        };

        (
            imports,
            memory,
            tables,
            globals,
            exports,
            start_function_index,
            execution_context,
            helpers,
            data_segments,
            global_initializers,
            element_segments,
        )
    };

    let total_table_count = module_shape
        .import_table_count
        .checked_add(module_shape.local_table_count)
        .ok_or_else(|| {
            AotMetadataError::InvalidHeader("aot metadata: invalid table count".to_string())
        })?;
    if element_segments
        .iter()
        .any(|element| element.table_index >= total_table_count)
    {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: invalid element table index".to_string(),
        ));
    }
    if element_segments
        .iter()
        .filter(|element| matches!(element.mode, ElementMode::Active))
        .any(|element| {
            let table_ty = if element.table_index < module_shape.import_table_count {
                imports
                    .iter()
                    .filter_map(|import| match &import.desc {
                        AotImportDescMetadata::Table(table) => Some(table.ty),
                        _ => None,
                    })
                    .nth(element.table_index as usize)
            } else {
                tables
                    .get((element.table_index - module_shape.import_table_count) as usize)
                    .map(|table| table.ty)
            };
            table_ty.is_some_and(|table_ty| element.ty != table_ty)
        })
    {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: active element ref type mismatch".to_string(),
        ));
    }
    if imports.iter().any(|import| {
        matches!(
            import.desc,
            AotImportDescMetadata::Func(type_index) if (type_index as usize) >= types.len()
        )
    }) {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: invalid import function type index".to_string(),
        ));
    }
    let total_import_count = module_shape
        .import_function_count
        .checked_add(module_shape.import_global_count)
        .and_then(|count| count.checked_add(module_shape.import_memory_count))
        .and_then(|count| count.checked_add(module_shape.import_table_count))
        .ok_or_else(|| {
            AotMetadataError::InvalidHeader("aot metadata: invalid import count".to_string())
        })?;
    if total_import_count as usize != imports.len() {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: import count mismatch".to_string(),
        ));
    }
    let total_function_count = module_shape
        .import_function_count
        .checked_add(module_shape.local_function_count)
        .ok_or_else(|| {
            AotMetadataError::InvalidHeader("aot metadata: invalid function count".to_string())
        })?;
    let total_global_count = module_shape
        .import_global_count
        .checked_add(module_shape.local_global_count)
        .ok_or_else(|| {
            AotMetadataError::InvalidHeader("aot metadata: invalid global count".to_string())
        })?;
    let total_memory_count = module_shape
        .import_memory_count
        .checked_add(u32::from(module_shape.has_local_memory))
        .ok_or_else(|| {
            AotMetadataError::InvalidHeader("aot metadata: invalid memory count".to_string())
        })?;
    if exports.iter().any(|export| match export.ty {
        ExternType::FUNC => export.index >= total_function_count,
        ExternType::TABLE => export.index >= total_table_count,
        ExternType::MEMORY => export.index >= total_memory_count,
        ExternType::GLOBAL => export.index >= total_global_count,
        _ => true,
    }) {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: invalid export index".to_string(),
        ));
    }
    if let Some(start_idx) = start_function_index {
        if start_idx >= total_function_count {
            return Err(AotMetadataError::InvalidHeader(
                "aot metadata: invalid start function index".to_string(),
            ));
        }
    }
    if global_initializers.len() != globals.len() {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: global initializer count mismatch".to_string(),
        ));
    }
    if module_shape.local_global_count as usize != globals.len() {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: global count mismatch".to_string(),
        ));
    }
    if module_shape.local_table_count as usize != tables.len() {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: table count mismatch".to_string(),
        ));
    }
    if module_shape.data_segment_count as usize != data_segments.len() {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: data segment count mismatch".to_string(),
        ));
    }
    if data_segments
        .iter()
        .any(|segment| !segment.passive && !module_shape.has_any_memory)
    {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: active data segment requires memory".to_string(),
        ));
    }
    if module_shape.element_segment_count as usize != element_segments.len() {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: element segment count mismatch".to_string(),
        ));
    }
    if element_segments.iter().any(|element| {
        !matches!(element.mode, ElementMode::Active) && !element.offset_expression.is_empty()
    }) {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: passive/declarative element with offset expression".to_string(),
        ));
    }
    if module_shape.local_function_count as usize != functions.len() {
        return Err(AotMetadataError::InvalidHeader(
            "aot metadata: function count mismatch".to_string(),
        ));
    }

    Ok(AotCompiledMetadata {
        target: AotTarget {
            architecture,
            operating_system,
        },
        module_id,
        import_function_count,
        entry_preamble_offsets,
        types,
        imports,
        exports,
        start_function_index,
        data_segments,
        global_initializers,
        element_segments,
        memory,
        tables,
        globals,
        module_shape,
        functions,
        relocations,
        module_context,
        execution_context,
        helpers,
        source_map,
        ensure_termination,
        memory_isolation_enabled,
    })
}

fn write_usize_slice(buf: &mut Vec<u8>, values: &[usize]) {
    buf.extend_from_slice(&(values.len() as u32).to_le_bytes());
    for &value in values {
        buf.extend_from_slice(&(value as u64).to_le_bytes());
    }
}

fn write_value_types(buf: &mut Vec<u8>, values: &[ValueType]) {
    buf.extend_from_slice(&(values.len() as u32).to_le_bytes());
    for value in values {
        buf.push(value.0);
    }
}

fn write_string(buf: &mut Vec<u8>, value: &str) {
    buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
    buf.extend_from_slice(value.as_bytes());
}

fn write_bytes(buf: &mut Vec<u8>, value: &[u8]) {
    buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
    buf.extend_from_slice(value);
}

fn write_global_type_metadata(buf: &mut Vec<u8>, global: &AotGlobalTypeMetadata) {
    buf.push(global.val_type.0);
    buf.push(u8::from(global.mutable));
}

fn write_memory_metadata(buf: &mut Vec<u8>, memory: &AotMemoryMetadata) {
    buf.extend_from_slice(&memory.min.to_le_bytes());
    buf.extend_from_slice(&memory.cap.to_le_bytes());
    buf.extend_from_slice(&memory.max.to_le_bytes());
    buf.push(u8::from(memory.is_max_encoded));
    buf.push(u8::from(memory.is_shared));
}

fn write_table_metadata(buf: &mut Vec<u8>, table: &AotTableMetadata) {
    buf.extend_from_slice(&table.min.to_le_bytes());
    match table.max {
        Some(max) => {
            buf.push(1);
            buf.extend_from_slice(&max.to_le_bytes());
        }
        None => buf.push(0),
    }
    buf.push(table.ty.0);
}

fn write_import_desc(buf: &mut Vec<u8>, desc: &AotImportDescMetadata) {
    match desc {
        AotImportDescMetadata::Func(type_index) => {
            buf.push(0);
            buf.extend_from_slice(&type_index.to_le_bytes());
        }
        AotImportDescMetadata::Table(table) => {
            buf.push(1);
            write_table_metadata(buf, table);
        }
        AotImportDescMetadata::Memory(memory) => {
            buf.push(2);
            write_memory_metadata(buf, memory);
        }
        AotImportDescMetadata::Global(global) => {
            buf.push(3);
            write_global_type_metadata(buf, global);
        }
    }
}

fn read_usize_vec(cursor: &mut Cursor<&[u8]>, label: &str) -> Result<Vec<usize>, AotMetadataError> {
    let len = read_u32(cursor)? as u64;
    let len = checked_vec_len(
        cursor,
        len,
        8,
        &format!("aot metadata: invalid {label} length"),
    )?;
    let mut values = Vec::with_capacity(len);
    for _ in 0..len {
        values.push(
            read_u64_named(cursor, &format!("aot metadata: invalid {label} offset"))? as usize,
        );
    }
    Ok(values)
}

fn read_string(cursor: &mut Cursor<&[u8]>, label: &str) -> Result<String, AotMetadataError> {
    let len = read_u32(cursor)? as u64;
    let len = checked_vec_len(
        cursor,
        len,
        1,
        &format!("aot metadata: invalid {label} length"),
    )?;
    let mut bytes = vec![0; len];
    read_exact(
        cursor,
        &mut bytes,
        &format!("aot metadata: invalid {label}"),
    )?;
    String::from_utf8(bytes).map_err(|_| {
        AotMetadataError::InvalidHeader(format!("aot metadata: invalid {label} utf-8"))
    })
}

fn read_bytes(cursor: &mut Cursor<&[u8]>, label: &str) -> Result<Vec<u8>, AotMetadataError> {
    let len = read_u32(cursor)? as u64;
    let len = checked_vec_len(
        cursor,
        len,
        1,
        &format!("aot metadata: invalid {label} length"),
    )?;
    let mut bytes = vec![0; len];
    read_exact(
        cursor,
        &mut bytes,
        &format!("aot metadata: invalid {label}"),
    )?;
    Ok(bytes)
}

fn read_value_types(
    cursor: &mut Cursor<&[u8]>,
    label: &str,
) -> Result<Vec<ValueType>, AotMetadataError> {
    let len = read_u32(cursor)? as u64;
    let len = checked_vec_len(
        cursor,
        len,
        1,
        &format!("aot metadata: invalid {label} length"),
    )?;
    let mut values = Vec::with_capacity(len);
    for _ in 0..len {
        values.push(ValueType(read_u8(cursor)?));
    }
    Ok(values)
}

fn read_global_type_metadata(
    cursor: &mut Cursor<&[u8]>,
) -> Result<AotGlobalTypeMetadata, AotMetadataError> {
    let val_type = ValueType(read_u8(cursor)?);
    let mutable = match read_u8(cursor)? {
        0 => false,
        1 => true,
        _ => {
            return Err(AotMetadataError::InvalidHeader(
                "aot metadata: invalid global mutable flag".to_string(),
            ));
        }
    };
    Ok(AotGlobalTypeMetadata { val_type, mutable })
}

fn read_memory_metadata(cursor: &mut Cursor<&[u8]>) -> Result<AotMemoryMetadata, AotMetadataError> {
    let min = read_u32(cursor)?;
    let cap = read_u32(cursor)?;
    let max = read_u32(cursor)?;
    let is_max_encoded = read_u8(cursor)? != 0;
    let is_shared = match read_u8(cursor)? {
        0 => false,
        1 => true,
        _ => {
            return Err(AotMetadataError::InvalidHeader(
                "aot metadata: invalid memory shared flag".to_string(),
            ));
        }
    };
    Ok(AotMemoryMetadata {
        min,
        cap,
        max,
        is_max_encoded,
        is_shared,
    })
}

fn read_table_metadata(cursor: &mut Cursor<&[u8]>) -> Result<AotTableMetadata, AotMetadataError> {
    let min = read_u32(cursor)?;
    let max = match read_u8(cursor)? {
        0 => None,
        1 => Some(read_u32(cursor)?),
        _ => {
            return Err(AotMetadataError::InvalidHeader(
                "aot metadata: invalid table max flag".to_string(),
            ));
        }
    };
    Ok(AotTableMetadata {
        min,
        max,
        ty: match read_u8(cursor)? {
            0x70 => RefType::FUNCREF,
            0x6f => RefType::EXTERNREF,
            _ => {
                return Err(AotMetadataError::InvalidHeader(
                    "aot metadata: invalid table ref type".to_string(),
                ));
            }
        },
    })
}

fn read_import_desc(cursor: &mut Cursor<&[u8]>) -> Result<AotImportDescMetadata, AotMetadataError> {
    match read_u8(cursor)? {
        0 => Ok(AotImportDescMetadata::Func(read_u32(cursor)?)),
        1 => Ok(AotImportDescMetadata::Table(read_table_metadata(cursor)?)),
        2 => Ok(AotImportDescMetadata::Memory(read_memory_metadata(cursor)?)),
        3 => Ok(AotImportDescMetadata::Global(read_global_type_metadata(
            cursor,
        )?)),
        _ => Err(AotMetadataError::InvalidHeader(
            "aot metadata: invalid import descriptor tag".to_string(),
        )),
    }
}

fn checked_vec_len(
    cursor: &Cursor<&[u8]>,
    len: u64,
    element_size: usize,
    context: &str,
) -> Result<usize, AotMetadataError> {
    let len =
        usize::try_from(len).map_err(|_| AotMetadataError::InvalidHeader(context.to_string()))?;
    let bytes_needed = len
        .checked_mul(element_size)
        .ok_or_else(|| AotMetadataError::InvalidHeader(context.to_string()))?;
    if bytes_needed > remaining(cursor) {
        return Err(AotMetadataError::InvalidHeader(context.to_string()));
    }
    Ok(len)
}

fn remaining(cursor: &Cursor<&[u8]>) -> usize {
    cursor
        .get_ref()
        .len()
        .saturating_sub(cursor.position() as usize)
}

fn read_exact(
    cursor: &mut Cursor<&[u8]>,
    buf: &mut [u8],
    context: &str,
) -> Result<(), AotMetadataError> {
    cursor
        .read_exact(buf)
        .map_err(|err| AotMetadataError::Io(format!("{context}: {err}")))
}

fn read_u8(cursor: &mut Cursor<&[u8]>) -> Result<u8, AotMetadataError> {
    let mut buf = [0u8; 1];
    read_exact(cursor, &mut buf, "aot metadata: invalid u8")?;
    Ok(buf[0])
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> Result<u32, AotMetadataError> {
    let mut buf = [0u8; 4];
    read_exact(cursor, &mut buf, "aot metadata: invalid u32")?;
    Ok(u32::from_le_bytes(buf))
}

fn read_i32(cursor: &mut Cursor<&[u8]>) -> Result<i32, AotMetadataError> {
    let mut buf = [0u8; 4];
    read_exact(cursor, &mut buf, "aot metadata: invalid i32")?;
    Ok(i32::from_le_bytes(buf))
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> Result<u64, AotMetadataError> {
    let mut buf = [0u8; 8];
    read_exact(cursor, &mut buf, "aot metadata: invalid u64")?;
    Ok(u64::from_le_bytes(buf))
}

fn read_u64_named(cursor: &mut Cursor<&[u8]>, context: &str) -> Result<u64, AotMetadataError> {
    let mut buf = [0u8; 8];
    read_exact(cursor, &mut buf, context)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_i64(cursor: &mut Cursor<&[u8]>) -> Result<i64, AotMetadataError> {
    let mut buf = [0u8; 8];
    read_exact(cursor, &mut buf, "aot metadata: invalid i64")?;
    Ok(i64::from_le_bytes(buf))
}

#[cfg(test)]
mod tests {
    use super::{
        current_helper_metadata, deserialize_aot_metadata, relocations_for_function,
        serialize_aot_metadata, AotCompiledMetadata, AotDataSegmentMetadata,
        AotElementSegmentMetadata, AotExecutionContextMetadata, AotExportMetadata,
        AotFunctionMetadata, AotFunctionTypeMetadata, AotGlobalInitializerMetadata,
        AotGlobalTypeMetadata, AotHelperId, AotImportDescMetadata, AotImportMetadata,
        AotMemoryMetadata, AotModuleContextMetadata, AotModuleShapeMetadata, AotSourceMapEntry,
        AotTableMetadata, AotTarget, AotTargetArchitecture, AotTargetOperatingSystem,
        AOT_METADATA_MAGIC, EXECUTION_CONTEXT_ABI_VERSION,
    };
    use crate::backend::RelocationInfo;
    use crate::call_engine::ExecutionContext;
    use crate::ssa::FuncRef;
    use crate::wazevoapi::offsetdata::{
        EXECUTION_CONTEXT_OFFSET_CHECK_MODULE_EXIT_CODE_TRAMPOLINE_ADDRESS,
        EXECUTION_CONTEXT_OFFSET_FUEL, EXECUTION_CONTEXT_OFFSET_MEMMOVE_ADDRESS,
        EXECUTION_CONTEXT_OFFSET_MEMORY_GROW_TRAMPOLINE_ADDRESS,
        EXECUTION_CONTEXT_OFFSET_MEMORY_NOTIFY_TRAMPOLINE_ADDRESS,
        EXECUTION_CONTEXT_OFFSET_MEMORY_WAIT32_TRAMPOLINE_ADDRESS,
        EXECUTION_CONTEXT_OFFSET_MEMORY_WAIT64_TRAMPOLINE_ADDRESS,
        EXECUTION_CONTEXT_OFFSET_REF_FUNC_TRAMPOLINE_ADDRESS,
        EXECUTION_CONTEXT_OFFSET_STACK_GROW_CALL_TRAMPOLINE_ADDRESS,
        EXECUTION_CONTEXT_OFFSET_TABLE_GROW_TRAMPOLINE_ADDRESS,
    };
    use crate::wazevoapi::ExitCode;
    use razero_wasm::module::{ElementMode, ExternType, RefType, ValueType};
    use std::mem::size_of;

    #[test]
    fn metadata_round_trips() {
        let metadata = AotCompiledMetadata {
            target: AotTarget {
                architecture: AotTargetArchitecture::X86_64,
                operating_system: AotTargetOperatingSystem::Linux,
            },
            module_id: [7; 32],
            import_function_count: 3,
            entry_preamble_offsets: vec![0, 16],
            types: vec![AotFunctionTypeMetadata {
                params: vec![ValueType::I32, ValueType::V128],
                results: vec![ValueType::I64],
                param_num_in_u64: 3,
                result_num_in_u64: 1,
            }],
            imports: vec![
                AotImportMetadata {
                    ty: ExternType::FUNC,
                    module: "env".to_string(),
                    name: "host".to_string(),
                    desc: AotImportDescMetadata::Func(0),
                    index_per_type: 0,
                },
                AotImportMetadata {
                    ty: ExternType::FUNC,
                    module: "env".to_string(),
                    name: "host1".to_string(),
                    desc: AotImportDescMetadata::Func(0),
                    index_per_type: 1,
                },
                AotImportMetadata {
                    ty: ExternType::FUNC,
                    module: "env".to_string(),
                    name: "host2".to_string(),
                    desc: AotImportDescMetadata::Func(0),
                    index_per_type: 2,
                },
                AotImportMetadata {
                    ty: ExternType::MEMORY,
                    module: "env".to_string(),
                    name: "memory".to_string(),
                    desc: AotImportDescMetadata::Memory(AotMemoryMetadata {
                        min: 1,
                        cap: 2,
                        max: 3,
                        is_max_encoded: true,
                        is_shared: false,
                    }),
                    index_per_type: 0,
                },
                AotImportMetadata {
                    ty: ExternType::TABLE,
                    module: "env".to_string(),
                    name: "table".to_string(),
                    desc: AotImportDescMetadata::Table(AotTableMetadata {
                        min: 4,
                        max: Some(8),
                        ty: RefType::FUNCREF,
                    }),
                    index_per_type: 0,
                },
                AotImportMetadata {
                    ty: ExternType::GLOBAL,
                    module: "env".to_string(),
                    name: "global".to_string(),
                    desc: AotImportDescMetadata::Global(AotGlobalTypeMetadata {
                        val_type: ValueType::I64,
                        mutable: true,
                    }),
                    index_per_type: 0,
                },
            ],
            exports: vec![
                AotExportMetadata {
                    ty: ExternType::FUNC,
                    name: "_start".to_string(),
                    index: 3,
                },
                AotExportMetadata {
                    ty: ExternType::MEMORY,
                    name: "memory".to_string(),
                    index: 0,
                },
            ],
            start_function_index: Some(3),
            data_segments: vec![AotDataSegmentMetadata {
                offset_expression: vec![0x41, 0x00, 0x0b],
                init: b"hello".to_vec(),
                passive: false,
            }],
            global_initializers: vec![AotGlobalInitializerMetadata {
                init_expression: vec![0x41, 0x01, 0x0b],
            }],
            element_segments: vec![AotElementSegmentMetadata {
                offset_expression: vec![0x41, 0x00, 0x0b],
                table_index: 0,
                init_expressions: vec![vec![0xd2, 0x03, 0x0b]],
                ty: RefType::FUNCREF,
                mode: ElementMode::Active,
            }],
            memory: Some(AotMemoryMetadata {
                min: 2,
                cap: 3,
                max: 5,
                is_max_encoded: true,
                is_shared: false,
            }),
            tables: vec![AotTableMetadata {
                min: 6,
                max: Some(9),
                ty: RefType::EXTERNREF,
            }],
            globals: vec![AotGlobalTypeMetadata {
                val_type: ValueType::I32,
                mutable: false,
            }],
            module_shape: AotModuleShapeMetadata {
                enabled_features: 0x55,
                import_function_count: 3,
                import_global_count: 1,
                import_memory_count: 1,
                import_table_count: 1,
                local_function_count: 1,
                local_global_count: 1,
                local_table_count: 1,
                has_local_memory: true,
                has_any_memory: true,
                has_start_section: true,
                data_segment_count: 1,
                element_segment_count: 1,
                is_host_module: false,
            },
            functions: vec![AotFunctionMetadata {
                local_function_index: 0,
                wasm_function_index: 3,
                type_index: 1,
                executable_offset: 32,
                executable_len: 12,
            }],
            relocations: vec![super::AotRelocationMetadata {
                source_wasm_function_index: 3,
                target_function_index: 1,
                executable_offset: 36,
                is_tail_call: false,
            }],
            module_context: AotModuleContextMetadata {
                total_size: 64,
                module_instance_offset: 0,
                local_memory_begin: 8,
                imported_memory_begin: -1,
                imported_functions_begin: 24,
                globals_begin: 40,
                type_ids_1st_element: 56,
                tables_begin: 64,
                before_listener_trampolines_1st_element: -1,
                after_listener_trampolines_1st_element: -1,
                data_instances_1st_element: 72,
                element_instances_1st_element: 80,
            },
            execution_context: AotExecutionContextMetadata {
                abi_version: 7,
                size: 1192,
                exit_code_offset: 0,
                caller_module_context_ptr_offset: 8,
                original_frame_pointer_offset: 16,
                original_stack_pointer_offset: 24,
                go_return_address_offset: 32,
                stack_bottom_ptr_offset: 40,
                go_call_return_address_offset: 48,
                stack_pointer_before_go_call_offset: 56,
                stack_grow_required_size_offset: 64,
                memory_grow_trampoline_address_offset: 72,
                stack_grow_call_trampoline_address_offset: 80,
                check_module_exit_code_trampoline_address_offset: 88,
                saved_registers_offset: 96,
                go_function_call_callee_module_context_opaque_offset: 1120,
                table_grow_trampoline_address_offset: 1128,
                ref_func_trampoline_address_offset: 1136,
                memmove_address_offset: 1144,
                frame_pointer_before_go_call_offset: 1152,
                memory_wait32_trampoline_address_offset: 1160,
                memory_wait64_trampoline_address_offset: 1168,
                memory_notify_trampoline_address_offset: 1176,
                fuel_offset: 1184,
            },
            helpers: vec![
                super::AotHelperMetadata {
                    id: AotHelperId::MemoryGrow,
                    execution_context_offset: 72,
                    exit_code: Some(ExitCode::GROW_MEMORY),
                },
                super::AotHelperMetadata {
                    id: AotHelperId::Memmove,
                    execution_context_offset: 1144,
                    exit_code: None,
                },
            ],
            source_map: vec![AotSourceMapEntry {
                wasm_binary_offset: 11,
                executable_offset: 32,
            }],
            ensure_termination: true,
            memory_isolation_enabled: true,
        };

        let encoded = serialize_aot_metadata(&metadata);
        let decoded = deserialize_aot_metadata(&encoded).unwrap();
        assert_eq!(decoded, metadata);
    }

    #[test]
    fn helper_abi_defaults_match_runtime_contract() {
        let execution_context = AotExecutionContextMetadata::current();
        assert_eq!(execution_context.abi_version, EXECUTION_CONTEXT_ABI_VERSION);
        assert_eq!(execution_context.size, size_of::<ExecutionContext>());
        assert_eq!(
            execution_context.memory_grow_trampoline_address_offset,
            EXECUTION_CONTEXT_OFFSET_MEMORY_GROW_TRAMPOLINE_ADDRESS.raw()
        );
        assert_eq!(
            execution_context.memmove_address_offset,
            EXECUTION_CONTEXT_OFFSET_MEMMOVE_ADDRESS.raw()
        );
        assert_eq!(
            execution_context.fuel_offset,
            EXECUTION_CONTEXT_OFFSET_FUEL.raw()
        );

        assert_eq!(
            current_helper_metadata(),
            vec![
                super::AotHelperMetadata {
                    id: AotHelperId::MemoryGrow,
                    execution_context_offset:
                        EXECUTION_CONTEXT_OFFSET_MEMORY_GROW_TRAMPOLINE_ADDRESS.raw(),
                    exit_code: Some(ExitCode::GROW_MEMORY),
                },
                super::AotHelperMetadata {
                    id: AotHelperId::StackGrow,
                    execution_context_offset:
                        EXECUTION_CONTEXT_OFFSET_STACK_GROW_CALL_TRAMPOLINE_ADDRESS.raw(),
                    exit_code: None,
                },
                super::AotHelperMetadata {
                    id: AotHelperId::CheckModuleExitCode,
                    execution_context_offset:
                        EXECUTION_CONTEXT_OFFSET_CHECK_MODULE_EXIT_CODE_TRAMPOLINE_ADDRESS.raw(),
                    exit_code: Some(ExitCode::CHECK_MODULE_EXIT_CODE),
                },
                super::AotHelperMetadata {
                    id: AotHelperId::TableGrow,
                    execution_context_offset:
                        EXECUTION_CONTEXT_OFFSET_TABLE_GROW_TRAMPOLINE_ADDRESS.raw(),
                    exit_code: Some(ExitCode::TABLE_GROW),
                },
                super::AotHelperMetadata {
                    id: AotHelperId::RefFunc,
                    execution_context_offset: EXECUTION_CONTEXT_OFFSET_REF_FUNC_TRAMPOLINE_ADDRESS
                        .raw(),
                    exit_code: Some(ExitCode::REF_FUNC),
                },
                super::AotHelperMetadata {
                    id: AotHelperId::Memmove,
                    execution_context_offset: EXECUTION_CONTEXT_OFFSET_MEMMOVE_ADDRESS.raw(),
                    exit_code: None,
                },
                super::AotHelperMetadata {
                    id: AotHelperId::MemoryWait32,
                    execution_context_offset:
                        EXECUTION_CONTEXT_OFFSET_MEMORY_WAIT32_TRAMPOLINE_ADDRESS.raw(),
                    exit_code: Some(ExitCode::MEMORY_WAIT32),
                },
                super::AotHelperMetadata {
                    id: AotHelperId::MemoryWait64,
                    execution_context_offset:
                        EXECUTION_CONTEXT_OFFSET_MEMORY_WAIT64_TRAMPOLINE_ADDRESS.raw(),
                    exit_code: Some(ExitCode::MEMORY_WAIT64),
                },
                super::AotHelperMetadata {
                    id: AotHelperId::MemoryNotify,
                    execution_context_offset:
                        EXECUTION_CONTEXT_OFFSET_MEMORY_NOTIFY_TRAMPOLINE_ADDRESS.raw(),
                    exit_code: Some(ExitCode::MEMORY_NOTIFY),
                },
            ]
        );
    }

    #[test]
    fn legacy_metadata_without_abi_tail_still_deserializes() {
        let metadata = AotCompiledMetadata {
            module_id: [1; 32],
            functions: vec![AotFunctionMetadata {
                local_function_index: 0,
                wasm_function_index: 0,
                type_index: 0,
                executable_offset: 8,
                executable_len: 4,
            }],
            module_shape: AotModuleShapeMetadata {
                local_function_count: 1,
                ..AotModuleShapeMetadata::default()
            },
            ..AotCompiledMetadata::default()
        };

        let mut encoded = serialize_aot_metadata(&metadata);
        let trailing_metadata_len =
            4 + 1 + 4 + 8 + (22 * 4) + 4 + metadata.helpers.len() * 10 + 4 + 4 + 4;
        encoded.truncate(encoded.len() - trailing_metadata_len);

        let decoded = deserialize_aot_metadata(&encoded).unwrap();
        assert!(decoded.imports.is_empty());
        assert!(decoded.memory.is_none());
        assert!(decoded.tables.is_empty());
        assert!(decoded.globals.is_empty());
        assert_eq!(decoded.functions, metadata.functions);
        assert_eq!(
            decoded.execution_context,
            AotExecutionContextMetadata::current()
        );
        assert_eq!(decoded.helpers, current_helper_metadata());
        assert!(decoded.data_segments.is_empty());
    }

    #[test]
    fn deserialize_rejects_invalid_memory_presence_flag() {
        let metadata = AotCompiledMetadata {
            memory: Some(AotMemoryMetadata {
                min: 1,
                cap: 1,
                max: 1,
                is_max_encoded: true,
                is_shared: false,
            }),
            ..AotCompiledMetadata::default()
        };

        let mut encoded = serialize_aot_metadata(&metadata);
        let memory_flag_offset =
            AOT_METADATA_MAGIC.len() + 2 + 32 + 4 + 2 + 4 + 4 + 48 + 4 + 4 + 52 + 8 + 4;
        assert_eq!(encoded[memory_flag_offset], 1);
        encoded[memory_flag_offset] = 2;

        let err = deserialize_aot_metadata(&encoded).unwrap_err();
        assert_eq!(
            err.to_string(),
            "aot metadata: invalid memory metadata flag"
        );
    }

    #[test]
    fn relocations_are_rebased_to_module_offsets() {
        let relocations = relocations_for_function(
            5,
            64,
            &[RelocationInfo {
                offset: 7,
                func_ref: FuncRef(2),
                is_tail_call: true,
            }],
        );
        assert_eq!(1, relocations.len());
        assert_eq!(5, relocations[0].source_wasm_function_index);
        assert_eq!(2, relocations[0].target_function_index);
        assert_eq!(71, relocations[0].executable_offset);
        assert!(relocations[0].is_tail_call);
    }
}
