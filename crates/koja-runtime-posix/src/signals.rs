//! Process-wide OS signal capture shared by the LLVM runtime
//! scheduler and the eval interpreter: async-signal-safe handlers
//! latch SIGTERM / SIGINT / SIGHUP into atomic flags, and consumers
//! drain the flags into `Lifecycle` variant indices on their own
//! schedule (the scheduler's worker loop, eval's `receive` poll).

use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

static GOT_SIGTERM: AtomicBool = AtomicBool::new(false);
static GOT_SIGINT: AtomicBool = AtomicBool::new(false);
static GOT_SIGHUP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigterm(_sig: libc::c_int) {
    GOT_SIGTERM.store(true, Ordering::Relaxed);
}

extern "C" fn handle_sigint(_sig: libc::c_int) {
    GOT_SIGINT.store(true, Ordering::Relaxed);
}

extern "C" fn handle_sighup(_sig: libc::c_int) {
    GOT_SIGHUP.store(true, Ordering::Relaxed);
}

/// Install the latching handlers for SIGTERM / SIGINT / SIGHUP and
/// unblock those signals (the parent process, e.g. cargo test
/// linking LLVM, may have left them masked).
pub fn install() {
    unsafe {
        let mut sa: libc::sigaction = mem::zeroed();
        sa.sa_flags = 0;
        libc::sigemptyset(&mut sa.sa_mask);

        sa.sa_sigaction = handle_sigterm as *const () as usize;
        libc::sigaction(libc::SIGTERM, &sa, ptr::null_mut());

        sa.sa_sigaction = handle_sigint as *const () as usize;
        libc::sigaction(libc::SIGINT, &sa, ptr::null_mut());

        sa.sa_sigaction = handle_sighup as *const () as usize;
        libc::sigaction(libc::SIGHUP, &sa, ptr::null_mut());

        let mut unblock: libc::sigset_t = mem::zeroed();
        libc::sigemptyset(&mut unblock);
        libc::sigaddset(&mut unblock, libc::SIGTERM);
        libc::sigaddset(&mut unblock, libc::SIGINT);
        libc::sigaddset(&mut unblock, libc::SIGHUP);
        libc::sigprocmask(libc::SIG_UNBLOCK, &unblock, ptr::null_mut());
    }
}

/// Atomically clear the fired-signal flags and return the
/// corresponding `Lifecycle` variant indices, in declaration order:
/// SIGTERM -> `Shutdown` (0), SIGINT -> `Interrupt` (1), SIGHUP ->
/// `Reload` (2).
pub fn drain() -> Vec<i64> {
    let mut fired = Vec::new();
    if GOT_SIGTERM.swap(false, Ordering::Relaxed) {
        fired.push(0);
    }
    if GOT_SIGINT.swap(false, Ordering::Relaxed) {
        fired.push(1);
    }
    if GOT_SIGHUP.swap(false, Ordering::Relaxed) {
        fired.push(2);
    }
    fired
}
