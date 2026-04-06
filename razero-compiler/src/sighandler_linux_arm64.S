/* Linux arm64 signal handler scaffold mirrored from Go's sighandler_linux_arm64.s.
 *
 * The Rust port keeps the handler source explicit for future global_asm wiring.
 */

.text
.global jitSigHandler
jitSigHandler:
    ret

.global faultReturnTrampoline
faultReturnTrampoline:
    ret

.global jitSigHandlerAddr
jitSigHandlerAddr:
    adr x0, jitSigHandler
    ret
