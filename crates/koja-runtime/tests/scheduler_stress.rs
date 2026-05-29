//! Multi-worker scheduler stress harness.
//!
//! Drives the cooperative scheduler the same way a compiled Koja program
//! does — `koja_rt_spawn` registers an entry function as PID 1, then
//! `koja_rt_main_done` boots the reactor + worker pool and runs until that
//! process dies. Here the process bodies are plain Rust `extern "C"`
//! functions (matching the `ProcessFn = extern "C" fn(*const u8)` typedef)
//! rather than LLVM-emitted ones, so the whole scheduler — context switch,
//! mailbox handoff, and `Blocked <-> Runnable <-> Running` transitions —
//! runs across real worker threads under `cargo test`.
//!
//! A controller (PID 1) spawns `CHILDREN` children and ping-pongs a one-byte
//! message with each of them for `ROUNDS` rounds. The tight blocking
//! send/receive churn is exactly the interleaving that the nondeterministic
//! SIGBUS lives in, which makes this the workload to run under
//! ThreadSanitizer (see `just tsan`). In a normal debug build it also
//! exercises the `Process::transition` `debug_assert!` guards on every edge.
//!
//! The runtime is a process-global singleton (`SCHED`, the reactor
//! `OnceLock`, signal handlers, and a one-shot `SHUTDOWN` flag), so this
//! file deliberately contains exactly one `#[test]`: a second call to
//! `koja_rt_main_done` in the same process would observe an
//! already-shutdown scheduler.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

// The runtime exposes its scheduler purely through `#[no_mangle]` C symbols;
// this test reaches them via the `extern "C"` block below. Pull the rlib onto
// the link line so those symbols resolve (without a Rust-level reference,
// rustc would drop the otherwise-unused dependency).
extern crate koja_runtime;

unsafe extern "C" {
    fn koja_rt_spawn(fn_ptr: extern "C" fn(*const u8), state_ptr: *const u8, state_len: i64)
    -> i64;
    fn koja_rt_send(pid: i64, msg_ptr: *const u8, msg_len: i64);
    fn koja_rt_receive(out: *mut u8, out_cap: i64) -> i64;
    fn koja_rt_self() -> i64;
    fn koja_rt_main_done();
}

/// Generated Koja programs emit this null-terminated app-name string; the
/// runtime's panic handler links against it. Provide an empty one so the
/// runtime rlib resolves at link time.
#[unsafe(no_mangle)]
static __koja_app_name: [u8; 1] = [0];

/// Number of children the controller spawns.
static CHILDREN: AtomicUsize = AtomicUsize::new(0);
/// Ping-pong rounds each child completes.
static ROUNDS: AtomicUsize = AtomicUsize::new(0);
/// PID the children reply to, published by the controller before it spawns
/// any child (it is PID 1, but read it back rather than hard-code it).
static CONTROLLER_PID: AtomicI64 = AtomicI64::new(0);
/// Replies the controller successfully received.
static REPLIES: AtomicUsize = AtomicUsize::new(0);
/// Children that ran to completion.
static CHILDREN_DONE: AtomicUsize = AtomicUsize::new(0);

const PING: u8 = 0xAB;
const PONG: u8 = 0xCD;

/// Blocks until a real message arrives, ignoring spurious empty wakes
/// (`koja_rt_receive` returns -1 when woken with an empty mailbox).
fn recv_blocking() {
    let mut byte = 0u8;
    while unsafe { koja_rt_receive(&mut byte, 1) } < 0 {}
}

/// Child process body: receive a ping and reply to the controller, once per
/// round, then exit.
extern "C" fn child_entry(_state: *const u8) {
    let rounds = ROUNDS.load(Ordering::SeqCst);
    let controller = CONTROLLER_PID.load(Ordering::SeqCst);
    for _ in 0..rounds {
        recv_blocking();
        unsafe { koja_rt_send(controller, &PONG, 1) };
    }
    CHILDREN_DONE.fetch_add(1, Ordering::SeqCst);
}

/// Controller process body (PID 1): spawn the children, then for each round
/// ping every child and collect one reply from each before starting the next
/// round. Returning marks PID 1 dead, which tells the scheduler to shut down.
extern "C" fn controller_entry(_state: *const u8) {
    CONTROLLER_PID.store(unsafe { koja_rt_self() }, Ordering::SeqCst);

    let children = CHILDREN.load(Ordering::SeqCst);
    let rounds = ROUNDS.load(Ordering::SeqCst);

    let kids: Vec<i64> = (0..children)
        .map(|_| unsafe { koja_rt_spawn(child_entry, std::ptr::null(), 0) })
        .collect();

    for _ in 0..rounds {
        for &pid in &kids {
            unsafe { koja_rt_send(pid, &PING, 1) };
        }
        for _ in 0..children {
            recv_blocking();
            REPLIES.fetch_add(1, Ordering::SeqCst);
        }
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[test]
fn scheduler_ping_pong_storm() {
    let children = env_usize("KOJA_STRESS_CHILDREN", 8);
    let rounds = env_usize("KOJA_STRESS_ROUNDS", 200);

    CHILDREN.store(children, Ordering::SeqCst);
    ROUNDS.store(rounds, Ordering::SeqCst);

    unsafe {
        koja_rt_spawn(controller_entry, std::ptr::null(), 0);
        koja_rt_main_done();
    }

    assert_eq!(
        CHILDREN_DONE.load(Ordering::SeqCst),
        children,
        "every child should run to completion",
    );
    assert_eq!(
        REPLIES.load(Ordering::SeqCst),
        children * rounds,
        "controller should collect one reply per child per round",
    );
}
