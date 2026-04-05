#include "textflag.h"

// Linux x86-64 gregset indexes.
#define REG_R15 7
#define REG_RBP 10
#define REG_RSP 15
#define REG_RIP 16

// ucontext_t offsets (Go 1.26 runtime defs_linux_amd64.go).
#define UCONTEXT_MCONTEXT_OFF 40
#define MCONTEXT_GREGS_OFF 0

// executionContext field offsets (wazevoapi/offsetdata.go).
#define EXECCTX_EXITCODE_OFF 0
#define EXECCTX_ORIGFP_OFF 16
#define EXECCTX_ORIGSP_OFF 24

// SIGSEGV handler installed via rt_sigaction.
// Linux delivers args as: (sig int, info *siginfo_t, uctx *ucontext_t).
TEXT ·jitSigHandler(SB), NOSPLIT|TOPFRAME|NOFRAME, $0-0
	// Save args we need across helper calls.
	MOVQ DI, R14 // signal number
	MOVQ SI, R13 // siginfo
	MOVQ DX, R12 // ucontext

	// Load faulting RIP from ucontext->mcontext.gregs[REG_RIP].
	LEAQ UCONTEXT_MCONTEXT_OFF(R12), R8
	LEAQ MCONTEXT_GREGS_OFF(R8), R8
	MOVQ (REG_RIP*8)(R8), DI

	// Fast path: if not JIT code, forward to Go's original SIGSEGV handler.
	MOVQ $·jitRangeCount(SB), AX
	MOVL (AX), CX
	TESTL CX, CX
	JZ not_jit

	MOVQ $·jitRanges(SB), BX
jit_loop:
	MOVQ (BX), R9
	MOVQ 8(BX), R10
	CMPQ DI, R9
	JB next_range
	CMPQ DI, R10
	JAE next_range
	JMP jit_fault
next_range:
	ADDQ $16, BX
	DECL CX
	JNZ jit_loop

not_jit:
	MOVQ R14, DI
	MOVQ R13, SI
	MOVQ R12, DX
	MOVQ $·savedGoHandler(SB), AX
	MOVQ (AX), AX
	JMP AX

jit_fault:
	// Exec context pointer is held in r15 for JIT entry.
	MOVQ (REG_R15*8)(R8), R11
	TESTQ R11, R11
	JZ not_jit

	// Mark as OOB memory fault.
	MOVL $4, (EXECCTX_EXITCODE_OFF)(R11)

	// Restore Go stack pointers.
	MOVQ (EXECCTX_ORIGSP_OFF)(R11), R9
	MOVQ R9, (REG_RSP*8)(R8)
	MOVQ (EXECCTX_ORIGFP_OFF)(R11), R9
	MOVQ R9, (REG_RBP*8)(R8)

	// Resume at trampoline RET so kernel returns to caller of entrypoint.
	MOVQ $·faultReturnTrampoline(SB), R9
	MOVQ R9, (REG_RIP*8)(R8)
	RET

TEXT ·faultReturnTrampoline(SB), NOSPLIT|NOFRAME, $0-0
	RET

TEXT ·jitSigHandlerAddr(SB), NOSPLIT|NOFRAME, $0-8
	MOVQ $·jitSigHandler(SB), AX
	MOVQ AX, ret+0(FP)
	RET
