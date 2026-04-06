/* amd64 entry scaffold mirrored from Go's abi_entry_amd64.s.
 *
 * entrypoint(preamble_executable, function_executable, execution_context_ptr,
 *            module_context_ptr, param_result_slice_ptr, go_allocated_stack_ptr)
 *   R11 <- preamble executable
 *   R14 <- function executable
 *   RAX <- execution context
 *   RBX <- module context
 *   R12 <- param/result slice
 *   R13 <- go-allocated stack
 *   R15 <- saved execution context for signal/exit paths
 *   JMP  *R11
 *
 * after_go_function_call_entrypoint(executable, execution_context_ptr,
 *                                   stack_pointer, frame_pointer)
 *   restores BP/SP from Go and jumps back into JIT code.
 */
