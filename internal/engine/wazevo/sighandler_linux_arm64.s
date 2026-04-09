#include "textflag.h"

// Linux arm64 ucontext/sigcontext offsets (Go 1.26 runtime defs_linux_arm64.go).
#define UCONTEXT_MCONTEXT_OFF 176
#define SIGCONTEXT_REGS_OFF 8
#define SIGCONTEXT_SP_OFF 256
#define SIGCONTEXT_PC_OFF 264

// executionContext field offsets (wazevoapi/offsetdata.go).
#define EXECCTX_EXITCODE_OFF 0
#define EXECCTX_ORIGFP_OFF 16
#define EXECCTX_ORIGSP_OFF 24
#define EXECCTX_GORET_OFF 32

// wazevoapi.ExitCodeMemoryFault.
#define EXECCTX_EXITCODE_MEMORY_FAULT 26

// SIGSEGV handler installed via rt_sigaction.
// Linux delivers args as: (sig int, info *siginfo_t, uctx *ucontext_t).
TEXT ·jitSigHandler(SB), NOSPLIT|TOPFRAME|NOFRAME, $0-0
	// Save args we need across helper logic.
	MOVD R0, R12 // signal number
	MOVD R1, R13 // siginfo
	MOVD R2, R14 // ucontext

	// Load faulting PC from ucontext->uc_mcontext.pc.
	ADD $UCONTEXT_MCONTEXT_OFF, R14, R8
	MOVD SIGCONTEXT_PC_OFF(R8), R9

	// Fast path: if not JIT code, forward to Go's original SIGSEGV handler.
	MOVD $·jitRangeCount(SB), R10
	MOVWU (R10), R11
	CBZ R11, not_jit

	MOVD $·jitRanges(SB), R15
jit_loop:
	MOVD (R15), R16
	MOVD 8(R15), R17
	CMP R16, R9
	BLT next_range
	CMP R17, R9
	BGE next_range
	JMP jit_fault
next_range:
	ADD $16, R15, R15
	SUBS $1, R11, R11
	BNE jit_loop

not_jit:
	MOVD R12, R0
	MOVD R13, R1
	MOVD R14, R2
	MOVD $·savedGoHandler(SB), R10
	MOVD (R10), R10
	JMP (R10)

jit_fault:
	// Exec context pointer is held in x25 (regs[25]) for JIT entry.
	ADD $SIGCONTEXT_REGS_OFF, R8, R10
	MOVD 25*8(R10), R11
	CBZ R11, not_jit

	// Mark as hardware-backed memory fault.
	MOVW $EXECCTX_EXITCODE_MEMORY_FAULT, R16
	MOVW R16, EXECCTX_EXITCODE_OFF(R11)

	// Restore Go stack pointers.
	MOVD EXECCTX_ORIGSP_OFF(R11), R16
	MOVD R16, SIGCONTEXT_SP_OFF(R8)
	MOVD EXECCTX_ORIGFP_OFF(R11), R16
	MOVD R16, 29*8(R10)
	MOVD EXECCTX_GORET_OFF(R11), R16
	MOVD R16, 30*8(R10)

	// Resume at trampoline RET so kernel returns to caller of entrypoint.
	MOVD $·faultReturnTrampoline(SB), R16
	MOVD R16, SIGCONTEXT_PC_OFF(R8)
	RET

TEXT ·faultReturnTrampoline(SB), NOSPLIT|NOFRAME, $0-0
	RET

TEXT ·jitSigHandlerAddr(SB), NOSPLIT|NOFRAME, $0-8
	MOVD $·jitSigHandler(SB), R0
	MOVD R0, ret+0(FP)
	RET
