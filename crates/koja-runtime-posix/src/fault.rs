//! Stack overflow detection. A fault inside the current process
//! stack's guard region gets a diagnostic before the default signal
//! action kills the program. Any other fault crashes as before.

use std::cell::Cell;
use std::mem;
use std::ptr;

use crate::scheduler::{CURRENT_PID, STACK_SIZE};

thread_local! {
    /// Guard region `(base, len)` of the process stack this worker is
    /// currently running, `(0, 0)` on the scheduler stack. `const` +
    /// `Copy` keeps the signal handler's read a plain TLS load.
    static GUARD_RANGE: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

/// Publishes the guard region of the process this worker is switching
/// into. Pairs with [`clear_current_guard`].
pub(crate) fn set_current_guard(base: usize, len: usize) {
    GUARD_RANGE.with(|cell| cell.set((base, len)));
}

pub(crate) fn clear_current_guard() {
    GUARD_RANGE.with(|cell| cell.set((0, 0)));
}

/// Installs the process-wide SIGSEGV / SIGBUS handler (macOS reports a
/// guard hit as SIGBUS, Linux as SIGSEGV). Called once at runtime init.
pub(crate) fn install() {
    unsafe {
        let mut sa: libc::sigaction = mem::zeroed();
        sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_sigaction = fault_handler as *const () as usize;
        libc::sigaction(libc::SIGSEGV, &sa, ptr::null_mut());
        libc::sigaction(libc::SIGBUS, &sa, ptr::null_mut());
    }
}

/// Gives the calling thread an alternate signal stack, since the fault
/// handler cannot run on the stack that just overflowed. The mapping is
/// deliberately leaked because workers live for the whole program.
pub(crate) fn install_altstack() {
    let size = libc::SIGSTKSZ.max(64 * 1024);
    unsafe {
        let base = libc::mmap(
            ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        );
        if base == libc::MAP_FAILED {
            // The handler then runs on the faulting stack, so only the
            // diagnostic is lost.
            return;
        }
        let stack = libc::stack_t {
            ss_flags: 0,
            ss_size: size,
            ss_sp: base,
        };
        libc::sigaltstack(&stack, ptr::null_mut());
    }
}

/// Async-signal-safe handler: TLS loads, stack-buffer formatting, and
/// `write(2)` only. Always reinstalls the default action and returns,
/// so the faulting instruction re-executes and the default action kills
/// the program with the right status.
extern "C" fn fault_handler(sig: libc::c_int, info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
    let addr = unsafe { (*info).si_addr } as usize;
    let (guard_base, guard_len) = GUARD_RANGE.with(|cell| cell.get());
    if guard_len != 0 && addr >= guard_base && addr < guard_base + guard_len {
        write_overflow_diagnostic();
    }
    unsafe {
        let mut sa: libc::sigaction = mem::zeroed();
        sa.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(sig, &sa, ptr::null_mut());
    }
}

/// Writes `** (stack overflow) process N exceeded its M KiB stack`
/// to stderr without allocating.
fn write_overflow_diagnostic() {
    let mut buf = [0u8; 96];
    let mut len = 0;
    len = append(&mut buf, len, b"\n** (stack overflow) process ");
    len = append_decimal(&mut buf, len, CURRENT_PID.with(|c| c.get()).max(0) as u64);
    len = append(&mut buf, len, b" exceeded its ");
    len = append_decimal(&mut buf, len, (STACK_SIZE / 1024) as u64);
    len = append(&mut buf, len, b" KiB stack\n");
    unsafe {
        libc::write(
            libc::STDERR_FILENO,
            buf.as_ptr() as *const libc::c_void,
            len,
        );
    }
}

fn append(buf: &mut [u8], len: usize, text: &[u8]) -> usize {
    let end = (len + text.len()).min(buf.len());
    buf[len..end].copy_from_slice(&text[..end - len]);
    end
}

fn append_decimal(buf: &mut [u8], len: usize, value: u64) -> usize {
    let mut digits = [0u8; 20];
    let mut cursor = digits.len();
    let mut rest = value;
    loop {
        cursor -= 1;
        digits[cursor] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    append(buf, len, &digits[cursor..])
}
