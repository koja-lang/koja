// koja_context_switch(old_sp: *mut *mut u8, new_sp: *mut u8)
// rdi = pointer to save current RSP into
// rsi = RSP to switch to
//
// Saves callee-saved registers, swaps stack, restores registers.
// Covers macOS x86_64 and Linux x86_64 (System V ABI).

.global _koja_context_switch
.global koja_context_switch
.global _koja_process_start
.global koja_process_start
.p2align 4

// koja_process_start is the landing pad for the first switch onto a
// fresh process stack. Its CFI marks the frame pointer (rbp, DWARF 6)
// and return address (DWARF 16) undefined so DWARF unwinders (glibc
// backtrace, _Unwind) stop here instead of walking off the fabricated
// bottom frame. Dispatches to the process entry stashed in rbx by
// init_process_stack. The push re-aligns rsp to the SysV 16-byte call
// boundary, and since rbp is zero here it doubles as a null frame
// record. The entry never returns. ud2 traps if it ever does.
_koja_process_start:
koja_process_start:
    .cfi_startproc
    .cfi_undefined 6
    .cfi_undefined 16
    pushq  %rbp
    callq  *%rbx
    ud2
    .cfi_endproc

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
