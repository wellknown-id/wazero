.text

.global razero_amd64_entrypoint
.type razero_amd64_entrypoint, @function
razero_amd64_entrypoint:
    pushq %rbx
    pushq %r12
    pushq %r13
    pushq %r14
    pushq %r15
    movq %rdi, %r11
    movq %rsi, %r14
    movq %rdx, %rax
    movq %rcx, %rbx
    movq %r8, %r12
    movq %r9, %r13
    movq %rdx, %r15
    call *%r11
    popq %r15
    popq %r14
    popq %r13
    popq %r12
    popq %rbx
    ret

.global razero_amd64_after_go_function_call_entrypoint
.type razero_amd64_after_go_function_call_entrypoint, @function
razero_amd64_after_go_function_call_entrypoint:
    movq %rdi, %r14
    movq %rsi, %rax
    movq %rsi, %r15
    movq %rdx, %rsp
    movq %rcx, %rbp
    jmp *%r14
