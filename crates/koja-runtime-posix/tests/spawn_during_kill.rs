//! Spawn-during-kill: no child escapes its parent's kill cascade.
//!
//! A breeder process spawns blocked-forever children in a tight loop
//! while the controller kills it. The kill's cascade covers the children
//! that exist when the death edge lands. The dangerous window is the
//! breeder continuing to run afterwards (a deferred, `on_cpu` kill) and
//! spawning more. Those spawns must be refused over the breeder's
//! tombstone (PID 0), because the cascade will never see them.
//!
//! The oracle: after the kill settles, every successfully spawned child
//! is dead, and zero violations. `TOMBSTONE_REFUSALS` is reported as
//! evidence the mid-kill window was actually hit.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

extern crate koja_runtime;

mod common;
use common::*;

/// Kill rounds (one breeder each).
static ROUNDS: AtomicUsize = AtomicUsize::new(0);
/// Children each breeder tries to spawn before exiting on its own.
static BROOD_SIZE: AtomicUsize = AtomicUsize::new(0);
/// Every child PID any breeder successfully spawned.
static CHILDREN: Mutex<Vec<i64>> = Mutex::new(Vec::new());
/// Spawns refused over the breeder's tombstone (evidence, not asserted).
static TOMBSTONE_REFUSALS: AtomicUsize = AtomicUsize::new(0);
/// Whether the current round's breeder is done spawning (exited or
/// refused), so the controller knows the round's brood is complete.
static BREEDER_SETTLED: AtomicBool = AtomicBool::new(false);

/// Child body: block forever. Only the kill cascade ends it.
extern "C" fn child_entry(_state: *const u8) {
    loop {
        recv_blocking();
    }
}

/// Breeder body: spawn children until the brood is complete or a spawn is
/// refused over this process's own tombstone (the kill landed mid-loop).
extern "C" fn breeder_entry(_state: *const u8) {
    let brood = BROOD_SIZE.load(Ordering::SeqCst);
    for _ in 0..brood {
        let child = spawn_simple(child_entry);
        if child == 0 {
            TOMBSTONE_REFUSALS.fetch_add(1, Ordering::SeqCst);
            break;
        }
        CHILDREN.lock().unwrap().push(child);
        // Hand the quantum back so the controller's kill can interleave
        // mid-brood rather than only before or after.
        unsafe { koja_rt_yield_check() };
    }
    BREEDER_SETTLED.store(true, Ordering::SeqCst);
}

/// Controller (PID 1): spawn a breeder, kill it mid-brood, wait for the
/// breeder to stop spawning, and verify the whole brood died with it.
extern "C" fn controller_entry(_state: *const u8) {
    let rounds = ROUNDS.load(Ordering::SeqCst);
    for round in 0..rounds {
        BREEDER_SETTLED.store(false, Ordering::SeqCst);
        let breeder = spawn_simple(breeder_entry);

        // Stagger the kill so it lands at a different point of the brood
        // loop each round: sometimes before the breeder runs, sometimes
        // mid-loop, sometimes after it finished.
        for _ in 0..round {
            unsafe { koja_rt_yield_check() };
        }
        unsafe { koja_rt_kill(breeder) };

        // Wait until the breeder can spawn no more: it settled itself, or
        // it is dead and off-CPU. A deferred kill leaves it running
        // briefly, and a breeder reclaimed at switch-out never settles,
        // but by then it can no longer spawn either. Spin-yield rather
        // than block: the breeder posts no message.
        while !BREEDER_SETTLED.load(Ordering::SeqCst)
            && unsafe { koja_rt_is_process_alive(breeder) } != 0
        {
            unsafe { koja_rt_yield_check() };
        }
        for _ in 0..64 {
            unsafe { koja_rt_yield_check() };
        }
    }
}

#[test]
fn kill_cascade_reaches_every_spawned_child() {
    let rounds = env_usize("KOJA_STRESS_BREED_ROUNDS", 60);
    let brood = env_usize("KOJA_STRESS_BROOD_SIZE", 24);
    ROUNDS.store(rounds, Ordering::SeqCst);
    BROOD_SIZE.store(brood, Ordering::SeqCst);

    spawn_simple(controller_entry);
    unsafe { koja_rt_main_done() };

    let children = CHILDREN.lock().unwrap();
    assert!(!children.is_empty(), "the breeders should have spawned");
    let orphans: Vec<i64> = children
        .iter()
        .copied()
        .filter(|&pid| unsafe { koja_rt_is_process_alive(pid) } != 0)
        .collect();
    assert!(
        orphans.is_empty(),
        "every spawned child should die with its breeder, orphans: {orphans:?}",
    );
    assert_eq!(
        unsafe { koja_rt_sched_violations() },
        0,
        "no illegal lifecycle edge under spawn-during-kill churn",
    );
    eprintln!(
        "spawn_during_kill: tombstone_refusals = {}",
        TOMBSTONE_REFUSALS.load(Ordering::SeqCst),
    );
}
