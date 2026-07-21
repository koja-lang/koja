//! Multi-worker scheduler stress harness.
//!
//! Drives the cooperative scheduler the same way a compiled Koja program
//! does: `koja_rt_spawn` registers an entry function as PID 1, then
//! `koja_rt_main_done` boots the reactor + worker pool and runs until that
//! process dies. Here the process bodies are plain Rust `extern "C"`
//! functions (matching the `ProcessFn = extern "C" fn(*const u8)` typedef)
//! rather than LLVM-emitted ones, so the whole scheduler (context switch,
//! mailbox handoff, and `Blocked <-> Runnable <-> Running` transitions)
//! runs across real worker threads under `cargo test`.
//!
//! A controller (PID 1) spawns `CHILDREN` children and ping-pongs a one-byte
//! message with each of them for `ROUNDS` rounds. The tight blocking
//! send/receive churn is exactly the interleaving that the nondeterministic
//! SIGBUS lives in, which makes this the workload to run under
//! ThreadSanitizer (see `just tsan`). In a normal debug build it also
//! exercises the `ProcessTable::transition` `debug_assert!` guards on every
//! edge.
//!
//! It then runs a spawn-and-die churn phase: `WAVES` waves of `WAVE_SIZE`
//! short-lived children that each send one byte and exit. With the
//! generational slotmap, each wave's slots are freed and reused by the next,
//! so the table's backing storage stays bounded instead of growing by one
//! slot per spawn. This exercises slot reuse, generation bumping, and the
//! ready queue under real worker concurrency. The churn is configurable and
//! is disabled (`KOJA_STRESS_WAVES=0`) by `just tsan`: concurrent TSan fiber
//! reuse trips TSan's cooperative-fiber bookkeeping (see the recipe), so reuse
//! is validated in the debug build here while TSan focuses on the ping-pong
//! concurrency soak.
//!
//! It then runs a preemption phase: `SPINNERS` compute-bound children that
//! never block, each calling `koja_rt_yield_check` enough times to exhaust its
//! reduction budget repeatedly. Every exhaustion re-queues the spinner
//! (`Running -> Runnable`) and context-switches back to its worker, so this
//! soaks the cooperative-preemption path (budget decrement, `yield_running`
//! under the scheduler lock, and live-fiber migration across workers) that
//! the blocking ping-pong never hits. Like a parked process being woken on
//! another worker (not a freed slot reused), it stays TSan-clean, so `just
//! tsan` leaves it on.
//!
//! Finally it runs a steal phase: `STEAL_BURST` children are all spawned
//! before any is reaped, so a large backlog of runnable processes piles into
//! the work-stealing injectors at once and the idle workers must steal
//! batches off each other to drain it. This is the workload the per-worker
//! deques + steal protocol exist for, exercising injector hand-off and
//! cross-worker stealing under contention.
//!
//! The runtime is a process-global singleton (`SCHED`, the reactor
//! `OnceLock`, signal handlers, and a one-shot `SHUTDOWN` flag), so this
//! file deliberately contains exactly one `#[test]`: a second call to
//! `koja_rt_main_done` in the same process would observe an
//! already-shutdown scheduler.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

// Pull the runtime rlib onto the link line so its `#[no_mangle]` symbols
// resolve (without a Rust-level reference, rustc would drop the
// otherwise-unused dependency).
extern crate koja_runtime;

mod common;
use common::*;

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
/// Churn waves the controller runs (each spawns and reaps `CHURN_WAVE_SIZE`).
static CHURN_WAVES: AtomicUsize = AtomicUsize::new(0);
/// Short-lived children per churn wave.
static CHURN_WAVE_SIZE: AtomicUsize = AtomicUsize::new(0);
/// Short-lived churn children that ran to completion.
static CHURN_DONE: AtomicUsize = AtomicUsize::new(0);
/// Compute-bound spinners the controller runs in the preemption phase.
static SPINNERS: AtomicUsize = AtomicUsize::new(0);
/// `koja_rt_yield_check` calls each spinner makes before exiting.
static SPIN_ITERS: AtomicUsize = AtomicUsize::new(0);
/// Spinners that ran to completion.
static SPIN_DONE: AtomicUsize = AtomicUsize::new(0);
/// Children spawned all at once in the steal phase (before any is reaped).
static STEAL_BURST: AtomicUsize = AtomicUsize::new(0);
/// Steal-phase children that ran to completion.
static STEAL_DONE: AtomicUsize = AtomicUsize::new(0);

const PING: u8 = 0xAB;
const PONG: u8 = 0xCD;
const CHURN_BYTE: u8 = 0xEF;

/// Child process body: receive a ping and reply to the controller, once per
/// round, then exit.
extern "C" fn child_entry(_state: *const u8) {
    let rounds = ROUNDS.load(Ordering::SeqCst);
    let controller = CONTROLLER_PID.load(Ordering::SeqCst);
    for _ in 0..rounds {
        recv_blocking();
        unsafe { koja_rt_send(controller, &PONG, 1, None) };
    }
    CHILDREN_DONE.fetch_add(1, Ordering::SeqCst);
}

/// Short-lived churn child: announce completion to the controller and exit
/// immediately, so its slot is freed and recycled by a later wave.
extern "C" fn churn_child_entry(_state: *const u8) {
    let controller = CONTROLLER_PID.load(Ordering::SeqCst);
    CHURN_DONE.fetch_add(1, Ordering::SeqCst);
    unsafe { koja_rt_send(controller, &CHURN_BYTE, 1, None) };
}

/// Compute-bound spinner: never blocks, just burns reductions via
/// `koja_rt_yield_check`. With `SPIN_ITERS` well above the per-quantum
/// budget it is preempted (re-queued and switched out) many times before it
/// announces completion and exits.
extern "C" fn spin_child_entry(_state: *const u8) {
    let controller = CONTROLLER_PID.load(Ordering::SeqCst);
    for _ in 0..SPIN_ITERS.load(Ordering::SeqCst) {
        unsafe { koja_rt_yield_check() };
    }
    SPIN_DONE.fetch_add(1, Ordering::SeqCst);
    unsafe { koja_rt_send(controller, &CHURN_BYTE, 1, None) };
}

/// Steal-phase child: do a little scheduler-visible work (so it actually
/// migrates across workers) then announce completion and exit. Spawned en
/// masse, these flood the injectors and are drained by stealing.
extern "C" fn steal_child_entry(_state: *const u8) {
    let controller = CONTROLLER_PID.load(Ordering::SeqCst);
    for _ in 0..16 {
        unsafe { koja_rt_yield_check() };
    }
    STEAL_DONE.fetch_add(1, Ordering::SeqCst);
    unsafe { koja_rt_send(controller, &CHURN_BYTE, 1, None) };
}

/// Controller process body (PID 1): ping-pong with a fixed set of children,
/// then run the spawn-and-die churn phase. Returning marks PID 1 dead, which
/// tells the scheduler to shut down.
extern "C" fn controller_entry(_state: *const u8) {
    CONTROLLER_PID.store(unsafe { koja_rt_self() }, Ordering::SeqCst);

    let children = CHILDREN.load(Ordering::SeqCst);
    let rounds = ROUNDS.load(Ordering::SeqCst);

    let kids: Vec<i64> = (0..children)
        .map(|_| unsafe { koja_rt_spawn(child_entry, std::ptr::null(), 0, None) })
        .collect();

    for _ in 0..rounds {
        for &pid in &kids {
            unsafe { koja_rt_send(pid, &PING, 1, None) };
        }
        for _ in 0..children {
            recv_blocking();
            REPLIES.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Spawn-and-die churn: each wave's children die before the next wave
    // spawns, so their slots are reused rather than the table growing
    // unboundedly. Reaping a full wave before starting the next keeps the
    // freelist hot.
    let waves = CHURN_WAVES.load(Ordering::SeqCst);
    let wave_size = CHURN_WAVE_SIZE.load(Ordering::SeqCst);
    for _ in 0..waves {
        for _ in 0..wave_size {
            unsafe { koja_rt_spawn(churn_child_entry, std::ptr::null(), 0, None) };
        }
        for _ in 0..wave_size {
            recv_blocking();
        }
    }

    // Preemption: compute-bound spinners that yield repeatedly rather than
    // block, then reap them. Exercises the budget/`yield_running` path under
    // worker concurrency.
    let spinners = SPINNERS.load(Ordering::SeqCst);
    for _ in 0..spinners {
        unsafe { koja_rt_spawn(spin_child_entry, std::ptr::null(), 0, None) };
    }
    for _ in 0..spinners {
        recv_blocking();
    }

    // Steal: spawn the whole burst before reaping any, so a large backlog of
    // runnable processes lands in the injectors at once and workers steal to
    // distribute it.
    let burst = STEAL_BURST.load(Ordering::SeqCst);
    for _ in 0..burst {
        unsafe { koja_rt_spawn(steal_child_entry, std::ptr::null(), 0, None) };
    }
    for _ in 0..burst {
        recv_blocking();
    }
}

#[test]
fn scheduler_ping_pong_storm() {
    let children = env_usize("KOJA_STRESS_CHILDREN", 8);
    let rounds = env_usize("KOJA_STRESS_ROUNDS", 200);
    let waves = env_usize("KOJA_STRESS_WAVES", 50);
    let wave_size = env_usize("KOJA_STRESS_WAVE_SIZE", 16);
    let spinners = env_usize("KOJA_STRESS_SPINNERS", 8);
    let spin_iters = env_usize("KOJA_STRESS_SPIN_ITERS", 5000);
    let steal_burst = env_usize("KOJA_STRESS_STEAL_BURST", 2000);

    CHILDREN.store(children, Ordering::SeqCst);
    ROUNDS.store(rounds, Ordering::SeqCst);
    CHURN_WAVES.store(waves, Ordering::SeqCst);
    CHURN_WAVE_SIZE.store(wave_size, Ordering::SeqCst);
    SPINNERS.store(spinners, Ordering::SeqCst);
    SPIN_ITERS.store(spin_iters, Ordering::SeqCst);
    STEAL_BURST.store(steal_burst, Ordering::SeqCst);

    unsafe {
        koja_rt_spawn(controller_entry, std::ptr::null(), 0, None);
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
    assert_eq!(
        CHURN_DONE.load(Ordering::SeqCst),
        waves * wave_size,
        "every churn child should run to completion across all waves",
    );
    assert_eq!(
        SPIN_DONE.load(Ordering::SeqCst),
        spinners,
        "every spinner should be preempted repeatedly yet run to completion",
    );
    assert_eq!(
        STEAL_DONE.load(Ordering::SeqCst),
        steal_burst,
        "every steal-phase child should be drained off the injectors and complete",
    );
}
