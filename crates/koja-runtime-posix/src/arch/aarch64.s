// koja_context_switch(old_sp: *mut *mut u8, new_sp: *mut u8)
// x0 = pointer to save current SP into
// x1 = SP to switch to
//
// Saves callee-saved registers, swaps stack, restores registers.
// Covers macOS arm64 and Linux arm64.

.global _koja_context_switch
.global koja_context_switch
.global _koja_process_start
.global koja_process_start
.p2align 2

// koja_process_start is the landing pad for the first switch onto a
// fresh process stack. Its CFI marks the frame pointer (x29, DWARF 29)
// and return address (x30, DWARF 30) undefined so DWARF unwinders
// (glibc backtrace, _Unwind) stop here instead of walking off the
// fabricated bottom frame into unmapped memory. Dispatches to the
// process entry stashed in x19 by init_process_stack. The entry never
// returns. brk traps if it ever does.
_koja_process_start:
koja_process_start:
    .cfi_startproc
    .cfi_undefined 29
    .cfi_undefined 30
    blr  x19
    brk  #0
    .cfi_endproc

_koja_context_switch:
koja_context_switch:
    // Save callee-saved registers onto current stack
    stp  x19, x20, [sp, #-160]!
    stp  x21, x22, [sp, #16]
    stp  x23, x24, [sp, #32]
    stp  x25, x26, [sp, #48]
    stp  x27, x28, [sp, #64]
    stp  x29, x30, [sp, #80]
    stp  d8,  d9,  [sp, #96]
    stp  d10, d11, [sp, #112]
    stp  d12, d13, [sp, #128]
    stp  d14, d15, [sp, #144]

    // Save current SP to *old_sp
    mov  x2, sp
    str  x2, [x0]

    // Load new SP
    mov  sp, x1

    // Restore callee-saved registers from new stack
    ldp  x19, x20, [sp]
    ldp  x21, x22, [sp, #16]
    ldp  x23, x24, [sp, #32]
    ldp  x25, x26, [sp, #48]
    ldp  x27, x28, [sp, #64]
    ldp  x29, x30, [sp, #80]
    ldp  d8,  d9,  [sp, #96]
    ldp  d10, d11, [sp, #112]
    ldp  d12, d13, [sp, #128]
    ldp  d14, d15, [sp, #144]
    add  sp, sp, #160

    ret
