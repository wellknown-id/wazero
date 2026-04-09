.text

.equ UCONTEXT_MCONTEXT_OFF, 176
.equ SIGCONTEXT_REGS_OFF, 8
.equ SIGCONTEXT_SP_OFF, 256
.equ SIGCONTEXT_PC_OFF, 264

.equ EXECCTX_EXITCODE_OFF, 0
.equ EXECCTX_ORIGFP_OFF, 16
.equ EXECCTX_ORIGSP_OFF, 24
.equ EXECCTX_GORET_OFF, 32

.global razero_jit_sig_handler
.type razero_jit_sig_handler, %function
razero_jit_sig_handler:
    mov x12, x0
    mov x13, x1
    mov x14, x2

    add x8, x14, #UCONTEXT_MCONTEXT_OFF
    ldr x9, [x8, #SIGCONTEXT_PC_OFF]

    adrp x10, :got:razero_jit_range_count
    ldr x10, [x10, :got_lo12:razero_jit_range_count]
    ldr w11, [x10]
    cbz w11, .Lnot_jit

    adrp x15, :got:razero_jit_ranges
    ldr x15, [x15, :got_lo12:razero_jit_ranges]
.Ljit_loop:
    ldr x16, [x15]
    ldr x17, [x15, #8]
    cmp x9, x16
    blt .Lnext_range
    cmp x9, x17
    bge .Lnext_range
    b .Ljit_fault
.Lnext_range:
    add x15, x15, #16
    subs x11, x11, #1
    b.ne .Ljit_loop

.Lnot_jit:
    mov x0, x12
    mov x1, x13
    mov x2, x14
    adrp x10, :got:razero_saved_go_handler
    ldr x10, [x10, :got_lo12:razero_saved_go_handler]
    ldr x10, [x10]
    br x10

.Ljit_fault:
    add x10, x8, #SIGCONTEXT_REGS_OFF
    ldr x11, [x10, #200]
    cbz x11, .Lnot_jit

    mov w16, #4
    str w16, [x11, #EXECCTX_EXITCODE_OFF]

    ldr x16, [x11, #EXECCTX_ORIGSP_OFF]
    str x16, [x8, #SIGCONTEXT_SP_OFF]
    ldr x16, [x11, #EXECCTX_ORIGFP_OFF]
    str x16, [x10, #232]
    ldr x16, [x11, #EXECCTX_GORET_OFF]
    str x16, [x10, #240]

    adrp x16, razero_fault_return_trampoline
    add x16, x16, :lo12:razero_fault_return_trampoline
    str x16, [x8, #SIGCONTEXT_PC_OFF]
    ret

.global razero_fault_return_trampoline
.type razero_fault_return_trampoline, %function
razero_fault_return_trampoline:
    ret

.global razero_jit_sig_handler_addr
.type razero_jit_sig_handler_addr, %function
razero_jit_sig_handler_addr:
    adrp x0, razero_jit_sig_handler
    add x0, x0, :lo12:razero_jit_sig_handler
    ret
