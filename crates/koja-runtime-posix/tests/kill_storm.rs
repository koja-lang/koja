//! Kill-storm stress: kills racing parks, claims, and deliveries.
//!
//! The controller runs waves of victims that block in `receive` forever,
//! then kills every victim while simultaneously waking it with a message,
//! so kills land in every phase of the victim's lifecycle: parked
//! (reclaim now), `Runnable` in a queue (stale claim later), mid-run on
//! another worker (`on_cpu`, deferred reclaim at switch-out), and racing
//! the park itself (park refused over the tombstone). Waves reuse the
//! freed slots, so stale PIDs from one wave age against recycled slots in
//! the next.
//!
//! The oracle is `koja_rt_sched_violations() == 0` with every victim
//! observably dead. `koja_rt_parks_refused` is reported (not asserted:
//! the window is nondeterministic) as evidence of hit coverage.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

extern crate koja_runtime;

mod common;
use common::*;

/// Victims per wave.
static WAVE_SIZE: AtomicUsize = AtomicUsize::new(0);
/// Kill waves the controller runs.
static WAVES: AtomicUsize = AtomicUsize::new(0);
/// Victims that were observed still alive after their kill (must stay 0).
static SURVIVORS: AtomicUsize = AtomicUsize::new(0);
/// The controller's PID, published before any victim spawns.
static CONTROLLER_PID: AtomicI64 = AtomicI64::new(0);

const PING: u8 = 0x5A;
const PONG: u8 = 0xA5;

/// Victim body: echo every ping back to the controller, forever. Only a
/// kill ends it, so every exit in this test is a forced one. The
/// non-yielding burn after each echo holds the victim on-CPU on its way
/// to the next park, widening the mid-run window so kills land while it
/// is `on_cpu` and its next park attempt hits the tombstone.
extern "C" fn victim_entry(_state: *const u8) {
    let controller = CONTROLLER_PID.load(Ordering::SeqCst);
    let mut sink = 0u64;
    loop {
        recv_blocking();
        send_byte(controller, PONG);
        for i in 0..50_000u64 {
            sink = std::hint::black_box(sink.wrapping_add(i));
        }
    }
}

/// Controller (PID 1): spawn a wave, converse with it (so every victim is
/// claimed and actively cycling), then kill it mid-conversation, verify
/// the wave died, repeat.
extern "C" fn controller_entry(_state: *const u8) {
    CONTROLLER_PID.store(unsafe { koja_rt_self() }, Ordering::SeqCst);
    let waves = WAVES.load(Ordering::SeqCst);
    let wave_size = WAVE_SIZE.load(Ordering::SeqCst);

    for _ in 0..waves {
        let victims: Vec<i64> = (0..wave_size).map(|_| spawn_simple(victim_entry)).collect();

        // One full round trip: every victim has been claimed, echoed, and
        // is parked again (or on its way back to the park).
        for &pid in &victims {
            send_byte(pid, PING);
        }
        for _ in 0..wave_size {
            recv_blocking();
        }

        // Wake the whole wave, let the wakes get claimed across the
        // workers, then kill. The wave is mid-flight when the kills land,
        // so they hit every phase: parked (reclaim now), queued (stale
        // claim later), mid-run on another worker (`on_cpu`, deferred
        // reclaim), or racing its next park (park refused over the
        // tombstone). Echoes from victims that beat their kill linger in
        // the controller's mailbox, and the next wave's round-trip count
        // absorbs them.
        for &pid in &victims {
            send_byte(pid, PING);
        }
        unsafe { koja_rt_yield_check() };
        for &pid in &victims {
            unsafe { koja_rt_kill(pid) };
        }

        // A kill marks its target dead synchronously (reclaim may be
        // deferred to the owner's switch-out, but aliveness flips at the
        // kill), so no settling wait is needed before checking.
        for &pid in &victims {
            if unsafe { koja_rt_is_process_alive(pid) } != 0 {
                SURVIVORS.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

#[test]
fn kill_storm_leaves_no_survivors_and_no_violations() {
    let waves = env_usize("KOJA_STRESS_KILL_WAVES", 40);
    let wave_size = env_usize("KOJA_STRESS_KILL_WAVE_SIZE", 32);
    WAVES.store(waves, Ordering::SeqCst);
    WAVE_SIZE.store(wave_size, Ordering::SeqCst);

    spawn_simple(controller_entry);
    unsafe { koja_rt_main_done() };

    assert_eq!(
        SURVIVORS.load(Ordering::SeqCst),
        0,
        "every killed victim should be observably dead at the kill site",
    );
    assert_eq!(
        unsafe { koja_rt_sched_violations() },
        0,
        "no illegal lifecycle edge under the kill storm",
    );
    // Coverage evidence, not a requirement: parks refused over a kill
    // tombstone prove the kill-vs-park window was actually hit.
    eprintln!("kill_storm: parks_refused = {}", unsafe {
        koja_rt_parks_refused()
    },);
}
