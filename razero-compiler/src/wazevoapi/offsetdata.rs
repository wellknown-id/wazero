//! Offsets shared by compiler and runtime glue.

use core::fmt;
use core::ops::{Add, AddAssign};

pub const FUNCTION_INSTANCE_SIZE: Offset = Offset(24);
pub const FUNCTION_INSTANCE_EXECUTABLE_OFFSET: Offset = Offset(0);
pub const FUNCTION_INSTANCE_MODULE_CONTEXT_OPAQUE_PTR_OFFSET: Offset = Offset(8);
pub const FUNCTION_INSTANCE_TYPE_ID_OFFSET: Offset = Offset(16);

pub const EXECUTION_CONTEXT_OFFSET_EXIT_CODE_OFFSET: Offset = Offset(0);
pub const EXECUTION_CONTEXT_OFFSET_CALLER_MODULE_CONTEXT_PTR: Offset = Offset(8);
pub const EXECUTION_CONTEXT_OFFSET_ORIGINAL_FRAME_POINTER: Offset = Offset(16);
pub const EXECUTION_CONTEXT_OFFSET_ORIGINAL_STACK_POINTER: Offset = Offset(24);
pub const EXECUTION_CONTEXT_OFFSET_GO_RETURN_ADDRESS: Offset = Offset(32);
pub const EXECUTION_CONTEXT_OFFSET_STACK_BOTTOM_PTR: Offset = Offset(40);
pub const EXECUTION_CONTEXT_OFFSET_GO_CALL_RETURN_ADDRESS: Offset = Offset(48);
pub const EXECUTION_CONTEXT_OFFSET_STACK_POINTER_BEFORE_GO_CALL: Offset = Offset(56);
pub const EXECUTION_CONTEXT_OFFSET_STACK_GROW_REQUIRED_SIZE: Offset = Offset(64);
pub const EXECUTION_CONTEXT_OFFSET_MEMORY_GROW_TRAMPOLINE_ADDRESS: Offset = Offset(72);
pub const EXECUTION_CONTEXT_OFFSET_STACK_GROW_CALL_TRAMPOLINE_ADDRESS: Offset = Offset(80);
pub const EXECUTION_CONTEXT_OFFSET_CHECK_MODULE_EXIT_CODE_TRAMPOLINE_ADDRESS: Offset = Offset(88);
pub const EXECUTION_CONTEXT_OFFSET_SAVED_REGISTERS_BEGIN: Offset = Offset(96);
pub const EXECUTION_CONTEXT_OFFSET_GO_FUNCTION_CALL_CALLEE_MODULE_CONTEXT_OPAQUE: Offset =
    Offset(1120);
pub const EXECUTION_CONTEXT_OFFSET_TABLE_GROW_TRAMPOLINE_ADDRESS: Offset = Offset(1128);
pub const EXECUTION_CONTEXT_OFFSET_REF_FUNC_TRAMPOLINE_ADDRESS: Offset = Offset(1136);
pub const EXECUTION_CONTEXT_OFFSET_MEMMOVE_ADDRESS: Offset = Offset(1144);
pub const EXECUTION_CONTEXT_OFFSET_FRAME_POINTER_BEFORE_GO_CALL: Offset = Offset(1152);
pub const EXECUTION_CONTEXT_OFFSET_MEMORY_WAIT32_TRAMPOLINE_ADDRESS: Offset = Offset(1160);
pub const EXECUTION_CONTEXT_OFFSET_MEMORY_WAIT64_TRAMPOLINE_ADDRESS: Offset = Offset(1168);
pub const EXECUTION_CONTEXT_OFFSET_MEMORY_NOTIFY_TRAMPOLINE_ADDRESS: Offset = Offset(1176);
pub const EXECUTION_CONTEXT_OFFSET_FUEL: Offset = Offset(1184);

#[derive(Copy, Clone, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Offset(i32);

impl Offset {
    pub const INVALID: Self = Self(-1);

    pub const fn new(value: i32) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> i32 {
        self.0
    }

    pub const fn u32(self) -> u32 {
        self.0 as u32
    }

    pub const fn i64(self) -> i64 {
        self.0 as i64
    }

    pub const fn u64(self) -> u64 {
        self.0 as u64
    }
}

impl From<i32> for Offset {
    fn from(value: i32) -> Self {
        Self(value)
    }
}

impl Add for Offset {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for Offset {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl fmt::Debug for Offset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Offset({})", self.0)
    }
}

pub trait ModuleContextOffsetSource {
    fn has_memory(&self) -> bool;
    fn import_memory_count(&self) -> u32;
    fn import_function_count(&self) -> u32;
    fn import_global_count(&self) -> u32;
    fn global_count(&self) -> usize;
    fn import_table_count(&self) -> u32;
    fn table_count(&self) -> usize;
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ModuleContextOffsetData {
    pub total_size: usize,
    pub module_instance_offset: Offset,
    pub local_memory_begin: Offset,
    pub imported_memory_begin: Offset,
    pub imported_functions_begin: Offset,
    pub globals_begin: Offset,
    pub type_ids_1st_element: Offset,
    pub tables_begin: Offset,
    pub before_listener_trampolines_1st_element: Offset,
    pub after_listener_trampolines_1st_element: Offset,
    pub data_instances_1st_element: Offset,
    pub element_instances_1st_element: Offset,
}

impl ModuleContextOffsetData {
    pub fn new(module: &impl ModuleContextOffsetSource, with_listener: bool) -> Self {
        let mut ret = Self {
            local_memory_begin: Offset::INVALID,
            imported_memory_begin: Offset::INVALID,
            imported_functions_begin: Offset::INVALID,
            globals_begin: Offset::INVALID,
            type_ids_1st_element: Offset::INVALID,
            tables_begin: Offset::INVALID,
            before_listener_trampolines_1st_element: Offset::INVALID,
            after_listener_trampolines_1st_element: Offset::INVALID,
            ..Self::default()
        };
        let mut offset = Offset::new(0);

        ret.module_instance_offset = Offset::new(0);
        offset += Offset::new(8);

        if module.has_memory() {
            ret.local_memory_begin = offset;
            offset += Offset::new(16);
        }

        if module.import_memory_count() > 0 {
            offset = align8(offset);
            ret.imported_memory_begin = offset;
            offset += Offset::new(16);
        }

        if module.import_function_count() > 0 {
            offset = align8(offset);
            ret.imported_functions_begin = offset;
            offset +=
                Offset::new(module.import_function_count() as i32 * FUNCTION_INSTANCE_SIZE.raw());
        }

        let globals = module.import_global_count() as usize + module.global_count();
        if globals > 0 {
            offset = align16(offset);
            ret.globals_begin = offset;
            offset += Offset::new(globals as i32 * 16);
        }

        let tables = module.table_count() + module.import_table_count() as usize;
        if tables > 0 {
            offset = align8(offset);
            ret.type_ids_1st_element = offset;
            offset += Offset::new(8);

            ret.tables_begin = offset;
            offset += Offset::new(tables as i32 * 8);
        }

        if with_listener {
            offset = align8(offset);
            ret.before_listener_trampolines_1st_element = offset;
            offset += Offset::new(8);

            ret.after_listener_trampolines_1st_element = offset;
            offset += Offset::new(8);
        }

        ret.data_instances_1st_element = offset;
        offset += Offset::new(8);

        ret.element_instances_1st_element = offset;
        offset += Offset::new(8);

        ret.total_size = align16(offset).raw() as usize;
        ret
    }

    pub fn imported_function_offset(&self, index: u32) -> (Offset, Offset, Offset) {
        let base = self.imported_functions_begin
            + Offset::new(index as i32 * FUNCTION_INSTANCE_SIZE.raw());
        (
            base + FUNCTION_INSTANCE_EXECUTABLE_OFFSET,
            base + FUNCTION_INSTANCE_MODULE_CONTEXT_OPAQUE_PTR_OFFSET,
            base + FUNCTION_INSTANCE_TYPE_ID_OFFSET,
        )
    }

    pub fn global_instance_offset(&self, index: usize) -> Offset {
        self.globals_begin + Offset::new(index as i32 * 16)
    }

    pub fn local_memory_base(&self) -> Offset {
        self.local_memory_begin
    }

    pub fn local_memory_len(&self) -> Offset {
        if self.local_memory_begin.raw() >= 0 {
            self.local_memory_begin + Offset::new(8)
        } else {
            Offset::INVALID
        }
    }

    pub fn table_offset(&self, table_index: usize) -> Offset {
        self.tables_begin + Offset::new(table_index as i32 * 8)
    }
}

const fn align16(offset: Offset) -> Offset {
    Offset((offset.raw() + 15) & !15)
}

const fn align8(offset: Offset) -> Offset {
    Offset((offset.raw() + 7) & !7)
}

#[cfg(test)]
mod tests {
    use super::{
        align16, ModuleContextOffsetData, ModuleContextOffsetSource, Offset, FUNCTION_INSTANCE_SIZE,
    };

    #[derive(Default)]
    struct StubModule {
        has_memory: bool,
        import_memory_count: u32,
        import_function_count: u32,
        import_global_count: u32,
        global_count: usize,
        import_table_count: u32,
        table_count: usize,
    }

    impl ModuleContextOffsetSource for StubModule {
        fn has_memory(&self) -> bool {
            self.has_memory
        }

        fn import_memory_count(&self) -> u32 {
            self.import_memory_count
        }

        fn import_function_count(&self) -> u32 {
            self.import_function_count
        }

        fn import_global_count(&self) -> u32 {
            self.import_global_count
        }

        fn global_count(&self) -> usize {
            self.global_count
        }

        fn import_table_count(&self) -> u32 {
            self.import_table_count
        }

        fn table_count(&self) -> usize {
            self.table_count
        }
    }

    #[test]
    fn new_module_context_offset_data_matches_go_layouts() {
        let cases = [
            (
                "empty",
                StubModule::default(),
                false,
                ModuleContextOffsetData {
                    local_memory_begin: Offset::INVALID,
                    imported_memory_begin: Offset::INVALID,
                    imported_functions_begin: Offset::INVALID,
                    globals_begin: Offset::INVALID,
                    type_ids_1st_element: Offset::INVALID,
                    tables_begin: Offset::INVALID,
                    before_listener_trampolines_1st_element: Offset::INVALID,
                    after_listener_trampolines_1st_element: Offset::INVALID,
                    data_instances_1st_element: Offset::new(8),
                    element_instances_1st_element: Offset::new(16),
                    total_size: 32,
                    ..ModuleContextOffsetData::default()
                },
            ),
            (
                "local mem",
                StubModule {
                    has_memory: true,
                    ..StubModule::default()
                },
                false,
                ModuleContextOffsetData {
                    local_memory_begin: Offset::new(8),
                    imported_memory_begin: Offset::INVALID,
                    imported_functions_begin: Offset::INVALID,
                    globals_begin: Offset::INVALID,
                    type_ids_1st_element: Offset::INVALID,
                    tables_begin: Offset::INVALID,
                    before_listener_trampolines_1st_element: Offset::INVALID,
                    after_listener_trampolines_1st_element: Offset::INVALID,
                    data_instances_1st_element: Offset::new(24),
                    element_instances_1st_element: Offset::new(32),
                    total_size: 48,
                    ..ModuleContextOffsetData::default()
                },
            ),
            (
                "imported mem",
                StubModule {
                    import_memory_count: 1,
                    ..StubModule::default()
                },
                false,
                ModuleContextOffsetData {
                    local_memory_begin: Offset::INVALID,
                    imported_memory_begin: Offset::new(8),
                    imported_functions_begin: Offset::INVALID,
                    globals_begin: Offset::INVALID,
                    type_ids_1st_element: Offset::INVALID,
                    tables_begin: Offset::INVALID,
                    before_listener_trampolines_1st_element: Offset::INVALID,
                    after_listener_trampolines_1st_element: Offset::INVALID,
                    data_instances_1st_element: Offset::new(24),
                    element_instances_1st_element: Offset::new(32),
                    total_size: 48,
                    ..ModuleContextOffsetData::default()
                },
            ),
            (
                "imported func",
                StubModule {
                    import_function_count: 10,
                    ..StubModule::default()
                },
                false,
                ModuleContextOffsetData {
                    local_memory_begin: Offset::INVALID,
                    imported_memory_begin: Offset::INVALID,
                    imported_functions_begin: Offset::new(8),
                    globals_begin: Offset::INVALID,
                    type_ids_1st_element: Offset::INVALID,
                    tables_begin: Offset::INVALID,
                    before_listener_trampolines_1st_element: Offset::INVALID,
                    after_listener_trampolines_1st_element: Offset::INVALID,
                    data_instances_1st_element: Offset::new(10 * FUNCTION_INSTANCE_SIZE.raw() + 8),
                    element_instances_1st_element: Offset::new(
                        10 * FUNCTION_INSTANCE_SIZE.raw() + 16,
                    ),
                    total_size: align16(Offset::new(10 * FUNCTION_INSTANCE_SIZE.raw() + 24)).raw()
                        as usize,
                    ..ModuleContextOffsetData::default()
                },
            ),
            (
                "imported func/mem",
                StubModule {
                    import_memory_count: 1,
                    import_function_count: 10,
                    ..StubModule::default()
                },
                false,
                ModuleContextOffsetData {
                    local_memory_begin: Offset::INVALID,
                    imported_memory_begin: Offset::new(8),
                    imported_functions_begin: Offset::new(24),
                    globals_begin: Offset::INVALID,
                    type_ids_1st_element: Offset::INVALID,
                    tables_begin: Offset::INVALID,
                    before_listener_trampolines_1st_element: Offset::INVALID,
                    after_listener_trampolines_1st_element: Offset::INVALID,
                    data_instances_1st_element: Offset::new(10 * FUNCTION_INSTANCE_SIZE.raw() + 24),
                    element_instances_1st_element: Offset::new(
                        10 * FUNCTION_INSTANCE_SIZE.raw() + 32,
                    ),
                    total_size: align16(Offset::new(10 * FUNCTION_INSTANCE_SIZE.raw() + 40)).raw()
                        as usize,
                    ..ModuleContextOffsetData::default()
                },
            ),
            (
                "local mem / imported func / globals / tables",
                StubModule {
                    has_memory: true,
                    import_function_count: 10,
                    import_global_count: 10,
                    global_count: 20,
                    import_table_count: 5,
                    table_count: 10,
                    ..StubModule::default()
                },
                false,
                ModuleContextOffsetData {
                    local_memory_begin: Offset::new(8),
                    imported_memory_begin: Offset::INVALID,
                    imported_functions_begin: Offset::new(24),
                    globals_begin: Offset::new(32 + 10 * FUNCTION_INSTANCE_SIZE.raw()),
                    type_ids_1st_element: Offset::new(
                        32 + 10 * FUNCTION_INSTANCE_SIZE.raw() + 16 * 30,
                    ),
                    tables_begin: Offset::new(32 + 10 * FUNCTION_INSTANCE_SIZE.raw() + 16 * 30 + 8),
                    before_listener_trampolines_1st_element: Offset::INVALID,
                    after_listener_trampolines_1st_element: Offset::INVALID,
                    data_instances_1st_element: Offset::new(
                        32 + 10 * FUNCTION_INSTANCE_SIZE.raw() + 16 * 30 + 8 + 8 * 15,
                    ),
                    element_instances_1st_element: Offset::new(
                        32 + 10 * FUNCTION_INSTANCE_SIZE.raw() + 16 * 30 + 8 + 8 * 15 + 8,
                    ),
                    total_size: (32 + 10 * FUNCTION_INSTANCE_SIZE.raw() + 16 * 30 + 8 + 8 * 15 + 16)
                        as usize,
                    ..ModuleContextOffsetData::default()
                },
            ),
            (
                "local mem / imported func / globals / tables / listener",
                StubModule {
                    has_memory: true,
                    import_function_count: 10,
                    import_global_count: 10,
                    global_count: 20,
                    import_table_count: 5,
                    table_count: 10,
                    ..StubModule::default()
                },
                true,
                ModuleContextOffsetData {
                    local_memory_begin: Offset::new(8),
                    imported_memory_begin: Offset::INVALID,
                    imported_functions_begin: Offset::new(24),
                    globals_begin: Offset::new(32 + 10 * FUNCTION_INSTANCE_SIZE.raw()),
                    type_ids_1st_element: Offset::new(
                        32 + 10 * FUNCTION_INSTANCE_SIZE.raw() + 16 * 30,
                    ),
                    tables_begin: Offset::new(32 + 10 * FUNCTION_INSTANCE_SIZE.raw() + 16 * 30 + 8),
                    before_listener_trampolines_1st_element: Offset::new(
                        32 + 10 * FUNCTION_INSTANCE_SIZE.raw() + 16 * 30 + 8 + 8 * 15,
                    ),
                    after_listener_trampolines_1st_element: Offset::new(
                        32 + 10 * FUNCTION_INSTANCE_SIZE.raw() + 16 * 30 + 8 + 8 * 15 + 8,
                    ),
                    data_instances_1st_element: Offset::new(
                        32 + 10 * FUNCTION_INSTANCE_SIZE.raw() + 16 * 30 + 8 + 8 * 15 + 16,
                    ),
                    element_instances_1st_element: Offset::new(
                        32 + 10 * FUNCTION_INSTANCE_SIZE.raw() + 16 * 30 + 8 + 8 * 15 + 24,
                    ),
                    total_size: (32 + 10 * FUNCTION_INSTANCE_SIZE.raw() + 16 * 30 + 8 + 8 * 15 + 32)
                        as usize,
                    ..ModuleContextOffsetData::default()
                },
            ),
        ];

        for (name, module, with_listener, expected) in cases {
            assert_eq!(
                ModuleContextOffsetData::new(&module, with_listener),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn helper_offsets_match_expected_strides() {
        let data = ModuleContextOffsetData {
            imported_functions_begin: Offset::new(24),
            globals_begin: Offset::new(256),
            local_memory_begin: Offset::new(8),
            tables_begin: Offset::new(512),
            ..ModuleContextOffsetData::default()
        };

        assert_eq!(
            data.imported_function_offset(2),
            (Offset::new(72), Offset::new(80), Offset::new(88))
        );
        assert_eq!(data.global_instance_offset(3), Offset::new(304));
        assert_eq!(data.local_memory_base(), Offset::new(8));
        assert_eq!(data.local_memory_len(), Offset::new(16));
        assert_eq!(data.table_offset(4), Offset::new(544));
    }
}
