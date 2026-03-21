; expo_context_switch(old_sp: *mut *mut u8, new_sp: *mut u8)
; rcx = pointer to save current RSP into
; rdx = RSP to switch to
;
; Saves callee-saved registers, swaps stack, restores registers.
; Windows x86_64 ABI (more callee-saved regs than SysV).

.global expo_context_switch

expo_context_switch:
    ; Save callee-saved GPRs
    pushq  %rbx
    pushq  %rbp
    pushq  %r12
    pushq  %r13
    pushq  %r14
    pushq  %r15
    pushq  %rsi
    pushq  %rdi
    ; Save callee-saved XMM registers (128-bit each)
    subq   $160, %rsp
    movaps %xmm6,  0x00(%rsp)
    movaps %xmm7,  0x10(%rsp)
    movaps %xmm8,  0x20(%rsp)
    movaps %xmm9,  0x30(%rsp)
    movaps %xmm10, 0x40(%rsp)
    movaps %xmm11, 0x50(%rsp)
    movaps %xmm12, 0x60(%rsp)
    movaps %xmm13, 0x70(%rsp)
    movaps %xmm14, 0x80(%rsp)
    movaps %xmm15, 0x90(%rsp)

    ; Save current RSP to *old_sp
    movq   %rsp, (%rcx)

    ; Load new RSP
    movq   %rdx, %rsp

    ; Restore callee-saved XMM registers
    movaps 0x00(%rsp), %xmm6
    movaps 0x10(%rsp), %xmm7
    movaps 0x20(%rsp), %xmm8
    movaps 0x30(%rsp), %xmm9
    movaps 0x40(%rsp), %xmm10
    movaps 0x50(%rsp), %xmm11
    movaps 0x60(%rsp), %xmm12
    movaps 0x70(%rsp), %xmm13
    movaps 0x80(%rsp), %xmm14
    movaps 0x90(%rsp), %xmm15
    addq   $160, %rsp
    ; Restore callee-saved GPRs
    popq   %rdi
    popq   %rsi
    popq   %r15
    popq   %r14
    popq   %r13
    popq   %r12
    popq   %rbp
    popq   %rbx

    retq
