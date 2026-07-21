//! The scheduler's timer facility, split out of [`ProcessTable`] so
//! drivers can arm, cancel, and fire timers without holding the process
//! table lock. The native adapter wraps a [`TimerService`] in its own
//! `Mutex` behind an atomic next-fire gate, and the cooperative driver
//! owns one directly. Like the table, this contains no locking of its
//! own.
//!
//! Deadlines are keyed per process: a process has at most one armed
//! deadline, and [`TimerService::arm_deadline`] replaces any previous
//! entry. Tokens stay internal, so callers never round-trip a
//! [`TimerToken`](crate::timer_wheel::TimerToken) through the process
//! table.
//!
//! The safety contract with the table: firing hands back [`Due`] entries
//! that the driver applies under the table lock, and
//! [`ProcessTable::promote_expired`](crate::process_table::ProcessTable::promote_expired)
//! re-validates each promotion against the live process state. A stale
//! entry (the waiter was woken, re-parked, or died first) is skipped, so
//! the two locks never need to be held together.
//!
//! [`ProcessTable`]: crate::process_table::ProcessTable

use std::collections::HashMap;
use std::time::Instant;

use crate::protocol::Pid;
use crate::timer_wheel::{Due, TimerToken, TimerWheel};

/// Timers, deadlines, and the drain-grace backstop, advanced by the
/// driver loop.
pub struct TimerService<M> {
    /// The armed deadline entry per process, so a wake path can cancel
    /// by PID. Arming replaces, so each PID has at most one entry.
    armed: HashMap<Pid, TimerToken>,
    /// When set, the instant after which a draining runtime force-kills
    /// stragglers. Armed once per drain (see [`Self::arm_grace`]).
    grace_deadline: Option<Instant>,
    /// The instant the wheel was last advanced to, so concurrent driver
    /// iterations at the same `now` drain the wheel exactly once.
    last_advance: Option<Instant>,
    /// Delayed deliveries and receive/call deadlines, soonest first.
    wheel: TimerWheel<M>,
}

impl<M> TimerService<M> {
    pub fn new() -> Self {
        Self {
            armed: HashMap::new(),
            grace_deadline: None,
            last_advance: None,
            wheel: TimerWheel::new(),
        }
    }

    /// Arms (or re-arms) `pid`'s wake deadline, cancelling any previous
    /// entry so a re-park never accumulates duplicates. Call after a
    /// successful `try_park`, outside the table lock.
    pub fn arm_deadline(&mut self, pid: Pid, fire_at: Instant) {
        let token = self.wheel.insert_deadline(fire_at, pid);
        if let Some(previous) = self.armed.insert(pid, token) {
            self.wheel.cancel(previous);
        }
    }

    /// Disarms `pid`'s wake deadline. A no-op when none is armed (or it
    /// already fired), so wake and reclaim paths cancel unconditionally.
    pub fn cancel_deadline(&mut self, pid: Pid) {
        if let Some(token) = self.armed.remove(&pid) {
            self.wheel.cancel(token);
        }
    }

    /// Schedules a delayed message (`send_after`). Cancellation is lazy:
    /// a timer aimed at a process that later dies is dropped
    /// (undeliverable) when it fires, reclaiming its envelope then.
    pub fn schedule_deliver(&mut self, fire_at: Instant, target_pid: Pid, envelope: M) {
        self.wheel.insert_deliver(fire_at, target_pid, envelope);
    }

    /// Drains everything due at `now`, at most once per `now`, for the
    /// driver to apply under the table lock. Fired deadlines leave the
    /// armed map here, so a later cancel for the same PID is a no-op.
    pub fn advance(&mut self, now: Instant) -> Vec<Due<M>> {
        if self.last_advance.is_some_and(|last| now <= last) {
            return Vec::new();
        }
        self.last_advance = Some(now);
        let due = self.wheel.drain_due(now);
        for entry in &due {
            if let Due::Promote { pid, .. } = entry {
                // The armed entry is exactly the fired one: arming
                // replaces, so the wheel never holds a superseded entry.
                self.armed.remove(pid);
            }
        }
        due
    }

    /// The next instant anything needs servicing: the soonest pending
    /// entry folded with the grace deadline. A lower bound (see
    /// [`TimerWheel::soonest_bound`]), never later than the true next
    /// fire, so gates and idle parks built on it cannot oversleep.
    pub fn next_fire(&self) -> Option<Instant> {
        match (self.wheel.soonest_bound(), self.grace_deadline) {
            (Some(timer), Some(grace)) => Some(timer.min(grace)),
            (timer, grace) => timer.or(grace),
        }
    }

    /// Arms the drain-grace deadline. Idempotent: a second signal
    /// neither re-arms nor extends, so the window is measured from the
    /// first one.
    pub fn arm_grace(&mut self, deadline: Instant) {
        if self.grace_deadline.is_none() {
            self.grace_deadline = Some(deadline);
        }
    }

    /// Whether the drain grace deadline has passed at `now`. Always
    /// `false` before a drain arms it.
    pub fn grace_expired(&self, now: Instant) -> bool {
        self.grace_deadline.is_some_and(|deadline| now >= deadline)
    }
}

impl<M> Default for TimerService<M> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn promoted(due: &[Due<()>]) -> Vec<Pid> {
        due.iter()
            .filter_map(|entry| match entry {
                Due::Promote { pid, .. } => Some(*pid),
                Due::Deliver { .. } => None,
            })
            .collect()
    }

    #[test]
    fn cancel_disarms_a_pending_deadline() {
        let mut timers: TimerService<()> = TimerService::new();
        let base = Instant::now();
        timers.arm_deadline(7, base + Duration::from_millis(10));
        timers.cancel_deadline(7);

        assert_eq!(timers.next_fire(), None);
        assert!(timers.advance(base + Duration::from_millis(20)).is_empty());
    }

    #[test]
    fn rearming_replaces_the_previous_entry() {
        let mut timers: TimerService<()> = TimerService::new();
        let base = Instant::now();
        let first = base + Duration::from_millis(10);
        let second = base + Duration::from_millis(30);
        timers.arm_deadline(7, first);
        timers.arm_deadline(7, second);

        assert!(
            timers.advance(first + Duration::from_millis(1)).is_empty(),
            "the replaced entry must not fire",
        );
        assert_eq!(
            promoted(&timers.advance(second + Duration::from_millis(1))),
            vec![7],
        );
    }

    #[test]
    fn fired_deadline_leaves_the_armed_map() {
        let mut timers: TimerService<()> = TimerService::new();
        let base = Instant::now();
        let fire = base + Duration::from_millis(5);
        timers.arm_deadline(7, fire);
        assert_eq!(
            promoted(&timers.advance(fire + Duration::from_millis(1))),
            vec![7]
        );

        // A cancel after the fire is a no-op, and a fresh arm works.
        timers.cancel_deadline(7);
        timers.arm_deadline(7, fire + Duration::from_millis(10));
        assert_eq!(timers.next_fire(), Some(fire + Duration::from_millis(10)));
    }

    #[test]
    fn advance_drains_at_most_once_per_instant() {
        let mut timers: TimerService<()> = TimerService::new();
        let base = Instant::now();
        let fire = base + Duration::from_millis(5);
        timers.schedule_deliver(fire, 3, ());

        let now = fire + Duration::from_millis(1);
        assert_eq!(timers.advance(now).len(), 1);
        assert!(timers.advance(now).is_empty(), "same-instant re-advance");
    }

    #[test]
    fn next_fire_folds_in_the_grace_deadline() {
        let mut timers: TimerService<()> = TimerService::new();
        let base = Instant::now();
        assert_eq!(timers.next_fire(), None);

        timers.arm_grace(base + Duration::from_secs(5));
        assert_eq!(timers.next_fire(), Some(base + Duration::from_secs(5)));

        timers.arm_deadline(7, base + Duration::from_secs(1));
        assert_eq!(timers.next_fire(), Some(base + Duration::from_secs(1)));
    }

    #[test]
    fn grace_arms_once() {
        let mut timers: TimerService<()> = TimerService::new();
        let start = Instant::now();
        assert!(!timers.grace_expired(start));

        timers.arm_grace(start + Duration::from_secs(5));
        assert!(!timers.grace_expired(start));
        assert!(timers.grace_expired(start + Duration::from_secs(5)));

        // A second signal neither re-arms nor extends the window.
        timers.arm_grace(start + Duration::from_secs(100));
        assert!(timers.grace_expired(start + Duration::from_secs(5)));
    }
}
