#![doc = "Interpreter engine glue for razero-wasm stores."]

use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::ptr::NonNull;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, TryLockError};

use razero_wasm::engine::{
    Engine as WasmEngine, EngineError, FunctionHandle, FunctionTypeId,
    ModuleEngine as WasmModuleEngine,
};
use razero_wasm::host_func::{Caller, HostFuncRef as WasmHostFuncRef};
use razero_wasm::module::{
    FunctionType as WasmFunctionType, GlobalType as WasmGlobalType, ImportDesc, Index, Module,
    ModuleId, ValueType as WasmValueType,
};
use razero_wasm::module_instance::ModuleInstance;
use razero_wasm::store_module_list::ModuleInstanceId;
use razero_wasm::table::{
    decode_function_reference, encode_function_reference, Reference, TableInstance,
};

use crate::compiler::{
    CompileConfig, Compiler, FunctionType as InterpFunctionType, GlobalType as InterpGlobalType,
    ValueType as InterpValueType,
};
use crate::interpreter::{
    host_function, is_yield_suspend_payload, Function, HostFuncRef, Interpreter,
    InterpreterSuspend, Memory, Module as RuntimeModule, RuntimeResult, SuspendedCall, Table, Trap,
};
use crate::signature::Signature;

thread_local! {
    static ACTIVE_CALLER_MODULES: RefCell<Vec<NonNull<RuntimeModule>>> = const { RefCell::new(Vec::new()) };
    static PENDING_SUSPENDED_INVOCATIONS: RefCell<Vec<Arc<dyn SuspendedInvocation>>> = const { RefCell::new(Vec::new()) };
}

static MODULE_RUNTIMES: LazyLock<Mutex<HashMap<ModuleInstanceId, Arc<ModuleRuntime>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
type GuestCallback = Arc<dyn Fn(&[u64]) -> Result<Vec<u64>, String> + Send + Sync>;
type RegisteredGuestCallback = (Signature, GuestCallback);
static GUEST_CALLBACKS: LazyLock<Mutex<HashMap<(ModuleInstanceId, u32), RegisteredGuestCallback>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn register_guest_callback(
    module_id: ModuleInstanceId,
    function_index: u32,
    signature: Signature,
    callback: GuestCallback,
) {
    lock_or_poison(&GUEST_CALLBACKS).insert((module_id, function_index), (signature, callback));
}

fn lookup_guest_callback(
    module_id: ModuleInstanceId,
    function_index: u32,
) -> Option<RegisteredGuestCallback> {
    lock_or_poison(&GUEST_CALLBACKS)
        .get(&(module_id, function_index))
        .cloned()
}

fn with_active_caller_module<T>(module: &mut RuntimeModule, f: impl FnOnce() -> T) -> T {
    ACTIVE_CALLER_MODULES.with(|active| active.borrow_mut().push(NonNull::from(module)));
    let result = f();
    ACTIVE_CALLER_MODULES.with(|active| {
        active.borrow_mut().pop();
    });
    result
}

fn with_caller_module<T>(
    default: &mut RuntimeModule,
    f: impl FnOnce(&mut RuntimeModule) -> T,
) -> T {
    ACTIVE_CALLER_MODULES.with(|active| {
        let binding = active.borrow();
        if let Some(module) = binding.last() {
            unsafe { f(&mut *module.as_ptr()) }
        } else {
            f(default)
        }
    })
}

pub trait SuspendedInvocation: Send + Sync {
    fn resume(&self, host_results: &[u64]) -> RuntimeResult<Vec<u64>>;
    fn cancel(&self);
    fn expected_host_result_count(&self) -> Option<usize>;
}

pub fn take_suspended_invocation() -> Option<Arc<dyn SuspendedInvocation>> {
    PENDING_SUSPENDED_INVOCATIONS.with(|pending| pending.borrow_mut().pop())
}

fn push_suspended_invocation(invocation: Arc<dyn SuspendedInvocation>) {
    PENDING_SUSPENDED_INVOCATIONS.with(|pending| pending.borrow_mut().push(invocation));
}

#[derive(Clone, Debug)]
pub struct CompiledModule {
    source: Module,
    types: Vec<Signature>,
    local_functions: Vec<Function>,
}

#[derive(Clone, Debug)]
struct CompiledModuleWithCount {
    compiled_module: Arc<CompiledModule>,
    ref_count: usize,
}

#[derive(Debug)]
enum ModuleRuntimeState {
    Empty,
    Ready(RuntimeModule),
    Suspended(SuspendedExecution),
}

impl Default for ModuleRuntimeState {
    fn default() -> Self {
        Self::Empty
    }
}

impl ModuleRuntimeState {
    fn module(&self) -> &RuntimeModule {
        match self {
            Self::Ready(module) => module,
            Self::Suspended(suspended) => &suspended.interpreter.module,
            Self::Empty => panic!("module runtime entered an invalid state"),
        }
    }

    fn module_mut(&mut self) -> &mut RuntimeModule {
        match self {
            Self::Ready(module) => module,
            Self::Suspended(suspended) => &mut suspended.interpreter.module,
            Self::Empty => panic!("module runtime entered an invalid state"),
        }
    }

    fn take_module(&mut self) -> RuntimeModule {
        match std::mem::replace(self, Self::Empty) {
            Self::Ready(module) => module,
            Self::Suspended(suspended) => suspended.interpreter.module,
            Self::Empty => panic!("module runtime entered an invalid state"),
        }
    }

    fn suspend_id(&self) -> Option<u64> {
        match self {
            Self::Suspended(suspended) => Some(suspended.id),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct SuspendedExecution {
    id: u64,
    interpreter: Interpreter,
    call: SuspendedCall,
}

enum ModuleRuntimeAction {
    Call {
        function_index: usize,
        params: Vec<u64>,
    },
    Resume {
        suspend_id: u64,
        host_results: Vec<u64>,
    },
}

struct RuntimeSuspendedInvocation {
    runtime: Arc<ModuleRuntime>,
    suspend_id: u64,
}

impl SuspendedInvocation for RuntimeSuspendedInvocation {
    fn resume(&self, host_results: &[u64]) -> RuntimeResult<Vec<u64>> {
        self.runtime.resume(self.suspend_id, host_results)
    }

    fn cancel(&self) {
        self.runtime.cancel(self.suspend_id);
    }

    fn expected_host_result_count(&self) -> Option<usize> {
        self.runtime.expected_host_result_count(self.suspend_id)
    }
}

#[derive(Debug, Default)]
struct ModuleRuntime {
    state: Mutex<ModuleRuntimeState>,
    call_stack_ceiling: usize,
}

impl ModuleRuntime {
    fn new(module: RuntimeModule, call_stack_ceiling: usize) -> Self {
        Self {
            state: Mutex::new(ModuleRuntimeState::Ready(module)),
            call_stack_ceiling,
        }
    }

    fn call(self: &Arc<Self>, function_index: usize, params: &[u64]) -> RuntimeResult<Vec<u64>> {
        let params = params.to_vec();
        self.invoke(ModuleRuntimeAction::Call {
            function_index,
            params,
        })
    }

    fn resume(self: &Arc<Self>, suspend_id: u64, host_results: &[u64]) -> RuntimeResult<Vec<u64>> {
        self.invoke(ModuleRuntimeAction::Resume {
            suspend_id,
            host_results: host_results.to_vec(),
        })
    }

    fn cancel(&self, suspend_id: u64) {
        let mut state = lock_or_poison(&self.state);
        if state.suspend_id() == Some(suspend_id) {
            *state = ModuleRuntimeState::Ready(state.take_module());
        }
    }

    fn expected_host_result_count(&self, suspend_id: u64) -> Option<usize> {
        let state = lock_or_poison(&self.state);
        match &*state {
            ModuleRuntimeState::Suspended(suspended) if suspended.id == suspend_id => suspended
                .interpreter
                .expected_host_result_count(&suspended.call)
                .ok(),
            _ => None,
        }
    }

    fn call_from_import(
        self: &Arc<Self>,
        function_index: usize,
        params: &[u64],
    ) -> RuntimeResult<Vec<u64>> {
        let mut state = match self.state.try_lock() {
            Ok(module) => module,
            Err(TryLockError::WouldBlock) => {
                return Err(Trap::new("reentrant imported call not supported"));
            }
            Err(TryLockError::Poisoned(err)) => err.into_inner(),
        };
        if !matches!(*state, ModuleRuntimeState::Ready(_)) {
            return Err(Trap::new("module execution already suspended"));
        }
        let runtime_state = std::mem::replace(&mut *state, ModuleRuntimeState::Empty);
        drop(state);
        let (next_state, result) = match runtime_state {
            ModuleRuntimeState::Ready(module) => {
                self.invoke_ready_without_suspension(module, function_index, params)
            }
            _ => unreachable!("ready state already checked"),
        };
        *lock_or_poison(&self.state) = next_state;
        result
    }

    fn snapshot(&self) -> RuntimeModule {
        lock_or_poison(&self.state).module().clone()
    }

    fn refresh_ready_module(&self, module: RuntimeModule) {
        let mut state = lock_or_poison(&self.state);
        if matches!(*state, ModuleRuntimeState::Ready(_)) {
            *state = ModuleRuntimeState::Ready(module);
        }
    }

    fn replace_function(&self, index: usize, function: Function) {
        if let Some(slot) = lock_or_poison(&self.state)
            .module_mut()
            .functions
            .get_mut(index)
        {
            *slot = function;
        }
    }

    fn invoke(self: &Arc<Self>, action: ModuleRuntimeAction) -> RuntimeResult<Vec<u64>> {
        let mut state = lock_or_poison(&self.state);
        let runtime_state = std::mem::replace(&mut *state, ModuleRuntimeState::Empty);
        drop(state);

        let (next_state, result) = match (action, runtime_state) {
            (
                ModuleRuntimeAction::Call {
                    function_index,
                    params,
                },
                ModuleRuntimeState::Ready(module),
            ) => self.invoke_ready(module, function_index, &params),
            (
                ModuleRuntimeAction::Resume {
                    suspend_id,
                    host_results,
                },
                ModuleRuntimeState::Suspended(suspended),
            ) => {
                if suspended.id != suspend_id {
                    (
                        ModuleRuntimeState::Suspended(suspended),
                        Err(Trap::new("suspended execution is no longer resumable")),
                    )
                } else {
                    self.invoke_resumed(suspended, &host_results)
                }
            }
            (ModuleRuntimeAction::Call { .. }, ModuleRuntimeState::Suspended(suspended)) => (
                ModuleRuntimeState::Suspended(suspended),
                Err(Trap::new("module execution already suspended")),
            ),
            (ModuleRuntimeAction::Resume { .. }, ModuleRuntimeState::Ready(module)) => (
                ModuleRuntimeState::Ready(module),
                Err(Trap::new("suspended execution is no longer resumable")),
            ),
            (_, ModuleRuntimeState::Empty) => (
                ModuleRuntimeState::Empty,
                Err(Trap::new("module runtime entered an invalid state")),
            ),
        };

        *lock_or_poison(&self.state) = next_state;
        result
    }

    fn invoke_ready(
        self: &Arc<Self>,
        module: RuntimeModule,
        function_index: usize,
        params: &[u64],
    ) -> (ModuleRuntimeState, RuntimeResult<Vec<u64>>) {
        let mut interpreter = Interpreter::new(module);
        interpreter.call_stack_ceiling = self.call_stack_ceiling;
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            interpreter.call(function_index, params)
        }));
        self.handle_outcome(interpreter, outcome, true)
    }

    fn invoke_ready_without_suspension(
        self: &Arc<Self>,
        module: RuntimeModule,
        function_index: usize,
        params: &[u64],
    ) -> (ModuleRuntimeState, RuntimeResult<Vec<u64>>) {
        let mut interpreter = Interpreter::new(module);
        interpreter.call_stack_ceiling = self.call_stack_ceiling;
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            interpreter.call(function_index, params)
        }));
        self.handle_outcome(interpreter, outcome, false)
    }

    fn invoke_resumed(
        self: &Arc<Self>,
        suspended: SuspendedExecution,
        host_results: &[u64],
    ) -> (ModuleRuntimeState, RuntimeResult<Vec<u64>>) {
        let mut interpreter = suspended.interpreter;
        interpreter.call_stack_ceiling = self.call_stack_ceiling;
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            interpreter.resume(suspended.call, host_results)
        }));
        self.handle_outcome(interpreter, outcome, true)
    }

    fn handle_outcome(
        self: &Arc<Self>,
        interpreter: Interpreter,
        outcome: std::thread::Result<RuntimeResult<Vec<u64>>>,
        capture_suspension: bool,
    ) -> (ModuleRuntimeState, RuntimeResult<Vec<u64>>) {
        match outcome {
            Ok(result) => (ModuleRuntimeState::Ready(interpreter.module), result),
            Err(payload) => match payload.downcast::<InterpreterSuspend>() {
                Ok(suspend) => {
                    let InterpreterSuspend {
                        payload,
                        suspended_call,
                    } = *suspend;
                    if is_yield_suspend_payload(payload.as_ref()) {
                        if !capture_suspension {
                            *lock_or_poison(&self.state) =
                                ModuleRuntimeState::Ready(interpreter.module);
                            panic::resume_unwind(payload);
                        }
                        let suspended = SuspendedExecution {
                            id: self.next_suspend_id(),
                            interpreter,
                            call: suspended_call,
                        };
                        let invocation: Arc<dyn SuspendedInvocation> =
                            Arc::new(RuntimeSuspendedInvocation {
                                runtime: self.clone(),
                                suspend_id: suspended.id,
                            });
                        push_suspended_invocation(invocation);
                        let state = ModuleRuntimeState::Suspended(suspended);
                        *lock_or_poison(&self.state) = state;
                        panic::resume_unwind(payload);
                    } else {
                        *lock_or_poison(&self.state) =
                            ModuleRuntimeState::Ready(interpreter.module);
                        panic::resume_unwind(payload);
                    }
                }
                Err(payload) => {
                    *lock_or_poison(&self.state) = ModuleRuntimeState::Ready(interpreter.module);
                    panic::resume_unwind(payload);
                }
            },
        }
    }

    fn next_suspend_id(&self) -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_SUSPEND_ID: AtomicU64 = AtomicU64::new(1);
        NEXT_SUSPEND_ID.fetch_add(1, Ordering::SeqCst)
    }
}

fn lock_or_poison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(err) => err.into_inner(),
    }
}

fn lookup_module_runtime(id: ModuleInstanceId) -> Option<Arc<ModuleRuntime>> {
    lock_or_poison(&MODULE_RUNTIMES).get(&id).cloned()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InterpFunctionHandle {
    index: Index,
}

impl InterpFunctionHandle {
    fn new(index: Index) -> Self {
        Self { index }
    }
}

impl FunctionHandle for InterpFunctionHandle {
    fn index(&self) -> Index {
        self.index
    }
}

#[derive(Debug, Clone)]
pub struct InterpModuleEngine {
    parent: Arc<CompiledModule>,
    instance: ModuleInstance,
    runtime: Arc<ModuleRuntime>,
}

impl InterpModuleEngine {
    pub fn new(
        parent: Arc<CompiledModule>,
        instance: ModuleInstance,
        call_stack_ceiling: usize,
    ) -> Result<Self, EngineError> {
        let module = build_runtime_module(&parent, &instance)?;
        let runtime = Arc::new(ModuleRuntime::new(module, call_stack_ceiling));
        lock_or_poison(&MODULE_RUNTIMES).insert(instance.id, runtime.clone());
        Ok(Self {
            parent,
            instance,
            runtime,
        })
    }

    pub fn call(&self, function_index: Index, params: &[u64]) -> RuntimeResult<Vec<u64>> {
        self.runtime.refresh_ready_module(self.snapshot());
        let result = self.runtime.call(function_index as usize, params);
        let runtime_snapshot = self.runtime.snapshot();
        self.sync_instance_globals_from_runtime(&runtime_snapshot);
        self.sync_instance_tables_from_runtime(&runtime_snapshot);
        result
    }

    pub fn snapshot(&self) -> RuntimeModule {
        let mut module = self.runtime.snapshot();
        module.functions.truncate(self.instance.functions.len());
        let mut wrappers = HashMap::<(ModuleInstanceId, u32), usize>::new();
        for ((runtime_global, instance_global), global_type) in module
            .globals
            .iter_mut()
            .zip(&self.instance.globals)
            .zip(&self.instance.global_types)
        {
            let (lo, hi) = instance_global.value();
            runtime_global.lo = if global_type.val_type == WasmValueType::FUNCREF && lo != 0 {
                runtime_funcref_slot(lo, &self.instance, &mut module.functions, &mut wrappers)
                    as u64
                    + 1
            } else {
                lo
            };
            runtime_global.hi = hi;
            runtime_global.is_vector = instance_global.ty.val_type == WasmValueType::V128;
        }
        module.tables = self
            .instance
            .tables
            .iter()
            .map(|table| self.refresh_table(table, &mut module.functions))
            .collect();
        module
    }

    fn sync_instance_globals_from_runtime(&self, module: &RuntimeModule) {
        for ((runtime_global, instance_global), global_type) in module
            .globals
            .iter()
            .zip(&self.instance.globals)
            .zip(&self.instance.global_types)
        {
            let mut instance_global = instance_global.clone();
            let lo = if global_type.val_type == WasmValueType::FUNCREF && runtime_global.lo != 0 {
                store_funcref_reference(runtime_global.lo - 1, &self.instance, module)
                    .unwrap_or_default()
            } else {
                runtime_global.lo
            };
            instance_global.set_value(lo, runtime_global.hi);
        }
    }

    pub fn memory_size(&self) -> Option<u32> {
        let state = lock_or_poison(&self.runtime.state);
        let len = state.module().memory.as_ref()?.bytes().len() as u32;
        Some(len)
    }

    fn refresh_table(&self, table: &TableInstance, functions: &mut Vec<Function>) -> Table {
        let mut wrappers = HashMap::<(ModuleInstanceId, u32), usize>::new();
        let elements = table
            .elements()
            .into_iter()
            .map(|reference| {
                reference.and_then(|reference| match table.ty {
                    razero_wasm::module::RefType::FUNCREF => Some(runtime_funcref_slot(
                        reference,
                        &self.instance,
                        functions,
                        &mut wrappers,
                    ) as u64),
                    razero_wasm::module::RefType::EXTERNREF => Some(reference),
                    _ => Some(reference),
                })
            })
            .collect();
        Table::from_elements_typed(elements, table.max, table.ty)
    }

    fn sync_instance_tables_from_runtime(&self, runtime_module: &RuntimeModule) {
        for ((runtime_table, instance_table), table_type) in runtime_module
            .tables
            .iter()
            .zip(&self.instance.tables)
            .zip(&self.instance.table_types)
        {
            let elements = runtime_table
                .elements()
                .into_iter()
                .map(|reference| match table_type.ty {
                    razero_wasm::module::RefType::FUNCREF => reference.and_then(|index| {
                        if let Some(function) = self.instance.functions.get(index as usize) {
                            return Some(encode_function_reference(
                                function.module_id,
                                function.function_index,
                            ));
                        }
                        runtime_module
                            .functions
                            .get(index as usize)
                            .and_then(|function| function.table_reference)
                    }),
                    razero_wasm::module::RefType::EXTERNREF => reference,
                    _ => reference,
                })
                .collect::<Vec<_>>();
            let shared = instance_table.shared_elements();
            let mut table_elements = shared.write().expect("table write lock");
            table_elements.clear();
            table_elements.extend(elements);
        }
    }

    pub fn memory_read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        let state = lock_or_poison(&self.runtime.state);
        let memory = state.module().memory.as_ref()?;
        let end = offset.checked_add(len)?;
        let bytes = memory.bytes();
        bytes.get(offset..end).map(ToOwned::to_owned)
    }

    pub fn memory_write(&self, offset: usize, values: &[u8]) -> bool {
        let mut state = lock_or_poison(&self.runtime.state);
        let Some(memory) = state.module_mut().memory.as_mut() else {
            return false;
        };
        let Some(end) = offset.checked_add(values.len()) else {
            return false;
        };
        let mut bytes = memory.bytes_mut();
        let Some(slice) = bytes.get_mut(offset..end) else {
            return false;
        };
        slice.copy_from_slice(values);
        true
    }

    pub fn memory_write_u32(&self, offset: Index, value: u32) -> bool {
        let mut state = lock_or_poison(&self.runtime.state);
        let Some(memory) = state.module_mut().memory.as_mut() else {
            return false;
        };
        memory.write_u32_le(offset, value)
    }

    pub fn memory_grow(&self, delta_pages: u32, maximum_pages: Option<u32>) -> Option<u32> {
        let mut state = lock_or_poison(&self.runtime.state);
        let memory = state.module_mut().memory.as_mut()?;
        if let Some(maximum_pages) = maximum_pages {
            memory.max_pages = Some(memory.max_pages.unwrap_or(maximum_pages).min(maximum_pages));
        }
        memory.grow(delta_pages)
    }

    pub fn global_value(&self, index: Index) -> Option<(u64, u64, WasmValueType)> {
        let state = lock_or_poison(&self.runtime.state);
        let global = state.module().globals.get(index as usize)?;
        let ty = self.instance.global_types.get(index as usize)?.val_type;
        Some((global.lo, global.hi, ty))
    }

    fn replace_function(&self, index: usize, function: Function) {
        self.runtime.replace_function(index, function);
    }

    fn signature_for_function(&self, index: Index) -> Result<Signature, EngineError> {
        let ty = self
            .parent
            .source
            .type_of_function(index)
            .ok_or_else(|| EngineError::new(format!("function[{index}] type is undefined")))?;
        Ok(signature_from_wasm_function_type(ty)?)
    }

    fn resolve_imported_module_engine<'a>(
        imported_module_engine: &'a dyn WasmModuleEngine,
    ) -> &'a InterpModuleEngine {
        unsafe {
            &*(imported_module_engine as *const dyn WasmModuleEngine as *const InterpModuleEngine)
        }
    }
}

impl WasmModuleEngine for InterpModuleEngine {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn new_function(&self, index: Index) -> Box<dyn FunctionHandle> {
        Box::new(InterpFunctionHandle::new(index))
    }

    fn resolve_imported_function(
        &mut self,
        index: Index,
        _desc_func: Index,
        index_in_imported_module: Index,
        imported_module_engine: &dyn WasmModuleEngine,
    ) {
        let imported = Self::resolve_imported_module_engine(imported_module_engine);
        let Ok(signature) = self.signature_for_function(index) else {
            return;
        };
        let runtime = imported.runtime.clone();
        let function = Function::new_host(
            signature.clone(),
            imported_host_function(signature, move |params| {
                runtime.call_from_import(index_in_imported_module as usize, params)
            }),
        );
        self.replace_function(index as usize, function);
    }

    fn resolve_imported_memory(&mut self, imported_module_engine: &dyn WasmModuleEngine) {
        let imported = Self::resolve_imported_module_engine(imported_module_engine);
        let shared_memory = self.runtime.snapshot().memory;
        if let Some(memory) = shared_memory.clone() {
            lock_or_poison(&imported.runtime.state).module_mut().memory = Some(memory.clone());
            lock_or_poison(&self.runtime.state).module_mut().memory = Some(memory);
        }
    }

    fn memory_snapshot(&self) -> Option<(Vec<u8>, Option<u32>, bool)> {
        self.runtime
            .snapshot()
            .memory
            .map(|memory| (memory.bytes().to_vec(), memory.max_pages, memory.shared))
    }

    fn overwrite_memory(&mut self, bytes: &[u8], maximum_pages: Option<u32>, shared: bool) -> bool {
        let mut state = lock_or_poison(&self.runtime.state);
        let Some(memory) = state.module_mut().memory.as_mut() else {
            return false;
        };
        memory.max_pages = maximum_pages;
        memory.shared = shared;
        memory.overwrite(bytes);
        true
    }

    fn lookup_function(
        &self,
        table: &TableInstance,
        type_id: FunctionTypeId,
        table_offset: Index,
    ) -> Option<(&ModuleInstance, Index)> {
        let reference = table.get(table_offset as usize).flatten()?;
        let (module_id, function_index) = decode_function_reference(reference);
        if module_id != self.instance.id {
            return None;
        }
        let function = self.instance.functions.get(function_index as usize)?;
        (function.type_id == type_id).then_some((&self.instance, function_index))
    }

    fn get_global_value(&self, index: Index) -> (u64, u64) {
        self.instance
            .globals
            .get(index as usize)
            .map(|global| global.value())
            .unwrap_or((0, 0))
    }

    fn set_global_value(&mut self, index: Index, lo: u64, hi: u64) {
        if let Some(global) = lock_or_poison(&self.runtime.state)
            .module_mut()
            .globals
            .get_mut(index as usize)
        {
            global.lo = lo;
            global.hi = hi;
            global.is_vector = self
                .instance
                .global_types
                .get(index as usize)
                .is_some_and(|ty| ty.val_type == WasmValueType::V128);
        }
        if let Some(global) = self.instance.globals.get_mut(index as usize) {
            global.set_value(lo, hi);
        }
    }

    fn owns_globals(&self) -> bool {
        true
    }

    fn function_instance_reference(&self, func_index: Index) -> Reference {
        Some(encode_function_reference(self.instance.id, func_index))
    }
}

#[derive(Debug, Default)]
pub struct InterpEngine {
    compiled_modules: HashMap<ModuleId, CompiledModuleWithCount>,
    call_stack_ceiling: usize,
}

impl InterpEngine {
    pub fn new() -> Self {
        Self {
            compiled_modules: HashMap::new(),
            call_stack_ceiling: crate::interpreter::DEFAULT_CALL_STACK_CEILING,
        }
    }

    pub fn compiled_module(&self, module: &Module) -> Option<Arc<CompiledModule>> {
        self.compiled_modules
            .get(&module.id)
            .map(|entry| entry.compiled_module.clone())
    }

    fn compile_module_impl(&self, module: &Module) -> Result<CompiledModule, EngineError> {
        let types = module
            .type_section
            .iter()
            .map(signature_from_wasm_function_type)
            .collect::<Result<Vec<_>, _>>()?;
        let globals = visible_global_types(module)?;
        let function_type_indices = module_function_type_indices(module)?;
        let interp_types = module
            .type_section
            .iter()
            .map(interp_function_type_from_wasm)
            .collect::<Result<Vec<_>, _>>()?;

        let local_functions = module
            .function_section
            .iter()
            .copied()
            .enumerate()
            .map(|(local_index, type_index)| {
                let signature = types.get(type_index as usize).cloned().ok_or_else(|| {
                    EngineError::new(format!("function type[{type_index}] out of range"))
                })?;
                let code = module
                    .code_section
                    .get(local_index)
                    .ok_or_else(|| EngineError::new(format!("code[{local_index}] missing")))?;
                if code.is_host_function() {
                    return host_function_from_code(code, signature);
                }

                let function_type =
                    interp_types
                        .get(type_index as usize)
                        .cloned()
                        .ok_or_else(|| {
                            EngineError::new(format!("function type[{type_index}] out of range"))
                        })?;
                let local_types = wasm_value_types_to_interp(&code.local_types)?;
                let lowered = Compiler
                    .lower_with_config(CompileConfig {
                        body: &code.body,
                        signature: function_type,
                        local_types: &local_types,
                        globals: &globals,
                        functions: &function_type_indices,
                        types: &interp_types,
                        call_frame_stack_size_in_u64: 0,
                        ensure_termination: module.ensure_termination,
                    })
                    .map_err(|err| {
                        EngineError::new(format!("local[{local_index}] failed: {err}"))
                    })?;
                Function::new_native(signature, lowered.operations)
                    .map_err(|err| EngineError::new(err.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(CompiledModule {
            source: module.clone(),
            types,
            local_functions,
        })
    }
}

fn module_function_type_indices(module: &Module) -> Result<Vec<Index>, EngineError> {
    let mut functions =
        Vec::with_capacity(module.import_function_count as usize + module.function_section.len());
    for import in &module.import_section {
        if let ImportDesc::Func(type_index) = import.desc {
            functions.push(type_index);
        }
    }
    functions.extend(module.function_section.iter().copied());
    if functions.len() != module.import_function_count as usize + module.function_section.len() {
        return Err(EngineError::new("function import count mismatch"));
    }
    Ok(functions)
}

impl WasmEngine for InterpEngine {
    fn close(&mut self) -> Result<(), EngineError> {
        self.compiled_modules.clear();
        Ok(())
    }

    fn compile_module(&mut self, module: &Module) -> Result<(), EngineError> {
        if let Some(existing) = self.compiled_modules.get_mut(&module.id) {
            existing.ref_count += 1;
            return Ok(());
        }
        let compiled = Arc::new(self.compile_module_impl(module)?);
        self.compiled_modules.insert(
            module.id,
            CompiledModuleWithCount {
                compiled_module: compiled,
                ref_count: 1,
            },
        );
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
        Ok(Box::new(InterpModuleEngine::new(
            compiled,
            instance.clone(),
            self.call_stack_ceiling,
        )?))
    }
}

fn runtime_funcref_slot(
    reference: u64,
    instance: &ModuleInstance,
    functions: &mut Vec<Function>,
    wrappers: &mut HashMap<(ModuleInstanceId, u32), usize>,
) -> usize {
    let (module_id, function_index) = decode_function_reference(reference);
    if let Some(local_index) = instance.functions.iter().position(|function| {
        function.module_id == module_id && function.function_index == function_index
    }) {
        return local_index;
    }
    if let Some(local_index) = instance
        .functions
        .iter()
        .position(|function| function.is_host && function.function_index == function_index)
    {
        return local_index;
    }
    *wrappers.entry((module_id, function_index)).or_insert_with(|| {
        let (signature, callback) = if let Some(callback) = lookup_guest_callback(module_id, function_index) {
            callback
        } else if let Some(runtime) = lookup_module_runtime(module_id) {
            let target = runtime.snapshot();
            (
                target.functions[function_index as usize].signature.clone(),
                Arc::new(move |params: &[u64]| {
                    runtime
                        .call_from_import(function_index as usize, params)
                        .map_err(|err| err.to_string())
                }) as GuestCallback,
            )
        } else {
            panic!("foreign function callback missing for module {module_id} function {function_index}")
        };
        let wrapper = Function::new_host_with_table_reference(
            signature.clone(),
            imported_host_function(signature, move |params| callback(params).map_err(Trap::new)),
            reference,
        );
        let slot = functions.len();
        functions.push(wrapper);
        slot
    })
}

fn store_funcref_reference(
    slot: u64,
    instance: &ModuleInstance,
    runtime_module: &RuntimeModule,
) -> Option<u64> {
    if let Some(function) = instance.functions.get(slot as usize) {
        return Some(encode_function_reference(
            function.module_id,
            function.function_index,
        ));
    }
    runtime_module
        .functions
        .get(slot as usize)
        .and_then(|function| function.table_reference)
}

fn build_runtime_module(
    parent: &CompiledModule,
    instance: &ModuleInstance,
) -> Result<RuntimeModule, EngineError> {
    let import_function_count = instance.source.import_function_count as usize;
    let mut functions = Vec::with_capacity(instance.functions.len());
    for index in 0..instance.functions.len() {
        if index < import_function_count {
            let signature = signature_from_wasm_function_type(
                instance
                    .source
                    .type_of_function(index as u32)
                    .ok_or_else(|| {
                        EngineError::new(format!("function[{index}] type is undefined"))
                    })?,
            )?;
            functions.push(unresolved_import_function(index as u32, signature));
        } else {
            let local_index = index - import_function_count;
            functions.push(
                parent
                    .local_functions
                    .get(local_index)
                    .cloned()
                    .ok_or_else(|| {
                        EngineError::new(format!("function[{index}] missing compiled body"))
                    })?,
            );
        }
    }
    let mut wrappers = HashMap::<(ModuleInstanceId, u32), usize>::new();
    let globals = instance
        .globals
        .iter()
        .zip(instance.global_types.iter())
        .map(|(global, global_type)| {
            let (lo, hi) = global.value();
            crate::interpreter::GlobalValue {
                lo: if global_type.val_type == WasmValueType::FUNCREF && lo != 0 {
                    runtime_funcref_slot(lo, instance, &mut functions, &mut wrappers) as u64 + 1
                } else {
                    lo
                },
                hi,
                is_vector: global.ty.val_type == WasmValueType::V128,
            }
        })
        .collect();
    let tables = instance
        .tables
        .iter()
        .zip(instance.table_types.iter())
        .map(|(table, table_type)| {
            Table::from_elements_typed(
                table
                    .elements()
                    .into_iter()
                    .map(|reference| match table_type.ty {
                        razero_wasm::module::RefType::FUNCREF => reference.map(|reference| {
                            runtime_funcref_slot(reference, instance, &mut functions, &mut wrappers)
                                as u64
                        }),
                        razero_wasm::module::RefType::EXTERNREF => reference,
                        _ => reference,
                    })
                    .collect(),
                table_type.max,
                table_type.ty,
            )
        })
        .collect();
    let element_instances = instance
        .element_instances
        .iter()
        .zip(instance.source.element_section.iter())
        .map(|(element_instance, segment)| {
            Some(
                element_instance
                    .iter()
                    .map(|reference| match segment.ty {
                        razero_wasm::module::RefType::FUNCREF => reference.map(|reference| {
                            runtime_funcref_slot(reference, instance, &mut functions, &mut wrappers)
                                as u64
                        }),
                        razero_wasm::module::RefType::EXTERNREF => *reference,
                        _ => *reference,
                    })
                    .collect(),
            )
        })
        .collect();

    Ok(RuntimeModule {
        functions,
        globals,
        memory: instance.memory_instance.as_ref().map(|memory| {
            Memory::from_bytes(
                memory.bytes.to_vec(),
                instance
                    .memory_type
                    .as_ref()
                    .and_then(|memory_type| memory_type.is_max_encoded.then_some(memory_type.max)),
                memory.shared,
            )
        }),
        tables,
        types: parent.types.clone(),
        data_instances: instance.data_instances.iter().cloned().map(Some).collect(),
        element_instances,
        closed: instance.closed.clone(),
    })
}

fn unresolved_import_function(index: Index, signature: Signature) -> Function {
    Function::new_host(
        signature,
        host_function(move |_, _| {
            Err(Trap::new(format!(
                "function[{index}] import was not resolved"
            )))
        }),
    )
}

fn imported_host_function<F>(signature: Signature, invoke: F) -> HostFuncRef
where
    F: Fn(&[u64]) -> RuntimeResult<Vec<u64>> + Send + Sync + 'static,
{
    host_function(move |module, stack| {
        with_active_caller_module(module, || {
            let results = invoke(&stack[..signature.param_slots])?;
            if results.len() != signature.result_slots {
                return Err(Trap::new(format!(
                    "expected {} results, but imported call returned {}",
                    signature.result_slots,
                    results.len()
                )));
            }
            stack[..signature.result_slots].copy_from_slice(&results);
            Ok(())
        })
    })
}

fn host_function_from_code(
    code: &razero_wasm::module::Code,
    signature: Signature,
) -> Result<Function, EngineError> {
    let host = code
        .host_func
        .clone()
        .ok_or_else(|| EngineError::new("host function body missing callback"))?;
    Ok(Function::new_host(signature, adapt_host_function(host)))
}

fn adapt_host_function(host: WasmHostFuncRef) -> HostFuncRef {
    host_function(move |module, stack| {
        with_caller_module(module, |caller_module| {
            let mut caller = Caller::with_data(None, Some(caller_module));
            host.call(&mut caller, stack)
                .map_err(|err| Trap::new(err.to_string()))
        })
    })
}

fn visible_global_types(module: &Module) -> Result<Vec<InterpGlobalType>, EngineError> {
    module
        .import_section
        .iter()
        .filter_map(|import| match &import.desc {
            ImportDesc::Global(global) => Some(global_type_from_wasm(*global)),
            _ => None,
        })
        .chain(
            module
                .global_section
                .iter()
                .map(|global| global_type_from_wasm(global.ty)),
        )
        .collect()
}

fn signature_from_wasm_function_type(ty: &WasmFunctionType) -> Result<Signature, EngineError> {
    Ok(Signature::new(
        wasm_value_types_to_interp(&ty.params)?,
        wasm_value_types_to_interp(&ty.results)?,
    ))
}

fn interp_function_type_from_wasm(
    ty: &WasmFunctionType,
) -> Result<InterpFunctionType, EngineError> {
    Ok(InterpFunctionType::new(
        wasm_value_types_to_interp(&ty.params)?,
        wasm_value_types_to_interp(&ty.results)?,
    ))
}

fn global_type_from_wasm(ty: WasmGlobalType) -> Result<InterpGlobalType, EngineError> {
    Ok(InterpGlobalType {
        value_type: interp_value_type_from_wasm(ty.val_type)?,
    })
}

fn wasm_value_types_to_interp(
    types: &[WasmValueType],
) -> Result<Vec<InterpValueType>, EngineError> {
    types
        .iter()
        .copied()
        .map(interp_value_type_from_wasm)
        .collect()
}

fn interp_value_type_from_wasm(value: WasmValueType) -> Result<InterpValueType, EngineError> {
    match value {
        WasmValueType::I32 => Ok(InterpValueType::I32),
        WasmValueType::I64 => Ok(InterpValueType::I64),
        WasmValueType::F32 => Ok(InterpValueType::F32),
        WasmValueType::F64 => Ok(InterpValueType::F64),
        WasmValueType::V128 => Ok(InterpValueType::V128),
        WasmValueType::FUNCREF => Ok(InterpValueType::FuncRef),
        WasmValueType::EXTERNREF => Ok(InterpValueType::ExternRef),
        _ => Err(EngineError::new(format!(
            "unsupported interpreter value type {}",
            value.name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use razero_decoder::decoder::decode_module;
    use razero_features::CoreFeatures;
    use razero_wasm::engine::Engine as _;
    use razero_wasm::host_func::stack_host_func;
    use razero_wasm::module::{
        Code, CodeBody, Export, ExternType, FunctionType, Import, Module, ValueType,
    };
    use razero_wasm::store::Store;

    use super::{
        interp_function_type_from_wasm, module_function_type_indices, visible_global_types,
        wasm_value_types_to_interp, InterpEngine, InterpModuleEngine,
    };
    use crate::compiler::{CompileConfig, Compiler};

    fn function_type(params: &[ValueType], results: &[ValueType]) -> FunctionType {
        let mut ty = FunctionType::default();
        ty.params.extend_from_slice(params);
        ty.results.extend_from_slice(results);
        ty.cache_num_in_u64();
        ty
    }

    fn fixture(path: &str) -> Vec<u8> {
        let mut full_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        full_path.push(path);
        std::fs::read(full_path).unwrap()
    }

    #[test]
    fn compile_module_caches_and_reuses_compilation() {
        let mut engine = InterpEngine::new();
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
        engine.compile_module(&module).unwrap();
        assert_eq!(1, engine.compiled_module_count());
        engine.delete_compiled_module(&module);
        assert_eq!(1, engine.compiled_module_count());
        engine.delete_compiled_module(&module);
        assert_eq!(0, engine.compiled_module_count());
    }

    #[test]
    fn store_instantiation_executes_defined_function() {
        let mut store = Store::new(InterpEngine::new());
        let module = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        let module_id = store.instantiate(module, "demo", None).unwrap();
        let engine = store
            .module_engine(module_id)
            .unwrap()
            .as_any()
            .downcast_ref::<InterpModuleEngine>()
            .unwrap();

        assert_eq!(vec![42], engine.call(0, &[41]).unwrap());
    }

    #[test]
    fn store_instantiation_resolves_imported_functions() {
        let mut store = Store::new(InterpEngine::new());
        let host = Module {
            is_host_module: true,
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            function_section: vec![0],
            code_section: vec![Code {
                body_kind: CodeBody::Host,
                host_func: Some(stack_host_func(|stack| {
                    stack[0] = stack[0].wrapping_add(1);
                    Ok(())
                })),
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "inc".to_string(),
                index: 0,
            }],
            ..Module::default()
        };
        store.instantiate(host, "env", None).unwrap();

        let consumer = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            import_section: vec![Import::function("env", "inc", 0)],
            import_function_count: 1,
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x10, 0x00, 0x0b],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 1,
            }],
            ..Module::default()
        };

        let module_id = store.instantiate(consumer, "consumer", None).unwrap();
        let engine = store
            .module_engine(module_id)
            .unwrap()
            .as_any()
            .downcast_ref::<InterpModuleEngine>()
            .unwrap();

        assert_eq!(vec![42], engine.call(1, &[41]).unwrap());
    }

    #[test]
    fn compile_module_allows_later_imported_function_indices() {
        let mut engine = InterpEngine::new();
        let consumer = Module {
            type_section: vec![function_type(&[ValueType::I32], &[ValueType::I32])],
            import_section: vec![
                Import::function("env", "inc", 0),
                Import::function("env", "add_two", 0),
            ],
            import_function_count: 2,
            function_section: vec![0],
            code_section: vec![Code {
                body: vec![0x20, 0x00, 0x10, 0x01, 0x0b],
                ..Code::default()
            }],
            ..Module::default()
        };

        engine.compile_module(&consumer).unwrap();
    }

    #[test]
    fn store_instantiation_preserves_i64_local_order() {
        let mut store = Store::new(InterpEngine::new());
        let module = Module {
            type_section: vec![function_type(&[], &[ValueType::I64])],
            function_section: vec![0],
            code_section: vec![Code {
                local_types: vec![ValueType::I64, ValueType::I64],
                body: vec![
                    0x42, 0x0e, 0x21, 0x00, // local0 = 14
                    0x42, 0x05, 0x21, 0x01, // local1 = 5
                    0x20, 0x00, // len
                    0x20, 0x01, // ptr
                    0x42, 0x20, // 32
                    0x86, // i64.shl
                    0x84, // i64.or
                    0x0b,
                ],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "pack".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        let module_id = store.instantiate(module, "demo", None).unwrap();
        let engine = store
            .module_engine(module_id)
            .unwrap()
            .as_any()
            .downcast_ref::<InterpModuleEngine>()
            .unwrap();

        assert_eq!(vec![(5_u64 << 32) | 14], engine.call(0, &[]).unwrap());
    }

    #[test]
    fn store_instantiation_advances_past_multibyte_block_type() {
        let mut store = Store::new(InterpEngine::new());
        let mut type_section = (0..128)
            .map(|_| function_type(&[], &[]))
            .collect::<Vec<_>>();
        type_section.push(function_type(&[], &[ValueType::I32]));

        let module = Module {
            type_section,
            function_section: vec![128],
            code_section: vec![Code {
                body: vec![
                    0x02, 0x80, 0x01, // block (type 128)
                    0x41, 0x07, // i32.const 7
                    0x0b, // end block
                    0x0b, // end function
                ],
                ..Code::default()
            }],
            export_section: vec![Export {
                ty: ExternType::FUNC,
                name: "run".to_string(),
                index: 0,
            }],
            ..Module::default()
        };

        let module_id = store.instantiate(module, "demo", None).unwrap();
        let engine = store
            .module_engine(module_id)
            .unwrap()
            .as_any()
            .downcast_ref::<InterpModuleEngine>()
            .unwrap();

        assert_eq!(vec![7], engine.call(0, &[]).unwrap());
    }

    #[test]
    fn compile_allocation_fixture_modules() {
        for path in [
            "../examples/allocation/rust/testdata/greet.wasm",
            "../examples/allocation/zig/testdata/greet.wasm",
        ] {
            let module = decode_module(&fixture(path), CoreFeatures::V2).unwrap();
            let globals = visible_global_types(&module).unwrap();
            let function_type_indices = module_function_type_indices(&module).unwrap();
            let interp_types = module
                .type_section
                .iter()
                .map(interp_function_type_from_wasm)
                .collect::<Result<Vec<_>, _>>()
                .unwrap();

            for (local_index, type_index) in module.function_section.iter().copied().enumerate() {
                let code = module.code_section.get(local_index).unwrap();
                if code.is_host_function() {
                    continue;
                }
                let function_type = interp_types.get(type_index as usize).cloned().unwrap();
                let local_types = wasm_value_types_to_interp(&code.local_types).unwrap();
                if let Err(err) = Compiler.lower_with_config(CompileConfig {
                    body: &code.body,
                    signature: function_type,
                    local_types: &local_types,
                    globals: &globals,
                    functions: &function_type_indices,
                    types: &interp_types,
                    call_frame_stack_size_in_u64: 0,
                    ensure_termination: false,
                }) {
                    panic!("{path} local[{local_index}] failed: {err}");
                }
            }
        }
    }
}
