// koja_context_switch(old_sp: *mut *mut u8, new_sp: *mut u8)
// rdi = pointer to save current RSP into
// rsi = RSP to switch to
//
// Saves callee-saved registers, swaps stack, restores registers.
// Covers macOS x86_64 and Linux x86_64 (System V ABI).

.global _koja_context_switch
.global koja_context_switch
.p2align 4

_koja_context_switch:
koja_context_switch:
    // Save callee-saved registers onto current stack
    pushq  %rbx
    pushq  %rbp
    pushq  %r12
    pushq  %r13
    pushq  %r14
    pushq  %r15

    // Save current RSP to *old_sp
    movq   %rsp, (%rdi)

    // Load new RSP
    movq   %rsi, %rsp

    // Restore callee-saved registers from new stack
    popq   %r15
    popq   %r14
    popq   %r13
    popq   %r12
    popq   %rbp
    popq   %rbx

    retq
