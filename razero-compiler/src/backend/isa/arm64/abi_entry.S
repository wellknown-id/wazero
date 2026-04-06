.text

.global razero_arm64_entrypoint
.type razero_arm64_entrypoint, %function
razero_arm64_entrypoint:
    mov x27, x0
    mov x24, x1
    mov x0, x2
    mov x1, x3
    mov x19, x4
    mov x25, x2
    mov x26, x5
    br x27

.global razero_arm64_after_go_function_call_entrypoint
.type razero_arm64_after_go_function_call_entrypoint, %function
razero_arm64_after_go_function_call_entrypoint:
    mov x20, x0
    mov x0, x1
    mov x25, x1
    mov x19, x2
    mov x29, x3
    mov sp, x19
    br x20
