/* Linux amd64 signal handler scaffold mirrored from Go's sighandler_linux_amd64.s.
 *
 * The Rust port keeps the handler source explicit for future global_asm wiring.
 */

.text
.globl jitSigHandler
jitSigHandler:
    ret

.globl faultReturnTrampoline
faultReturnTrampoline:
    ret

.globl jitSigHandlerAddr
jitSigHandlerAddr:
    lea jitSigHandler(%rip), %rax
    ret
