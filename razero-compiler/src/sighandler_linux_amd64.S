.text

.equ REG_R15, 7
.equ REG_RBP, 10
.equ REG_RSP, 15
.equ REG_RIP, 16

.equ UCONTEXT_MCONTEXT_OFF, 40
.equ MCONTEXT_GREGS_OFF, 0

.equ EXECCTX_EXITCODE_OFF, 0
.equ EXECCTX_ORIGFP_OFF, 16
.equ EXECCTX_ORIGSP_OFF, 24

.globl razero_jit_sig_handler
.type razero_jit_sig_handler, @function
razero_jit_sig_handler:
    movq %rdi, %r14
    movq %rsi, %r13
    movq %rdx, %r12

    leaq UCONTEXT_MCONTEXT_OFF(%r12), %r8
    leaq MCONTEXT_GREGS_OFF(%r8), %r8
    movq REG_RIP*8(%r8), %rdi

    movq razero_jit_range_count@GOTPCREL(%rip), %rax
    movl (%rax), %ecx
    testl %ecx, %ecx
    jz .Lnot_jit

    movq razero_jit_ranges@GOTPCREL(%rip), %rbx
.Ljit_loop:
    movq (%rbx), %r9
    movq 8(%rbx), %r10
    cmpq %r9, %rdi
    jb .Lnext_range
    cmpq %r10, %rdi
    jae .Lnext_range
    jmp .Ljit_fault
.Lnext_range:
    addq $16, %rbx
    decl %ecx
    jnz .Ljit_loop

.Lnot_jit:
    movq %r14, %rdi
    movq %r13, %rsi
    movq %r12, %rdx
    movq razero_saved_go_handler@GOTPCREL(%rip), %rax
    movq (%rax), %rax
    jmp *%rax

.Ljit_fault:
    movq REG_R15*8(%r8), %r11
    testq %r11, %r11
    jz .Lnot_jit

    cmpl $24, EXECCTX_EXITCODE_OFF(%r11)
    je .Lrestore_context
    movl $26, EXECCTX_EXITCODE_OFF(%r11)

.Lrestore_context:
    movq EXECCTX_ORIGSP_OFF(%r11), %r9
    movq %r9, REG_RSP*8(%r8)
    movq EXECCTX_ORIGFP_OFF(%r11), %r9
    movq %r9, REG_RBP*8(%r8)

    leaq razero_fault_return_trampoline(%rip), %r9
    movq %r9, REG_RIP*8(%r8)
    ret

.globl razero_fault_return_trampoline
.type razero_fault_return_trampoline, @function
razero_fault_return_trampoline:
    ret

.globl razero_jit_sig_handler_addr
.type razero_jit_sig_handler_addr, @function
razero_jit_sig_handler_addr:
    leaq razero_jit_sig_handler(%rip), %rax
    ret
