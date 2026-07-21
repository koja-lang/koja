//! Monitor-vs-kill churn: every watcher gets exactly one `ExitSignal`.
//!
//! Each round spawns a target and a watcher. The target blocks until
//! poked and then exits. The watcher monitors the target and blocks until
//! the `ExitSignal` arrives. The controller pokes the target immediately
//! after spawning the watcher, so the `monitor` registration races the
//! target's death across workers: some monitors land before the death
//! (staged notice on the death edge), some after (immediate notice for an
//! already-dead PID), and the stress is that neither interleaving loses
//! or duplicates the signal.
//!
//! Half the rounds monitor a target that is long dead (its slot possibly
//! recycled), pinning the immediate-notice path against stale PIDs too.
//!
//! The oracle: one `ExitSignal` per watcher, zero violations.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

extern crate koja_runtime;

mod common;
use common::*;

/// Monitor rounds the controller runs.
static ROUNDS: AtomicUsize = AtomicUsize::new(0);
/// `ExitSignal`s received across all watchers.
static EXIT_SIGNALS: AtomicUsize = AtomicUsize::new(0);
/// Non-`ExitSignal` messages watchers saw first (must stay 0).
static UNEXPECTED_TAGS: AtomicUsize = AtomicUsize::new(0);
/// Watchers that ran to completion.
static WATCHERS_DONE: AtomicUsize = AtomicUsize::new(0);
/// A target from an early round, long dead by the time late rounds
/// monitor it (the stale-PID immediate-notice path).
static STALE_TARGET: AtomicI64 = AtomicI64::new(0);

/// Target body: wait for one poke, then exit.
extern "C" fn target_entry(_state: *const u8) {
    recv_blocking();
}

/// Watcher body: monitor the PID in the init state, then block until its
/// `ExitSignal` arrives.
extern "C" fn watcher_entry(state: *const u8) {
    let target = unsafe { state_pid(state) };
    unsafe { koja_rt_monitor(target) };
    match recv_tag_blocking() {
        TAG_EXIT_SIGNAL => {
            EXIT_SIGNALS.fetch_add(1, Ordering::SeqCst);
        }
        _ => {
            UNEXPECTED_TAGS.fetch_add(1, Ordering::SeqCst);
        }
    }
    WATCHERS_DONE.fetch_add(1, Ordering::SeqCst);
}

/// Controller (PID 1): run the monitor rounds, then reap every watcher by
/// monitoring the watchers themselves.
extern "C" fn controller_entry(_state: *const u8) {
    let rounds = ROUNDS.load(Ordering::SeqCst);

    // Seed the stale-PID path: a target that dies right away, monitored
    // again by every odd round long after its slot may have been reused.
    let stale = spawn_simple(target_entry);
    send_byte(stale, 0x01);
    STALE_TARGET.store(stale, Ordering::SeqCst);

    let mut watchers = Vec::with_capacity(rounds);
    for round in 0..rounds {
        let target = if round % 2 == 0 {
            spawn_simple(target_entry)
        } else {
            STALE_TARGET.load(Ordering::SeqCst)
        };
        let watcher = spawn_with_pid(watcher_entry, target);
        watchers.push(watcher);
        // Poke a fresh target the moment its watcher exists, so the
        // monitor registration and the death race across workers.
        if round % 2 == 0 {
            send_byte(target, 0x01);
        }
    }

    // Reap: the controller monitors each watcher and waits for its exit,
    // exercising monitor-of-a-watcher while the watchers finish.
    for &watcher in &watchers {
        unsafe { koja_rt_monitor(watcher) };
    }
    for _ in 0..watchers.len() {
        recv_blocking();
    }
}

#[test]
fn every_watcher_gets_exactly_one_exit_signal() {
    let rounds = env_usize("KOJA_STRESS_MONITOR_ROUNDS", 400);
    ROUNDS.store(rounds, Ordering::SeqCst);

    spawn_simple(controller_entry);
    unsafe { koja_rt_main_done() };

    assert_eq!(
        WATCHERS_DONE.load(Ordering::SeqCst),
        rounds,
        "every watcher should run to completion",
    );
    assert_eq!(
        EXIT_SIGNALS.load(Ordering::SeqCst),
        rounds,
        "every watcher should receive exactly one ExitSignal",
    );
    assert_eq!(
        UNEXPECTED_TAGS.load(Ordering::SeqCst),
        0,
        "no watcher should be woken by anything but its ExitSignal",
    );
    assert_eq!(
        unsafe { koja_rt_sched_violations() },
        0,
        "no illegal lifecycle edge under monitor churn",
    );
}
