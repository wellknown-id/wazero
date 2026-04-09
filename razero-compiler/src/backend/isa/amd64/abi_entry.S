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
    movq %rax, %r15
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
    pushq %rbx
    pushq %r12
    pushq %r13
    pushq %r14
    pushq %r15
    leaq .Lafter_go_epilogue(%rip), %r10
    pushq %r10
    movq %rdi, %r11
    movq %rsi, %rax
    movq %rax, %r15
    movq %rbp, 16(%rax)
    movq %rsp, 24(%rax)
    movq %rcx, %rbp
    movq %rdx, %rsp
    jmp *%r11

.Lafter_go_epilogue:
    popq %r15
    popq %r14
    popq %r13
    popq %r12
    popq %rbx
    ret
