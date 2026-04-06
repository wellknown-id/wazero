#![doc = "Compiler call-engine scaffolding."]

use std::fmt::{Display, Formatter};
use std::sync::Arc;

use razero_wasm::engine::FunctionHandle;
use razero_wasm::host_func::{Caller, HostFuncError, HostFuncRef};
use razero_wasm::module::Index;
use razero_wasm::wasmruntime;

use crate::engine::CompiledModule;
use crate::memmove::memmove_ptr;
use crate::wazevoapi::ExitCode;

pub const CALL_STACK_CEILING: usize = 50_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEngineError {
    InvalidParamCount {
        expected: usize,
        actual: usize,
    },
    Host(HostFuncError),
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

        if self.exec_ctx.exit_code == ExitCode::OK {
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
                    self.invoke_host(stack)?;
                    self.exec_ctx.exit_code = ExitCode::OK;
                }
                ExitCode::CHECK_MODULE_EXIT_CODE => {
                    self.exec_ctx.exit_code = ExitCode::OK;
                }
                ExitCode::UNREACHABLE => {
                    return Err(CallEngineError::Runtime(wasmruntime::UNREACHABLE));
                }
                ExitCode::MEMORY_OUT_OF_BOUNDS | ExitCode::MEMORY_FAULT => {
                    return Err(CallEngineError::Runtime(
                        wasmruntime::OUT_OF_BOUNDS_MEMORY_ACCESS,
                    ));
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
                    return Err(self.unsupported_exit("memory.grow requires runtime integration"));
                }
                ExitCode::TABLE_GROW => {
                    return Err(self.unsupported_exit("table.grow requires runtime integration"));
                }
                ExitCode::REF_FUNC => {
                    return Err(self.unsupported_exit("ref.func requires runtime integration"));
                }
                ExitCode::MEMORY_WAIT32 | ExitCode::MEMORY_WAIT64 => {
                    return Err(self.unsupported_exit("memory.wait requires runtime integration"));
                }
                ExitCode::MEMORY_NOTIFY => {
                    return Err(self.unsupported_exit("memory.notify requires runtime integration"));
                }
                ExitCode::CALL_LISTENER_BEFORE | ExitCode::CALL_LISTENER_AFTER => {
                    return Err(self.unsupported_exit("listener trampoline requires runtime integration"));
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

    pub fn invoke_host<'a>(&self, stack: &'a mut [u64]) -> Result<&'a [u64], HostFuncError> {
        if self.required_params > stack.len() {
            return Err(HostFuncError::new(format!(
                "need {} params, but stack size is {}",
                self.required_params,
                stack.len()
            )));
        }
        let host = self
            .host_func
            .as_ref()
            .ok_or_else(|| HostFuncError::new("host function not configured"))?;
        let mut caller = Caller::default();
        host.call(&mut caller, &mut stack[..self.size_of_param_result_slice])?;
        Ok(&stack[..self.number_of_results])
    }

    fn initial_exit_code(&self) -> ExitCode {
        if self.host_func.is_some() {
            ExitCode::CALL_GO_FUNCTION
        } else {
            ExitCode::OK
        }
    }

    fn base_exit_code(&self) -> ExitCode {
        ExitCode::new(self.exec_ctx.exit_code.raw() & 0xff)
    }

    fn unsupported_exit(&self, detail: &'static str) -> CallEngineError {
        CallEngineError::UnsupportedExit {
            exit_code: self.base_exit_code(),
            detail,
        }
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
    use razero_wasm::host_func::stack_host_func;
    use razero_wasm::wasmruntime;

    use super::{aligned_stack_top, CallEngine, CallEngineError, CALL_STACK_CEILING};
    use crate::wazevoapi::ExitCode;

    #[test]
    fn aligned_stack_top_is_16_byte_aligned() {
        let stack = vec![0u8; 64];
        assert_eq!(aligned_stack_top(&stack) & 15, 0);
    }

    #[test]
    fn call_engine_init_sets_bottom_pointer() {
        let call_engine = CallEngine::new(0, 0, 0, 0, 0, 0, 0, None, None);
        assert_eq!(call_engine.stack_top & 15, 0);
        assert_eq!(call_engine.exec_ctx.stack_bottom_ptr, call_engine.stack.as_ptr() as usize);
    }

    #[test]
    fn grow_stack_copies_active_window() {
        let mut call_engine = CallEngine::new(0, 0, 0, 0, 0, 0, 0, None, None);
        call_engine.stack = (0..32).collect();
        call_engine.stack_top = call_engine.stack.as_ptr() as usize + 15;
        call_engine.exec_ctx.stack_grow_required_size = 160;
        call_engine.exec_ctx.stack_pointer_before_go_call = call_engine.stack.as_ptr() as usize + 10;
        call_engine.exec_ctx.frame_pointer_before_go_call = call_engine.stack.as_ptr() as usize + 14;

        let (new_sp, new_fp) = call_engine.grow_stack().unwrap();
        assert_eq!(call_engine.stack.len(), 160 + 32 * 2 + 16);
        assert_eq!(&call_engine.stack[(new_sp - call_engine.stack.as_ptr() as usize)..][..5], &[10, 11, 12, 13, 14]);
        assert_eq!(new_fp - new_sp, 4);
    }

    #[test]
    fn grow_stack_rejects_ceiling_overflow() {
        let mut call_engine = CallEngine::new(0, 0, 0, 0, 0, 0, 0, None, None);
        call_engine.stack = vec![0; CALL_STACK_CEILING + 1];
        assert!(call_engine.grow_stack().is_err());
    }

    #[test]
    fn invoke_host_uses_stored_host_function() {
        let host = stack_host_func(|stack| {
            stack[0] += stack[1];
            Ok(())
        });
        let call_engine = CallEngine::new(3, 0, 0, 0, 2, 2, 1, Some(host), None);
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
        let mut call_engine = CallEngine::new(3, 0, 0, 0, 1, 1, 1, Some(host), None);
        let mut stack = [21];
        let results = call_engine.call(&mut stack).unwrap();
        assert_eq!(results, &[42]);
        assert_eq!(call_engine.exec_ctx.exit_code, ExitCode::OK);
    }

    #[test]
    fn exit_code_loop_grows_stack_before_returning() {
        let mut call_engine = CallEngine::new(0, 0, 0, 0, 0, 0, 0, None, None);
        call_engine.stack = vec![0; 32];
        call_engine.stack_top = call_engine.stack.as_ptr() as usize + 15;
        call_engine.exec_ctx.stack_pointer_before_go_call = call_engine.stack.as_ptr() as usize + 8;
        call_engine.exec_ctx.frame_pointer_before_go_call = call_engine.stack.as_ptr() as usize + 12;
        call_engine.exec_ctx.stack_grow_required_size = 64;
        call_engine.exec_ctx.exit_code = ExitCode::GROW_STACK;

        let results = call_engine.run_exit_code_loop(&mut []).unwrap();
        assert!(results.is_empty());
        assert!(call_engine.stack.len() > 32);
    }

    #[test]
    fn exit_code_loop_maps_traps_to_runtime_errors() {
        let mut call_engine = CallEngine::new(0, 0, 0, 0, 0, 0, 0, None, None);
        call_engine.exec_ctx.exit_code = ExitCode::FUEL_EXHAUSTED;
        let err = call_engine.run_exit_code_loop(&mut []).unwrap_err();
        assert_eq!(err, CallEngineError::Runtime(wasmruntime::FUEL_EXHAUSTED));

        call_engine.exec_ctx.exit_code = ExitCode::MEMORY_OUT_OF_BOUNDS;
        let err = call_engine.run_exit_code_loop(&mut []).unwrap_err();
        assert_eq!(
            err,
            CallEngineError::Runtime(wasmruntime::OUT_OF_BOUNDS_MEMORY_ACCESS)
        );
    }

    #[test]
    fn unsupported_runtime_builtins_remain_explicit() {
        let mut call_engine = CallEngine::new(0, 0, 0, 0, 0, 0, 0, None, None);
        call_engine.exec_ctx.exit_code = ExitCode::TABLE_GROW;
        let err = call_engine.run_exit_code_loop(&mut []).unwrap_err();
        match err {
            CallEngineError::UnsupportedExit { exit_code, .. } => {
                assert_eq!(exit_code, ExitCode::TABLE_GROW);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
