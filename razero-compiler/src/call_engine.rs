#![doc = "Compiler call-engine scaffolding."]

use std::fmt::{Display, Formatter};
use std::sync::Arc;

use razero_wasm::engine::FunctionHandle;
use razero_wasm::host_func::{Caller, HostFuncError, HostFuncRef};
use razero_wasm::module::Index;
use razero_wasm::module_instance::{ModuleCloseState, ModuleExitError, ModuleInstance};
use razero_wasm::table::Reference;
use razero_wasm::wasmruntime;

use crate::engine::CompiledModule;
use crate::hostmodule::host_module_host_func_from_opaque;
use crate::memmove::memmove_ptr;
use crate::wazevoapi::{go_function_index_from_exit_code, ExitCode};

pub const CALL_STACK_CEILING: usize = 50_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEngineError {
    InvalidParamCount {
        expected: usize,
        actual: usize,
    },
    Host(HostFuncError),
    ModuleExit(ModuleExitError),
    Runtime(wasmruntime::RuntimeError),
    UnsupportedExit {
        exit_code: ExitCode,
        detail: &'static str,
    },
}

impl Display for CallEngineError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidParamCount { expected, actual } => {
                write!(f, "expected {expected} params, but passed {actual}")
            }
            Self::Host(err) => Display::fmt(err, f),
            Self::ModuleExit(err) => Display::fmt(err, f),
            Self::Runtime(err) => Display::fmt(err, f),
            Self::UnsupportedExit { exit_code, detail } => write!(f, "{detail}: {exit_code}"),
        }
    }
}

impl std::error::Error for CallEngineError {}

impl From<HostFuncError> for CallEngineError {
    fn from(value: HostFuncError) -> Self {
        Self::Host(value)
    }
}

impl From<ModuleExitError> for CallEngineError {
    fn from(value: ModuleExitError) -> Self {
        Self::ModuleExit(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct ExecutionContext {
    pub exit_code: ExitCode,
    pub caller_module_context_ptr: usize,
    pub original_frame_pointer: usize,
    pub original_stack_pointer: usize,
    pub go_return_address: usize,
    pub stack_bottom_ptr: usize,
    pub go_call_return_address: usize,
    pub stack_pointer_before_go_call: usize,
    pub stack_grow_required_size: usize,
    pub memory_grow_trampoline_address: usize,
    pub stack_grow_call_trampoline_address: usize,
    pub check_module_exit_code_trampoline_address: usize,
    pub saved_registers: [[u64; 2]; 64],
    pub go_function_call_callee_module_context_opaque: usize,
    pub table_grow_trampoline_address: usize,
    pub ref_func_trampoline_address: usize,
    pub memmove_address: usize,
    pub frame_pointer_before_go_call: usize,
    pub memory_wait32_trampoline_address: usize,
    pub memory_wait64_trampoline_address: usize,
    pub memory_notify_trampoline_address: usize,
    pub fuel: i64,
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self {
            exit_code: ExitCode::OK,
            caller_module_context_ptr: 0,
            original_frame_pointer: 0,
            original_stack_pointer: 0,
            go_return_address: 0,
            stack_bottom_ptr: 0,
            go_call_return_address: 0,
            stack_pointer_before_go_call: 0,
            stack_grow_required_size: 0,
            memory_grow_trampoline_address: 0,
            stack_grow_call_trampoline_address: 0,
            check_module_exit_code_trampoline_address: 0,
            saved_registers: [[0; 2]; 64],
            go_function_call_callee_module_context_opaque: 0,
            table_grow_trampoline_address: 0,
            ref_func_trampoline_address: 0,
            memmove_address: 0,
            frame_pointer_before_go_call: 0,
            memory_wait32_trampoline_address: 0,
            memory_wait64_trampoline_address: 0,
            memory_notify_trampoline_address: 0,
            fuel: 0,
        }
    }
}

#[derive(Clone)]
pub struct CallEngine {
    index_in_module: Index,
    executable_ptr: usize,
    preamble_executable_ptr: usize,
    module_context_ptr: usize,
    pub stack: Vec<u8>,
    pub stack_top: usize,
    pub exec_ctx: ExecutionContext,
    size_of_param_result_slice: usize,
    required_params: usize,
    number_of_results: usize,
    host_func: Option<HostFuncRef>,
    _compiled_module: Option<Arc<CompiledModule>>,
    module_close_state: Option<ModuleCloseState>,
}

impl std::fmt::Debug for CallEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallEngine")
            .field("index_in_module", &self.index_in_module)
            .field("executable_ptr", &self.executable_ptr)
            .field("preamble_executable_ptr", &self.preamble_executable_ptr)
            .field("module_context_ptr", &self.module_context_ptr)
            .field("stack_len", &self.stack.len())
            .field("stack_top", &self.stack_top)
            .field("required_params", &self.required_params)
            .field("number_of_results", &self.number_of_results)
            .finish()
    }
}

impl CallEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        index_in_module: Index,
        executable_ptr: usize,
        preamble_executable_ptr: usize,
        module_context_ptr: usize,
        size_of_param_result_slice: usize,
        required_params: usize,
        number_of_results: usize,
        host_func: Option<HostFuncRef>,
        compiled_module: Option<Arc<CompiledModule>>,
        module_close_state: Option<ModuleCloseState>,
    ) -> Self {
        let mut call_engine = Self {
            index_in_module,
            executable_ptr,
            preamble_executable_ptr,
            module_context_ptr,
            stack: Vec::new(),
            stack_top: 0,
            exec_ctx: ExecutionContext {
                memmove_address: memmove_ptr(),
                ..ExecutionContext::default()
            },
            size_of_param_result_slice,
            required_params,
            number_of_results,
            host_func,
            _compiled_module: compiled_module,
            module_close_state,
        };
        call_engine.init();
        call_engine
    }

    pub fn executable_ptr(&self) -> usize {
        self.executable_ptr
    }

    pub fn preamble_executable_ptr(&self) -> usize {
        self.preamble_executable_ptr
    }

    pub fn module_context_ptr(&self) -> usize {
        self.module_context_ptr
    }

    pub fn exec_ctx_ptr(&self) -> usize {
        &self.exec_ctx as *const ExecutionContext as usize
    }

    pub fn required_initial_stack_size(&self) -> usize {
        const DEFAULT: usize = 10_240;
        let required = self.size_of_param_result_slice * 8 * 2 + 32 + 16;
        DEFAULT.max(required)
    }

    pub fn init(&mut self) {
        let stack_size = self.required_initial_stack_size();
        self.stack = vec![0; stack_size];
        self.stack_top = aligned_stack_top(&self.stack);
        self.exec_ctx.stack_bottom_ptr = self.stack.as_ptr() as usize;
    }

    pub fn grow_stack(&mut self) -> Result<(usize, usize), wasmruntime::RuntimeError> {
        let current_len = self.stack.len();
        if current_len > CALL_STACK_CEILING {
            return Err(wasmruntime::ERR_RUNTIME_STACK_OVERFLOW);
        }
        let new_len = current_len * 2 + self.exec_ctx.stack_grow_required_size + 16;
        let (new_sp, new_fp, new_top, new_stack) = self.clone_stack(new_len);
        self.stack = new_stack;
        self.stack_top = new_top;
        self.exec_ctx.stack_bottom_ptr = self.stack.as_ptr() as usize;
        Ok((new_sp, new_fp))
    }

    pub fn clone_stack(&self, len: usize) -> (usize, usize, usize, Vec<u8>) {
        let mut new_stack = vec![0; len];
        let old_bottom = self.stack.as_ptr() as usize;
        let old_sp = self.exec_ctx.stack_pointer_before_go_call;
        let old_fp = self.exec_ctx.frame_pointer_before_go_call;
        let rel_sp = self.stack_top.saturating_sub(old_sp);
        let rel_fp = self.stack_top.saturating_sub(old_fp);
        let new_top = aligned_stack_top(&new_stack);
        let new_sp = new_top.saturating_sub(rel_sp);
        let new_fp = new_top.saturating_sub(rel_fp);

        let copy_len = rel_sp.min(self.stack_top.saturating_sub(old_bottom));
        if copy_len > 0 {
            let start = old_sp.saturating_sub(old_bottom);
            let end = start + copy_len;
            let target = new_sp.saturating_sub(new_stack.as_ptr() as usize);
            new_stack[target..target + copy_len].copy_from_slice(&self.stack[start..end]);
        }
        (new_sp, new_fp, new_top, new_stack)
    }

    pub fn call<'a>(&mut self, stack: &'a mut [u64]) -> Result<&'a [u64], CallEngineError> {
        if self.required_params > stack.len() {
            return Err(CallEngineError::InvalidParamCount {
                expected: self.required_params,
                actual: stack.len(),
            });
        }

        self.exec_ctx.caller_module_context_ptr = 0;

        if self.uses_compiled_entrypoint() {
            self.invoke_compiled(stack)?;
        } else if self.exec_ctx.exit_code == ExitCode::OK {
            self.exec_ctx.exit_code = self.initial_exit_code();
        }

        self.run_exit_code_loop(stack)
    }

    pub fn run_exit_code_loop<'a>(
        &mut self,
        stack: &'a mut [u64],
    ) -> Result<&'a [u64], CallEngineError> {
        loop {
            match self.base_exit_code() {
                ExitCode::OK => return Ok(&stack[..self.number_of_results]),
                ExitCode::GROW_STACK => {
                    self.grow_stack().map_err(CallEngineError::Runtime)?;
                    self.exec_ctx.exit_code = ExitCode::OK;
                }
                ExitCode::CALL_GO_FUNCTION
                | ExitCode::CALL_GO_MODULE_FUNCTION
                | ExitCode::CALL_GO_FUNCTION_WITH_LISTENER
                | ExitCode::CALL_GO_MODULE_FUNCTION_WITH_LISTENER => {
                    if self.exec_ctx.stack_pointer_before_go_call != 0
                        && self.exec_ctx.go_call_return_address != 0
                    {
                        self.invoke_compiled_host_import()?;
                    } else {
                        self.invoke_host(stack)?;
                        self.exec_ctx.exit_code = ExitCode::OK;
                    }
                }
                ExitCode::CHECK_MODULE_EXIT_CODE => {
                    self.check_module_exit_code()?;
                    self.exec_ctx.exit_code = ExitCode::OK;
                    if self.exec_ctx.stack_pointer_before_go_call != 0
                        && self.exec_ctx.go_call_return_address != 0
                    {
                        #[cfg(target_arch = "x86_64")]
                        crate::entrypoint_amd64::after_go_function_call_entrypoint(
                            self.exec_ctx.go_call_return_address as *const u8,
                            self.exec_ctx_ptr(),
                            self.exec_ctx.stack_pointer_before_go_call,
                            self.exec_ctx.frame_pointer_before_go_call,
                        );
                        #[cfg(target_arch = "aarch64")]
                        crate::entrypoint_arm64::after_go_function_call_entrypoint(
                            self.exec_ctx.go_call_return_address as *const u8,
                            self.exec_ctx_ptr(),
                            self.exec_ctx.stack_pointer_before_go_call,
                            self.exec_ctx.frame_pointer_before_go_call,
                        );
                    }
                }
                ExitCode::UNREACHABLE => {
                    return Err(CallEngineError::Runtime(wasmruntime::UNREACHABLE));
                }
                ExitCode::MEMORY_OUT_OF_BOUNDS => {
                    return Err(CallEngineError::Runtime(
                        wasmruntime::OUT_OF_BOUNDS_MEMORY_ACCESS,
                    ));
                }
                ExitCode::MEMORY_FAULT => {
                    return Err(CallEngineError::Runtime(wasmruntime::MEMORY_FAULT));
                }
                ExitCode::TABLE_OUT_OF_BOUNDS | ExitCode::INDIRECT_CALL_NULL_POINTER => {
                    return Err(CallEngineError::Runtime(wasmruntime::INVALID_TABLE_ACCESS));
                }
                ExitCode::INDIRECT_CALL_TYPE_MISMATCH => {
                    return Err(CallEngineError::Runtime(
                        wasmruntime::INDIRECT_CALL_TYPE_MISMATCH,
                    ));
                }
                ExitCode::INTEGER_DIVISION_BY_ZERO => {
                    return Err(CallEngineError::Runtime(
                        wasmruntime::INTEGER_DIVIDE_BY_ZERO,
                    ));
                }
                ExitCode::INTEGER_OVERFLOW => {
                    return Err(CallEngineError::Runtime(wasmruntime::INTEGER_OVERFLOW));
                }
                ExitCode::INVALID_CONVERSION_TO_INTEGER => {
                    return Err(CallEngineError::Runtime(
                        wasmruntime::INVALID_CONVERSION_TO_INTEGER,
                    ));
                }
                ExitCode::UNALIGNED_ATOMIC => {
                    return Err(CallEngineError::Runtime(wasmruntime::UNALIGNED_ATOMIC));
                }
                ExitCode::FUEL_EXHAUSTED => {
                    return Err(CallEngineError::Runtime(wasmruntime::FUEL_EXHAUSTED));
                }
                ExitCode::POLICY_DENIED => {
                    return Err(CallEngineError::Runtime(wasmruntime::POLICY_DENIED));
                }
                ExitCode::GROW_MEMORY => {
                    self.grow_memory(stack)?;
                    self.exec_ctx.exit_code = ExitCode::OK;
                }
                ExitCode::TABLE_GROW => {
                    self.table_grow(stack)?;
                    self.exec_ctx.exit_code = ExitCode::OK;
                }
                ExitCode::REF_FUNC => {
                    self.ref_func(stack)?;
                    self.exec_ctx.exit_code = ExitCode::OK;
                }
                ExitCode::MEMORY_WAIT32 | ExitCode::MEMORY_WAIT64 => {
                    return Err(self.unsupported_exit("memory.wait requires runtime integration"));
                }
                ExitCode::MEMORY_NOTIFY => {
                    return Err(self.unsupported_exit("memory.notify requires runtime integration"));
                }
                ExitCode::CALL_LISTENER_BEFORE | ExitCode::CALL_LISTENER_AFTER => {
                    return Err(
                        self.unsupported_exit("listener trampoline requires runtime integration")
                    );
                }
                other => {
                    return Err(CallEngineError::UnsupportedExit {
                        exit_code: other,
                        detail: "unhandled compiler exit code",
                    });
                }
            }
        }
    }

    pub fn invoke_host<'a>(&mut self, stack: &'a mut [u64]) -> Result<&'a [u64], HostFuncError> {
        if self.required_params > stack.len() {
            return Err(HostFuncError::new(format!(
                "need {} params, but stack size is {}",
                self.required_params,
                stack.len()
            )));
        }
        let host = self
            .host_func
            .clone()
            .ok_or_else(|| HostFuncError::new("host function not configured"))?;
        let size_of_param_result_slice = self.size_of_param_result_slice;
        let mut caller = Caller::new(self.caller_module_instance_mut().map(|module| module as _));
        host.call(&mut caller, &mut stack[..size_of_param_result_slice])?;
        Ok(&stack[..self.number_of_results])
    }

    fn invoke_compiled_host_import(&mut self) -> Result<(), CallEngineError> {
        let host = unsafe {
            host_module_host_func_from_opaque(
                go_function_index_from_exit_code(self.exec_ctx.exit_code),
                self.exec_ctx.go_function_call_callee_module_context_opaque,
            )
        };
        let go_call_stack_words = unsafe {
            std::slice::from_raw_parts_mut(
                self.exec_ctx.stack_pointer_before_go_call as *mut u64,
                1,
            )
        };
        let go_call_stack = crate::isa_amd64::go_call_stack_view(go_call_stack_words);
        let go_call_stack = unsafe {
            std::slice::from_raw_parts_mut(go_call_stack.as_ptr() as *mut u64, go_call_stack.len())
        };
        let mut caller = Caller::new(self.caller_module_instance_mut().map(|module| module as _));
        host.call(&mut caller, go_call_stack)?;
        self.exec_ctx.exit_code = ExitCode::OK;

        #[cfg(target_arch = "x86_64")]
        {
            crate::entrypoint_amd64::after_go_function_call_entrypoint(
                self.exec_ctx.go_call_return_address as *const u8,
                self.exec_ctx_ptr(),
                self.exec_ctx.stack_pointer_before_go_call,
                self.exec_ctx.frame_pointer_before_go_call,
            );
            Ok(())
        }
        #[cfg(target_arch = "aarch64")]
        {
            crate::entrypoint_arm64::after_go_function_call_entrypoint(
                self.exec_ctx.go_call_return_address as *const u8,
                self.exec_ctx_ptr(),
                self.exec_ctx.stack_pointer_before_go_call,
                self.exec_ctx.frame_pointer_before_go_call,
            );
            Ok(())
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Err(CallEngineError::UnsupportedExit {
                exit_code: self.exec_ctx.exit_code,
                detail: "compiled host-call re-entry is not supported on this architecture",
            })
        }
    }

    fn initial_exit_code(&self) -> ExitCode {
        if self.host_func.is_some() {
            ExitCode::CALL_GO_FUNCTION
        } else {
            ExitCode::OK
        }
    }

    fn uses_compiled_entrypoint(&self) -> bool {
        self.preamble_executable_ptr != 0
    }

    fn invoke_compiled(&mut self, stack: &mut [u64]) -> Result<(), CallEngineError> {
        if self.executable_ptr == 0 {
            return Err(CallEngineError::UnsupportedExit {
                exit_code: ExitCode::OK,
                detail: "compiled function executable is not configured",
            });
        }
        let module_context_ptr = self.module_context_ptr as *const u8;
        let param_result_ptr = stack.as_mut_ptr();
        #[cfg(target_arch = "x86_64")]
        {
            crate::entrypoint_amd64::entrypoint(
                self.preamble_executable_ptr as *const u8,
                self.executable_ptr as *const u8,
                self.exec_ctx_ptr(),
                module_context_ptr,
                param_result_ptr,
                self.stack_top,
            );
            Ok(())
        }
        #[cfg(target_arch = "aarch64")]
        {
            crate::entrypoint_arm64::entrypoint(
                self.preamble_executable_ptr as *const u8,
                self.executable_ptr as *const u8,
                self.exec_ctx_ptr(),
                module_context_ptr,
                param_result_ptr,
                self.stack_top,
            );
            Ok(())
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            let _ = (module_context_ptr, param_result_ptr);
            Err(CallEngineError::UnsupportedExit {
                exit_code: ExitCode::OK,
                detail: "compiled function entrypoint is not supported on this architecture",
            })
        }
    }

    fn base_exit_code(&self) -> ExitCode {
        ExitCode::new(self.exec_ctx.exit_code.raw() & 0xff)
    }

    fn caller_module_instance_mut(&mut self) -> Option<&mut ModuleInstance> {
        let module_context_ptr = self.exec_ctx.caller_module_context_ptr;
        if module_context_ptr == 0 {
            return None;
        }
        let module_ptr = unsafe {
            std::ptr::read_unaligned(module_context_ptr as *const usize) as *mut ModuleInstance
        };
        (!module_ptr.is_null()).then(|| unsafe { &mut *module_ptr })
    }

    fn caller_module_instance(&self) -> Option<&ModuleInstance> {
        let module_context_ptr = self.exec_ctx.caller_module_context_ptr;
        if module_context_ptr == 0 {
            return None;
        }
        let module_ptr = unsafe {
            std::ptr::read_unaligned(module_context_ptr as *const usize) as *const ModuleInstance
        };
        (!module_ptr.is_null()).then(|| unsafe { &*module_ptr })
    }

    fn active_module_instance_mut(&mut self) -> Option<&mut ModuleInstance> {
        self.caller_module_instance_mut()
    }

    fn active_module_instance(&self) -> Option<&ModuleInstance> {
        self.caller_module_instance()
    }

    fn module_opaque_mut(&mut self) -> Option<&mut [u8]> {
        let compiled = self._compiled_module.as_ref()?;
        if self.module_context_ptr == 0 || compiled.offsets.total_size == 0 {
            return None;
        }
        Some(unsafe {
            std::slice::from_raw_parts_mut(
                self.module_context_ptr as *mut u8,
                compiled.offsets.total_size,
            )
        })
    }

    fn grow_memory(&mut self, stack: &mut [u64]) -> Result<(), CallEngineError> {
        let Some(arg) = stack.first_mut() else {
            return Err(CallEngineError::InvalidParamCount {
                expected: 1,
                actual: 0,
            });
        };

        let result = {
            let Some(module) = self.active_module_instance_mut() else {
                return Err(self.unsupported_exit("memory.grow requires a module context"));
            };
            let Some(memory) = module.memory_instance.as_mut() else {
                *arg = u64::from(u32::MAX);
                return Ok(());
            };
            let delta = u32::try_from(*arg).unwrap_or(u32::MAX);
            memory.grow(delta).unwrap_or(u32::MAX)
        };
        *arg = u64::from(result);
        self.refresh_local_memory_definition();
        Ok(())
    }

    fn refresh_local_memory_definition(&mut self) {
        let local_memory_offset = match self
            ._compiled_module
            .as_ref()
            .map(|compiled| compiled.offsets.local_memory_begin.raw())
        {
            Some(offset) if offset >= 0 => offset as usize,
            _ => return,
        };
        let (ptr, len) = match self.active_module_instance() {
            Some(module) => match module.memory_instance.as_ref() {
                Some(memory) => (
                    memory
                        .bytes
                        .first()
                        .map_or(0usize, |byte| byte as *const u8 as usize),
                    memory.bytes.len(),
                ),
                None => (0, 0),
            },
            None => return,
        };
        let Some(opaque) = self.module_opaque_mut() else {
            return;
        };
        opaque[local_memory_offset..local_memory_offset + 8]
            .copy_from_slice(&(ptr as u64).to_le_bytes());
        opaque[local_memory_offset + 8..local_memory_offset + 16]
            .copy_from_slice(&(len as u64).to_le_bytes());
    }

    fn table_grow(&mut self, stack: &mut [u64]) -> Result<(), CallEngineError> {
        if stack.len() < 3 {
            return Err(CallEngineError::InvalidParamCount {
                expected: 3,
                actual: stack.len(),
            });
        }
        let Some(module) = self.active_module_instance_mut() else {
            return Err(self.unsupported_exit("table.grow requires a module context"));
        };
        let table_index = usize::try_from(stack[0]).unwrap_or(usize::MAX);
        let delta = u32::try_from(stack[1]).unwrap_or(u32::MAX);
        let initial_ref = raw_reference(stack[2]);
        let Some(table) = module.tables.get_mut(table_index) else {
            return Err(CallEngineError::Runtime(wasmruntime::INVALID_TABLE_ACCESS));
        };
        stack[0] = u64::from(table.grow(delta, initial_ref));
        Ok(())
    }

    fn ref_func(&mut self, stack: &mut [u64]) -> Result<(), CallEngineError> {
        let Some(slot) = stack.first_mut() else {
            return Err(CallEngineError::InvalidParamCount {
                expected: 1,
                actual: 0,
            });
        };
        *slot = u64::from((*slot).min(u64::from(u32::MAX)) as u32);
        Ok(())
    }

    fn check_module_exit_code(&self) -> Result<(), CallEngineError> {
        match self
            .module_close_state
            .as_ref()
            .and_then(ModuleCloseState::exit_code)
        {
            Some(exit_code) => Err(CallEngineError::from(ModuleExitError { exit_code })),
            None => Ok(()),
        }
    }

    fn unsupported_exit(&self, detail: &'static str) -> CallEngineError {
        CallEngineError::UnsupportedExit {
            exit_code: self.base_exit_code(),
            detail,
        }
    }
}

fn raw_reference(raw: u64) -> Reference {
    if raw == u64::MAX {
        None
    } else {
        Some(raw)
    }
}

impl FunctionHandle for CallEngine {
    fn index(&self) -> Index {
        self.index_in_module
    }
}

pub fn aligned_stack_top(stack: &[u8]) -> usize {
    if stack.is_empty() {
        return 0;
    }
    let stack_addr = stack.as_ptr() as usize + stack.len() - 1;
    stack_addr - (stack_addr & 15)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::aot::AotCompiledMetadata;
    use razero_wasm::host_func::stack_host_func;
    use razero_wasm::memory::MemoryInstance;
    use razero_wasm::module::{Memory, Module, Table};
    use razero_wasm::module_instance::ModuleInstance;
    use razero_wasm::table::TableInstance;
    use razero_wasm::wasmruntime;

    use super::{aligned_stack_top, CallEngine, CallEngineError, CALL_STACK_CEILING};
    use crate::engine::{CompiledModule, Executables, SharedFunctions, SourceMap};
    use crate::module_engine::CompilerModuleEngine;
    use crate::wazevoapi::ExitCode;
    use crate::wazevoapi::ModuleContextOffsetData;

    #[cfg(target_arch = "x86_64")]
    core::arch::global_asm!(
        r#"
        .text
        .global razero_test_amd64_call_engine_preamble
        .type razero_test_amd64_call_engine_preamble, @function
    razero_test_amd64_call_engine_preamble:
        movq (%r12), %rax
        addq 8(%r12), %rax
        movq %rax, (%r12)
        ret

        .global razero_test_amd64_unused_function
        .type razero_test_amd64_unused_function, @function
    razero_test_amd64_unused_function:
        ret
    "#,
        options(att_syntax)
    );

    #[cfg(target_arch = "x86_64")]
    unsafe extern "C" {
        fn razero_test_amd64_call_engine_preamble();
        fn razero_test_amd64_unused_function();
    }

    #[test]
    fn aligned_stack_top_is_16_byte_aligned() {
        let stack = vec![0u8; 64];
        assert_eq!(aligned_stack_top(&stack) & 15, 0);
    }

    #[test]
    fn call_engine_init_sets_bottom_pointer() {
        let call_engine = CallEngine::new(0, 0, 0, 0, 0, 0, 0, None, None, None);
        assert_eq!(call_engine.stack_top & 15, 0);
        assert_eq!(
            call_engine.exec_ctx.stack_bottom_ptr,
            call_engine.stack.as_ptr() as usize
        );
    }

    #[test]
    fn grow_stack_copies_active_window() {
        let mut call_engine = CallEngine::new(0, 0, 0, 0, 0, 0, 0, None, None, None);
        call_engine.stack = (0..32).collect();
        call_engine.stack_top = call_engine.stack.as_ptr() as usize + 15;
        call_engine.exec_ctx.stack_grow_required_size = 160;
        call_engine.exec_ctx.stack_pointer_before_go_call =
            call_engine.stack.as_ptr() as usize + 10;
        call_engine.exec_ctx.frame_pointer_before_go_call =
            call_engine.stack.as_ptr() as usize + 14;

        let (new_sp, new_fp) = call_engine.grow_stack().unwrap();
        assert_eq!(call_engine.stack.len(), 160 + 32 * 2 + 16);
        assert_eq!(
            &call_engine.stack[(new_sp - call_engine.stack.as_ptr() as usize)..][..5],
            &[10, 11, 12, 13, 14]
        );
        assert_eq!(new_fp - new_sp, 4);
    }

    #[test]
    fn grow_stack_rejects_ceiling_overflow() {
        let mut call_engine = CallEngine::new(0, 0, 0, 0, 0, 0, 0, None, None, None);
        call_engine.stack = vec![0; CALL_STACK_CEILING + 1];
        assert!(call_engine.grow_stack().is_err());
    }

    #[test]
    fn invoke_host_uses_stored_host_function() {
        let host = stack_host_func(|stack| {
            stack[0] += stack[1];
            Ok(())
        });
        let mut call_engine = CallEngine::new(3, 0, 0, 0, 2, 2, 1, Some(host), None, None);
        let mut stack = [20, 22];
        let results = call_engine.invoke_host(&mut stack).unwrap();
        assert_eq!(results, &[42]);
    }

    #[test]
    fn call_runs_host_exit_code_loop_until_ok() {
        let host = stack_host_func(|stack| {
            stack[0] = stack[0].wrapping_mul(2);
            Ok(())
        });
        let mut call_engine = CallEngine::new(3, 0, 0, 0, 1, 1, 1, Some(host), None, None);
        let mut stack = [21];
        let results = call_engine.call(&mut stack).unwrap();
        assert_eq!(results, &[42]);
        assert_eq!(call_engine.exec_ctx.exit_code, ExitCode::OK);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn call_executes_compiled_entrypoint_on_amd64() {
        let mut call_engine = CallEngine::new(
            0,
            razero_test_amd64_unused_function as *const () as usize,
            razero_test_amd64_call_engine_preamble as *const () as usize,
            0,
            2,
            2,
            1,
            None,
            None,
            None,
        );
        let mut stack = [20, 22];
        let results = call_engine.call(&mut stack).unwrap();
        assert_eq!(results, &[42]);
        assert_eq!(call_engine.exec_ctx.exit_code, ExitCode::OK);
    }

    #[test]
    fn exit_code_loop_grows_stack_before_returning() {
        let mut call_engine = CallEngine::new(0, 0, 0, 0, 0, 0, 0, None, None, None);
        call_engine.stack = vec![0; 32];
        call_engine.stack_top = call_engine.stack.as_ptr() as usize + 15;
        call_engine.exec_ctx.stack_pointer_before_go_call = call_engine.stack.as_ptr() as usize + 8;
        call_engine.exec_ctx.frame_pointer_before_go_call =
            call_engine.stack.as_ptr() as usize + 12;
        call_engine.exec_ctx.stack_grow_required_size = 64;
        call_engine.exec_ctx.exit_code = ExitCode::GROW_STACK;

        let results = call_engine.run_exit_code_loop(&mut []).unwrap();
        assert!(results.is_empty());
        assert!(call_engine.stack.len() > 32);
    }

    #[test]
    fn exit_code_loop_maps_traps_to_runtime_errors() {
        let mut call_engine = CallEngine::new(0, 0, 0, 0, 0, 0, 0, None, None, None);
        call_engine.exec_ctx.exit_code = ExitCode::FUEL_EXHAUSTED;
        let err = call_engine.run_exit_code_loop(&mut []).unwrap_err();
        assert_eq!(err, CallEngineError::Runtime(wasmruntime::FUEL_EXHAUSTED));

        call_engine.exec_ctx.exit_code = ExitCode::MEMORY_OUT_OF_BOUNDS;
        let err = call_engine.run_exit_code_loop(&mut []).unwrap_err();
        assert_eq!(
            err,
            CallEngineError::Runtime(wasmruntime::OUT_OF_BOUNDS_MEMORY_ACCESS)
        );

        call_engine.exec_ctx.exit_code = ExitCode::MEMORY_FAULT;
        let err = call_engine.run_exit_code_loop(&mut []).unwrap_err();
        assert_eq!(err, CallEngineError::Runtime(wasmruntime::MEMORY_FAULT));
    }

    #[test]
    fn unsupported_runtime_builtins_remain_explicit() {
        let mut call_engine = CallEngine::new(0, 0, 0, 0, 0, 0, 0, None, None, None);
        call_engine.exec_ctx.exit_code = ExitCode::MEMORY_WAIT32;
        let err = call_engine.run_exit_code_loop(&mut []).unwrap_err();
        match err {
            CallEngineError::UnsupportedExit { exit_code, .. } => {
                assert_eq!(exit_code, ExitCode::MEMORY_WAIT32);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn compiled_module_for(module: &Module) -> Arc<CompiledModule> {
        Arc::new(CompiledModule {
            executables: Executables::default(),
            function_offsets: vec![0],
            module: module.clone(),
            offsets: ModuleContextOffsetData::new(module, false),
            aot: AotCompiledMetadata::default(),
            shared_functions: Arc::new(SharedFunctions::default()),
            ensure_termination: false,
            fuel_enabled: false,
            fuel: 0,
            memory_isolation_enabled: false,
            source_map: SourceMap::default(),
        })
    }

    fn runtime_call_engine_for(module: Module, mut instance: ModuleInstance) -> CallEngine {
        instance.source = module.clone();
        let parent = compiled_module_for(&module);
        let mut module_engine = Box::new(CompilerModuleEngine::new(parent.clone(), instance));
        module_engine.init_opaque();
        let module_engine = Box::leak(module_engine);
        CallEngine::new(
            0,
            0,
            0,
            module_engine.opaque_ptr(),
            3,
            0,
            1,
            None,
            Some(parent),
            Some(module_engine.module().closed.clone()),
        )
    }

    #[test]
    fn exit_code_loop_grows_memory_and_refreshes_opaque() {
        let module = Module {
            memory_section: Some(Memory {
                min: 1,
                cap: 3,
                max: 3,
                ..Memory::default()
            }),
            ..Module::default()
        };
        let instance = ModuleInstance {
            memory_instance: Some(MemoryInstance::new(module.memory_section.as_ref().unwrap())),
            ..ModuleInstance::default()
        };
        let mut call_engine = runtime_call_engine_for(module, instance);
        let total_size = call_engine
            ._compiled_module
            .as_ref()
            .unwrap()
            .offsets
            .total_size;
        let local_memory_offset = call_engine
            ._compiled_module
            .as_ref()
            .unwrap()
            .offsets
            .local_memory_begin
            .raw() as usize;
        let opaque_before = unsafe {
            std::slice::from_raw_parts(call_engine.module_context_ptr() as *const u8, total_size)
        };
        let before_len = u64::from_le_bytes(
            opaque_before[local_memory_offset + 8..local_memory_offset + 16]
                .try_into()
                .unwrap(),
        );
        assert_eq!(before_len, u64::from(65_536u32));

        let mut stack = [1u64];
        call_engine.exec_ctx.caller_module_context_ptr = call_engine.module_context_ptr();
        call_engine.exec_ctx.exit_code = ExitCode::GROW_MEMORY;
        let results = call_engine.run_exit_code_loop(&mut stack).unwrap();
        assert_eq!(results, &[1]);
        assert_eq!(
            call_engine
                .active_module_instance()
                .unwrap()
                .memory_instance
                .as_ref()
                .unwrap()
                .pages(),
            2
        );
        let opaque_after = unsafe {
            std::slice::from_raw_parts(call_engine.module_context_ptr() as *const u8, total_size)
        };
        let after_len = u64::from_le_bytes(
            opaque_after[local_memory_offset + 8..local_memory_offset + 16]
                .try_into()
                .unwrap(),
        );
        assert_eq!(after_len, u64::from(131_072u32));
    }

    #[test]
    fn exit_code_loop_handles_table_grow_and_ref_func() {
        let module = Module {
            table_section: vec![Table::default()],
            ..Module::default()
        };
        let instance = ModuleInstance {
            tables: vec![TableInstance::new(&Table::default())],
            ..ModuleInstance::default()
        };
        let mut call_engine = runtime_call_engine_for(module, instance);

        let mut table_stack = [0u64, 2, 7];
        call_engine.exec_ctx.caller_module_context_ptr = call_engine.module_context_ptr();
        call_engine.exec_ctx.exit_code = ExitCode::TABLE_GROW;
        let results = call_engine.run_exit_code_loop(&mut table_stack).unwrap();
        assert_eq!(results, &[0]);
        assert_eq!(
            call_engine.active_module_instance().unwrap().tables[0].elements(),
            vec![Some(7), Some(7)]
        );

        let mut ref_stack = [5u64];
        call_engine.exec_ctx.caller_module_context_ptr = call_engine.module_context_ptr();
        call_engine.exec_ctx.exit_code = ExitCode::REF_FUNC;
        let results = call_engine.run_exit_code_loop(&mut ref_stack).unwrap();
        assert_eq!(results, &[5]);
    }

    #[test]
    fn exit_code_loop_reports_closed_module() {
        let module = Module::default();
        let mut instance = ModuleInstance::default();
        instance.close_with_exit_code(7);
        let mut call_engine = runtime_call_engine_for(module, instance);
        call_engine.exec_ctx.exit_code = ExitCode::CHECK_MODULE_EXIT_CODE;
        let err = call_engine.run_exit_code_loop(&mut []).unwrap_err();
        match err {
            CallEngineError::ModuleExit(err) => assert_eq!(err.exit_code, 7),
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
