#![doc = "Compiler engine glue."]

use std::collections::HashMap;
use std::sync::Arc;

use razero_wasm::engine::{Engine as WasmEngine, EngineError, ModuleEngine as WasmModuleEngine};
use razero_wasm::module::{Index, Module, ModuleId};
use razero_wasm::module_instance::ModuleInstance;

use crate::backend::{Compiler as BackendCompiler, Machine, RelocationInfo, SourceOffsetInfo};
use crate::engine_cache::{
    deserialize_compiled_module, file_cache_key, serialize_compiled_module, CachedCompiledModule,
    CompiledModuleCache,
};
use crate::frontend::{
    signature_for_wasm_function_type, wasm_type_to_ssa_type, Compiler as FrontendCompiler,
};
use crate::module_engine::CompilerModuleEngine;
use crate::ssa::{Builder, Signature, SignatureId, Type};
use crate::wazevoapi::{ExitCode, ModuleContextOffsetData, ModuleContextOffsetSource};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use crate::sighandler_linux_amd64 as signal_handler;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
use crate::sighandler_linux_arm64 as signal_handler;
#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64")
)))]
use crate::sighandler_stub as signal_handler;

#[cfg(target_arch = "x86_64")]
use crate::backend::isa::amd64::machine::Amd64Machine as NativeMachine;
#[cfg(target_arch = "aarch64")]
use crate::backend::isa::arm64::machine::Arm64Machine as NativeMachine;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlignedBytes {
    storage: Vec<u128>,
    len: usize,
}

impl AlignedBytes {
    pub fn zeroed(len: usize) -> Self {
        if len == 0 {
            return Self::default();
        }
        let words = len.div_ceil(16);
        Self {
            storage: vec![0; words],
            len,
        }
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let mut ret = Self::zeroed(bytes.len());
        ret.as_mut_slice().copy_from_slice(&bytes);
        ret
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_ptr(&self) -> *const u8 {
        if self.len == 0 {
            std::ptr::null()
        } else {
            self.storage.as_ptr().cast()
        }
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        if self.len == 0 {
            std::ptr::null_mut()
        } else {
            self.storage.as_mut_ptr().cast()
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        if self.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.as_ptr(), self.len) }
        }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        if self.len == 0 {
            &mut []
        } else {
            unsafe { std::slice::from_raw_parts_mut(self.as_mut_ptr(), self.len) }
        }
    }

    pub fn ptr_at(&self, offset: usize) -> Option<usize> {
        (offset < self.len).then_some(self.as_ptr() as usize + offset)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Executables {
    pub executable: AlignedBytes,
    pub entry_preambles: AlignedBytes,
    pub(crate) entry_preamble_offsets: Vec<usize>,
}

impl Executables {
    pub fn from_executable_bytes(bytes: Vec<u8>) -> Self {
        Self {
            executable: AlignedBytes::from_bytes(bytes),
            ..Self::default()
        }
    }

    pub fn entry_preamble_ptr(&self, index: usize) -> Option<usize> {
        self.entry_preamble_offsets
            .get(index)
            .and_then(|offset| self.entry_preambles.ptr_at(*offset))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListenerTrampolines {
    executable: AlignedBytes,
    before_offset: usize,
    after_offset: usize,
}

impl ListenerTrampolines {
    pub fn before_ptr(&self) -> Option<usize> {
        self.executable.ptr_at(self.before_offset)
    }

    pub fn after_ptr(&self) -> Option<usize> {
        self.executable.ptr_at(self.after_offset)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SharedFunctions {
    executable: AlignedBytes,
    memory_grow_offset: Option<usize>,
    check_module_exit_code_offset: Option<usize>,
    stack_grow_offset: Option<usize>,
    table_grow_offset: Option<usize>,
    ref_func_offset: Option<usize>,
    memory_wait32_offset: Option<usize>,
    memory_wait64_offset: Option<usize>,
    memory_notify_offset: Option<usize>,
    listener_trampolines: HashMap<String, ListenerTrampolines>,
}

impl SharedFunctions {
    pub fn memory_grow_ptr(&self) -> Option<usize> {
        self.memory_grow_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn check_module_exit_code_ptr(&self) -> Option<usize> {
        self.check_module_exit_code_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn stack_grow_ptr(&self) -> Option<usize> {
        self.stack_grow_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn table_grow_ptr(&self) -> Option<usize> {
        self.table_grow_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn ref_func_ptr(&self) -> Option<usize> {
        self.ref_func_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn memory_wait32_ptr(&self) -> Option<usize> {
        self.memory_wait32_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn memory_wait64_ptr(&self) -> Option<usize> {
        self.memory_wait64_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }

    pub fn memory_notify_ptr(&self) -> Option<usize> {
        self.memory_notify_offset
            .and_then(|offset| self.executable.ptr_at(offset))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceMap {
    pub executable_offsets: Vec<usize>,
    pub wasm_binary_offsets: Vec<u64>,
}

#[derive(Clone, Debug)]
pub struct CompiledModule {
    pub executables: Executables,
    pub function_offsets: Vec<usize>,
    pub module: Module,
    pub offsets: ModuleContextOffsetData,
    pub shared_functions: Arc<SharedFunctions>,
    pub ensure_termination: bool,
    pub fuel_enabled: bool,
    pub fuel: i64,
    pub memory_isolation_enabled: bool,
    pub source_map: SourceMap,
}

impl CompiledModule {
    pub fn function_ptr(&self, local_index: usize) -> Option<usize> {
        self.function_offsets
            .get(local_index)
            .and_then(|offset| self.executables.executable.ptr_at(*offset))
    }

    pub fn entry_preamble_ptr(&self, type_index: usize) -> Option<usize> {
        self.executables.entry_preamble_ptr(type_index)
    }

    pub fn function_index_of(&self, addr: usize) -> Option<Index> {
        let base = self.executables.executable.as_ptr() as usize;
        if base == 0 || addr < base || addr >= base + self.executables.executable.len() {
            return None;
        }
        let rel = addr - base;
        let index = self
            .function_offsets
            .partition_point(|offset| *offset <= rel);
        (index > 0).then_some((index - 1) as Index)
    }

    pub fn source_offset_for_pc(&self, addr: usize) -> u64 {
        let base = self.executables.executable.as_ptr() as usize;
        let rel = addr.saturating_sub(base);
        let index = self
            .source_map
            .executable_offsets
            .partition_point(|offset| *offset <= rel);
        if index == 0 {
            0
        } else {
            self.source_map.wasm_binary_offsets[index - 1]
        }
    }
}

#[derive(Clone)]
struct CompiledModuleWithCount {
    compiled_module: Arc<CompiledModule>,
    ref_count: usize,
}

pub struct CompilerEngine {
    version: String,
    compiled_modules: HashMap<ModuleId, CompiledModuleWithCount>,
    sorted_compiled_modules: Vec<Arc<CompiledModule>>,
    shared_functions: Arc<SharedFunctions>,
    cache: Option<Arc<dyn CompiledModuleCache>>,
}

impl Default for CompilerEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl CompilerEngine {
    pub fn new() -> Self {
        Self::with_cache(None)
    }

    pub fn with_cache(cache: Option<Arc<dyn CompiledModuleCache>>) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            compiled_modules: HashMap::new(),
            sorted_compiled_modules: Vec::new(),
            shared_functions: Arc::new(compile_shared_functions()),
            cache,
        }
    }

    pub fn compiled_module(&self, module: &Module) -> Option<Arc<CompiledModule>> {
        self.compiled_modules
            .get(&module.id)
            .map(|entry| entry.compiled_module.clone())
    }

    pub fn compiled_module_of_addr(&self, addr: usize) -> Option<Arc<CompiledModule>> {
        let index = self.sorted_compiled_modules.partition_point(|candidate| {
            candidate.executables.executable.as_ptr() as usize <= addr
        });
        let candidate = self.sorted_compiled_modules.get(index.checked_sub(1)?)?;
        let base = candidate.executables.executable.as_ptr() as usize;
        let end = base + candidate.executables.executable.len();
        (base <= addr && addr < end).then_some(candidate.clone())
    }

    fn add_compiled_module(
        &mut self,
        module: &Module,
        compiled: Arc<CompiledModule>,
    ) -> Arc<CompiledModule> {
        if let Some(existing) = self.compiled_modules.get_mut(&module.id) {
            existing.ref_count += 1;
            return existing.compiled_module.clone();
        }
        if !compiled.executables.executable.is_empty() {
            self.add_compiled_module_to_sorted_list(compiled.clone());
        }
        self.compiled_modules.insert(
            module.id,
            CompiledModuleWithCount {
                compiled_module: compiled.clone(),
                ref_count: 1,
            },
        );
        compiled
    }

    fn add_compiled_module_to_sorted_list(&mut self, compiled: Arc<CompiledModule>) {
        let ptr = compiled.executables.executable.as_ptr() as usize;
        let index = self
            .sorted_compiled_modules
            .partition_point(|existing| (existing.executables.executable.as_ptr() as usize) < ptr);
        self.sorted_compiled_modules.insert(index, compiled);
    }

    fn delete_compiled_module_from_sorted_list(&mut self, compiled: &Arc<CompiledModule>) {
        let ptr = compiled.executables.executable.as_ptr() as usize;
        if let Some(index) = self
            .sorted_compiled_modules
            .iter()
            .position(|candidate| candidate.executables.executable.as_ptr() as usize == ptr)
        {
            self.sorted_compiled_modules.remove(index);
        }
    }

    fn load_from_cache(&self, module: &Module) -> Result<Option<Arc<CompiledModule>>, EngineError> {
        let Some(cache) = &self.cache else {
            return Ok(None);
        };
        let Some(bytes) = cache.get(&file_cache_key(module)) else {
            return Ok(None);
        };
        let Some(cached) = deserialize_compiled_module(&self.version, &bytes)
            .map_err(|err| EngineError::new(err.to_string()))?
        else {
            cache.delete(&file_cache_key(module));
            return Ok(None);
        };
        Ok(Some(Arc::new(
            self.cached_module_into_compiled(module, cached),
        )))
    }

    fn cached_module_into_compiled(
        &self,
        module: &Module,
        cached: CachedCompiledModule,
    ) -> CompiledModule {
        CompiledModule {
            executables: cached.executables,
            function_offsets: cached.function_offsets,
            module: module.clone(),
            offsets: ModuleContextOffsetData::new(module, false),
            shared_functions: self.shared_functions.clone(),
            ensure_termination: false,
            fuel_enabled: false,
            fuel: 0,
            memory_isolation_enabled: false,
            source_map: cached.source_map,
        }
    }

    fn store_in_cache(&self, module: &Module, compiled: &CompiledModule) {
        if module.is_host_module {
            return;
        }
        if let Some(cache) = &self.cache {
            let bytes = serialize_compiled_module(
                &self.version,
                compiled.executables.executable.as_slice(),
                &compiled.function_offsets,
                &compiled.source_map,
            );
            cache.insert(file_cache_key(module), bytes);
        }
    }

    fn compile_module_impl(&self, module: &Module) -> Result<Arc<CompiledModule>, EngineError> {
        if module.is_host_module {
            self.compile_host_module(module)
        } else {
            self.compile_wasm_module(module)
        }
    }

    fn compile_host_module(&self, module: &Module) -> Result<Arc<CompiledModule>, EngineError> {
        let mut function_offsets = Vec::with_capacity(module.code_section.len());
        let mut executable = Vec::new();

        for (index, code) in module.code_section.iter().enumerate() {
            let host_func = code.host_func.as_ref().ok_or_else(|| {
                EngineError::new("host module function missing host implementation")
            })?;
            let mut signature = Signature::new(
                SignatureId(module.function_section[index]),
                vec![Type::I64, Type::I64],
                vec![],
            );
            let mut typ = module.type_section[module.function_section[index] as usize].clone();
            typ.cache_num_in_u64();
            signature
                .params
                .extend(typ.params.iter().copied().map(wasm_type_to_ssa_type));
            signature
                .results
                .extend(typ.results.iter().copied().map(wasm_type_to_ssa_type));
            let exit_code = if Arc::strong_count(host_func) > 0 {
                ExitCode::call_go_module_function_with_index(index, false)
            } else {
                ExitCode::call_go_function_with_index(index, false)
            };
            align16_vec(&mut executable);
            function_offsets.push(executable.len());
            executable.extend(native_compile_host_trampoline(exit_code, &signature)?);
        }

        Ok(Arc::new(CompiledModule {
            executables: Executables {
                executable: AlignedBytes::from_bytes(executable),
                ..Executables::default()
            },
            function_offsets,
            module: module.clone(),
            offsets: ModuleContextOffsetData::new(module, false),
            shared_functions: self.shared_functions.clone(),
            ensure_termination: false,
            fuel_enabled: false,
            fuel: 0,
            memory_isolation_enabled: false,
            source_map: SourceMap::default(),
        }))
    }

    fn compile_wasm_module(&self, module: &Module) -> Result<Arc<CompiledModule>, EngineError> {
        let executables = compile_entry_preambles(module)?;
        let mut function_offsets = Vec::with_capacity(module.code_section.len());
        let mut executable = Vec::new();
        let mut relocations = Vec::<(usize, Vec<RelocationInfo>)>::new();
        let mut source_map = SourceMap::default();

        for local_index in 0..module.code_section.len() {
            align16_vec(&mut executable);
            let function_offset = executable.len();
            function_offsets.push(function_offset);
            let compiled = compile_function(module, local_index)?;
            source_map.executable_offsets.extend(
                compiled
                    .source_offsets
                    .iter()
                    .map(|info| function_offset + info.executable_offset as usize),
            );
            source_map.wasm_binary_offsets.extend(
                compiled
                    .source_offsets
                    .iter()
                    .map(|info| info.source_offset.0 as u64),
            );
            executable.extend(compiled.code);
            relocations.push((function_offset, compiled.relocations));
        }

        let mut executable = AlignedBytes::from_bytes(executable);
        let mut ref_to_binary_offset =
            vec![0i32; module.import_function_count as usize + function_offsets.len()];
        for (local_index, offset) in function_offsets.iter().copied().enumerate() {
            ref_to_binary_offset[module.import_function_count as usize + local_index] =
                offset as i32;
        }
        for (function_offset, relocs) in relocations {
            native_resolve_relocations(
                &ref_to_binary_offset,
                module.import_function_count as usize,
                executable.as_mut_slice(),
                &relocs,
                &[],
                function_offset,
            );
        }

        let compiled = CompiledModule {
            executables: Executables {
                executable,
                entry_preambles: executables.0,
                entry_preamble_offsets: executables.1,
            },
            function_offsets,
            module: module.clone(),
            offsets: ModuleContextOffsetData::new(module, false),
            shared_functions: self.shared_functions.clone(),
            ensure_termination: false,
            fuel_enabled: false,
            fuel: 0,
            memory_isolation_enabled: signal_handler::signal_handler_supported(),
            source_map,
        };
        if compiled.memory_isolation_enabled && !compiled.executables.executable.is_empty() {
            let start = compiled.executables.executable.as_ptr() as usize;
            let end = start + compiled.executables.executable.len();
            signal_handler::register_jit_code_range(start, end);
        }
        Ok(Arc::new(compiled))
    }
}

impl WasmEngine for CompilerEngine {
    fn close(&mut self) -> Result<(), EngineError> {
        self.compiled_modules.clear();
        self.sorted_compiled_modules.clear();
        self.shared_functions = Arc::new(SharedFunctions::default());
        Ok(())
    }

    fn compile_module(&mut self, module: &Module) -> Result<(), EngineError> {
        if self.compiled_modules.contains_key(&module.id) {
            if let Some(existing) = self.compiled_modules.get_mut(&module.id) {
                existing.ref_count += 1;
            }
            return Ok(());
        }
        if let Some(cached) = self.load_from_cache(module)? {
            self.add_compiled_module(module, cached);
            return Ok(());
        }
        let compiled = self.compile_module_impl(module)?;
        self.store_in_cache(module, &compiled);
        self.add_compiled_module(module, compiled);
        Ok(())
    }

    fn compiled_module_count(&self) -> u32 {
        self.compiled_modules.len() as u32
    }

    fn delete_compiled_module(&mut self, module: &Module) {
        let Some(entry) = self.compiled_modules.get_mut(&module.id) else {
            return;
        };
        if entry.ref_count > 1 {
            entry.ref_count -= 1;
            return;
        }
        let compiled = entry.compiled_module.clone();
        self.delete_compiled_module_from_sorted_list(&compiled);
        self.compiled_modules.remove(&module.id);
    }

    fn new_module_engine(
        &self,
        module: &Module,
        instance: &ModuleInstance,
    ) -> Result<Box<dyn WasmModuleEngine>, EngineError> {
        let compiled = self.compiled_module(module).ok_or_else(|| {
            EngineError::new("source module must be compiled before instantiation")
        })?;
        let mut module_engine = Box::new(CompilerModuleEngine::new(compiled, instance.clone()));
        module_engine.done_instantiation();
        Ok(module_engine)
    }
}

impl ModuleContextOffsetSource for Module {
    fn has_memory(&self) -> bool {
        self.memory_section.is_some()
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
        self.global_section.len()
    }

    fn import_table_count(&self) -> u32 {
        self.import_table_count
    }

    fn table_count(&self) -> usize {
        self.table_section.len()
    }
}

#[derive(Debug)]
struct CompiledFunction {
    code: Vec<u8>,
    relocations: Vec<RelocationInfo>,
    source_offsets: Vec<SourceOffsetInfo>,
}

fn compile_function(module: &Module, local_index: usize) -> Result<CompiledFunction, EngineError> {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let mut frontend = FrontendCompiler::new(
            module,
            Builder::new(),
            Some(ModuleContextOffsetData::new(module, false)),
            false,
            false,
            module.dwarf_lines.is_some(),
            false,
            false,
        );
        frontend
            .init_with_module_function(module.import_function_count + local_index as u32, false);
        frontend.lower_to_ssa();
        let builder = std::mem::take(frontend.builder_mut());
        let mut backend = BackendCompiler::new(NativeMachine::new(), builder);
        let output = backend
            .compile()
            .map_err(|err| EngineError::new(err.to_string()))?;
        Ok(CompiledFunction {
            code: output.code,
            relocations: output.relocations,
            source_offsets: backend.source_offsets.clone(),
        })
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = (module, local_index);
        Err(EngineError::new("unsupported architecture"))
    }
}

fn compile_entry_preambles(module: &Module) -> Result<(AlignedBytes, Vec<usize>), EngineError> {
    let mut bytes = Vec::new();
    let mut offsets = Vec::with_capacity(module.type_section.len());
    for function_type in &module.type_section {
        let signature = signature_for_wasm_function_type(function_type);
        align16_vec(&mut bytes);
        offsets.push(bytes.len());
        bytes.extend(native_compile_entry_preamble(&signature)?);
    }
    Ok((AlignedBytes::from_bytes(bytes), offsets))
}

fn compile_shared_functions() -> SharedFunctions {
    let mut executable = Vec::new();
    let mut record = |buf: Vec<u8>, slot: &mut Option<usize>| {
        align16_vec(&mut executable);
        *slot = Some(executable.len());
        executable.extend(buf);
    };

    let mut shared = SharedFunctions::default();
    record(
        native_compile_host_trampoline(
            ExitCode::GROW_MEMORY,
            &Signature::new(SignatureId(0), vec![Type::I64, Type::I32], vec![Type::I32]),
        )
        .unwrap_or_default(),
        &mut shared.memory_grow_offset,
    );
    record(
        native_compile_host_trampoline(
            ExitCode::TABLE_GROW,
            &Signature::new(
                SignatureId(1),
                vec![Type::I64, Type::I32, Type::I32, Type::I64],
                vec![Type::I32],
            ),
        )
        .unwrap_or_default(),
        &mut shared.table_grow_offset,
    );
    record(
        native_compile_host_trampoline(
            ExitCode::CHECK_MODULE_EXIT_CODE,
            &Signature::new(SignatureId(2), vec![Type::I64], vec![Type::I32]),
        )
        .unwrap_or_default(),
        &mut shared.check_module_exit_code_offset,
    );
    record(
        native_compile_host_trampoline(
            ExitCode::REF_FUNC,
            &Signature::new(SignatureId(3), vec![Type::I64, Type::I32], vec![Type::I64]),
        )
        .unwrap_or_default(),
        &mut shared.ref_func_offset,
    );
    record(
        native_compile_stack_grow_sequence().unwrap_or_default(),
        &mut shared.stack_grow_offset,
    );
    record(
        native_compile_host_trampoline(
            ExitCode::MEMORY_WAIT32,
            &Signature::new(
                SignatureId(4),
                vec![Type::I64, Type::I64, Type::I32, Type::I64],
                vec![Type::I32],
            ),
        )
        .unwrap_or_default(),
        &mut shared.memory_wait32_offset,
    );
    record(
        native_compile_host_trampoline(
            ExitCode::MEMORY_WAIT64,
            &Signature::new(
                SignatureId(5),
                vec![Type::I64, Type::I64, Type::I64, Type::I64],
                vec![Type::I32],
            ),
        )
        .unwrap_or_default(),
        &mut shared.memory_wait64_offset,
    );
    record(
        native_compile_host_trampoline(
            ExitCode::MEMORY_NOTIFY,
            &Signature::new(
                SignatureId(6),
                vec![Type::I64, Type::I32, Type::I64],
                vec![Type::I32],
            ),
        )
        .unwrap_or_default(),
        &mut shared.memory_notify_offset,
    );
    shared.executable = AlignedBytes::from_bytes(executable);
    shared
}

fn align16_vec(bytes: &mut Vec<u8>) {
    let rem = bytes.len() & 15;
    if rem != 0 {
        bytes.resize(bytes.len() + (16 - rem), 0);
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn native_compile_host_trampoline(
    exit_code: ExitCode,
    signature: &Signature,
) -> Result<Vec<u8>, EngineError> {
    let mut machine = NativeMachine::new();
    Ok(machine.compile_host_function_trampoline(exit_code, signature, true))
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn native_compile_host_trampoline(
    _exit_code: ExitCode,
    _signature: &Signature,
) -> Result<Vec<u8>, EngineError> {
    Err(EngineError::new("unsupported architecture"))
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn native_compile_stack_grow_sequence() -> Result<Vec<u8>, EngineError> {
    let mut machine = NativeMachine::new();
    Ok(machine.compile_stack_grow_call_sequence())
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn native_compile_stack_grow_sequence() -> Result<Vec<u8>, EngineError> {
    Err(EngineError::new("unsupported architecture"))
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn native_compile_entry_preamble(signature: &Signature) -> Result<Vec<u8>, EngineError> {
    let mut machine = NativeMachine::new();
    Ok(machine.compile_entry_preamble(signature, false))
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn native_compile_entry_preamble(_signature: &Signature) -> Result<Vec<u8>, EngineError> {
    Err(EngineError::new("unsupported architecture"))
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn native_resolve_relocations(
    ref_to_binary_offset: &[i32],
    imported_fns: usize,
    executable: &mut [u8],
    relocations: &[RelocationInfo],
    call_trampoline_island_offsets: &[i32],
    function_offset: usize,
) {
    if relocations.is_empty() {
        return;
    }
    let mut adjusted = relocations.to_vec();
    for relocation in &mut adjusted {
        relocation.offset += function_offset as i64;
    }
    let mut machine = NativeMachine::new();
    machine.resolve_relocations(
        ref_to_binary_offset,
        imported_fns,
        executable,
        &adjusted,
        call_trampoline_island_offsets,
    );
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn native_resolve_relocations(
    _ref_to_binary_offset: &[i32],
    _imported_fns: usize,
    _executable: &mut [u8],
    _relocations: &[RelocationInfo],
    _call_trampoline_island_offsets: &[i32],
    _function_offset: usize,
) {
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::engine_cache::InMemoryCompiledModuleCache;
    use razero_wasm::engine::Engine as _;
    use razero_wasm::host_func::stack_host_func;
    use razero_wasm::module::{Code, CodeBody, FunctionType, Module, ValueType};

    use super::{
        compile_shared_functions, AlignedBytes, CompiledModule, CompilerEngine, Executables,
        SharedFunctions, SourceMap,
    };

    fn function_type(params: &[ValueType], results: &[ValueType]) -> FunctionType {
        let mut ty = FunctionType::default();
        ty.params.extend_from_slice(params);
        ty.results.extend_from_slice(results);
        ty.cache_num_in_u64();
        ty
    }

    #[test]
    fn compile_module_caches_and_reuses_compilation() {
        let cache = Arc::new(InMemoryCompiledModuleCache::default());
        let mut engine = CompilerEngine::with_cache(Some(cache));
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x0b],
                ..Code::default()
            }],
            ..Module::default()
        };

        engine.compile_module(&module).unwrap();
        assert_eq!(engine.compiled_module_count(), 1);
        engine.compile_module(&module).unwrap();
        assert_eq!(engine.compiled_module_count(), 1);
    }

    #[test]
    fn host_module_compilation_emits_trampolines() {
        let mut engine = CompilerEngine::new();
        let module = Module {
            is_host_module: true,
            type_section: vec![function_type(&[ValueType::I64], &[ValueType::I64])],
            function_section: vec![0],
            code_section: vec![Code {
                body_kind: CodeBody::Host,
                host_func: Some(stack_host_func(|stack| {
                    stack[0] += 1;
                    Ok(())
                })),
                ..Code::default()
            }],
            ..Module::default()
        };
        engine.compile_module(&module).unwrap();
        let compiled = engine.compiled_module(&module).unwrap();
        assert!(!compiled.executables.executable.is_empty());
        assert_eq!(compiled.function_offsets[0] & 15, 0);
    }

    #[test]
    fn compiled_module_lookup_by_address_follows_sorted_ranges() {
        let mut engine = CompilerEngine::new();
        let make = |size: usize| {
            Arc::new(CompiledModule {
                executables: Executables {
                    executable: AlignedBytes::from_bytes(vec![0; size]),
                    ..Executables::default()
                },
                function_offsets: vec![0],
                module: Module::default(),
                offsets: ModuleContextOffsetData::default(),
                shared_functions: Arc::new(SharedFunctions::default()),
                ensure_termination: false,
                fuel_enabled: false,
                fuel: 0,
                memory_isolation_enabled: false,
                source_map: SourceMap::default(),
            })
        };
        let first = make(32);
        let second = make(32);
        engine.add_compiled_module_to_sorted_list(first.clone());
        engine.add_compiled_module_to_sorted_list(second.clone());

        use crate::wazevoapi::ModuleContextOffsetData;

        let target = if (first.executables.executable.as_ptr() as usize)
            < second.executables.executable.as_ptr() as usize
        {
            first.clone()
        } else {
            second.clone()
        };
        let addr = target.executables.executable.as_ptr() as usize + 8;
        assert!(Arc::ptr_eq(
            &engine.compiled_module_of_addr(addr).unwrap(),
            &target
        ));
    }

    #[test]
    fn shared_functions_are_16_byte_aligned() {
        let shared = compile_shared_functions();
        for ptr in [
            shared.memory_grow_ptr(),
            shared.check_module_exit_code_ptr(),
            shared.stack_grow_ptr(),
            shared.table_grow_ptr(),
            shared.ref_func_ptr(),
            shared.memory_wait32_ptr(),
            shared.memory_wait64_ptr(),
            shared.memory_notify_ptr(),
        ]
        .into_iter()
        .flatten()
        {
            assert_eq!(ptr & 15, 0);
        }
    }
}
