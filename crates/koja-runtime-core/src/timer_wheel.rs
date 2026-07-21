//! The scheduler's timing structure: a single hashed timing wheel plus
//! an overflow map, unifying the two previously-separate concerns
//! (delayed `send_after` message delivery and receive/call deadline
//! promotion) into one keyspace keyed by fire instant.
//!
//! ## Why a wheel
//!
//! Arming and disarming a timer is `O(1)` (an index + a list push)
//! instead of the `O(log n)` of a binary heap. Firing is "drain a few
//! buckets" bounded by how many timers actually expire, not by `log n`.
//! At process scale (every `call` arms a deadline, periodic tasks
//! re-arm) that constant matters.
//!
//! ## Structure
//!
//! A circular array of [`WHEEL_SLOTS`] buckets at [`WHEEL_TICK`]
//! resolution covers a horizon of `WHEEL_SLOTS * WHEEL_TICK`. An entry
//! due at instant `t` lands in bucket `tick(t) % WHEEL_SLOTS`. Anything
//! beyond the horizon waits in the `overflow` map (ordered by fire
//! instant, so removal by key is `O(log n)`) and is drained when it
//! comes due. An `occupied` bitmap makes [`TimerWheel::nearest`] skip
//! empty buckets.
//!
//! ## Cancellation
//!
//! Deadline inserts hand back a [`TimerToken`] so the waiter's wake
//! path can remove the entry via [`TimerWheel::cancel`] instead of
//! letting it age out. Without eager cancellation every completed
//! `Ref.call` leaves a stale entry behind, and a call-heavy workload
//! carries millions of them.
//!
//! ## Correctness
//!
//! Bucketing is by *tick* (millisecond granularity), but every drain
//! re-checks the exact `fire_at <= now`, so firing precision matches an
//! exact heap. The ticks only decide distribution and cursor stepping,
//! never whether an entry is due. The load-bearing invariant: each
//! [`TimerWheel::drain_due`] scans buckets from the old cursor tick
//! through `tick(now)` inclusive (capped at one full rotation), so any
//! entry whose `fire_at <= now` is visited before the cursor passes its
//! bucket. Nothing is stranded behind the cursor for a rotation. The
//! nothing-due fast path may leave the cursor behind `tick(now)`, which
//! is safe for the same reason: the next real drain's span is capped at
//! one full rotation, so it still visits every bucket.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::protocol::Pid;

/// Bucket resolution. Sub-tick precision is recovered by the exact
/// `fire_at` re-check on drain.
const WHEEL_TICK: Duration = Duration::from_millis(1);
/// The wheel covers `WHEEL_SLOTS * WHEEL_TICK` (~4 s) before an entry
/// falls to the overflow heap.
const WHEEL_SLOTS: u64 = 4096;
/// `WHEEL_TICK` in nanoseconds, so tick math is a `u64` divide rather
/// than a 128-bit one.
const WHEEL_TICK_NANOS: u64 = WHEEL_TICK.as_nanos() as u64;
/// `u64` words backing the occupied-bucket bitmap.
const BITMAP_WORDS: usize = (WHEEL_SLOTS / 64) as usize;
/// Capacity an emptied bucket keeps. Buckets grow to absorb bursts
/// (every `Ref.call` arms a deadline entry), and `Vec` never returns
/// capacity on its own, so without this cap a burst's peak footprint
/// becomes permanent RSS. 4096 buckets a few thousand entries deep is
/// hundreds of MB.
const BUCKET_RETAIN: usize = 16;

/// What a fired wheel entry asks the table to do. The deliver payload
/// is boxed so the far more numerous deadline entries (one per
/// `Ref.call` / `receive after`) don't pay the envelope's inline size.
enum WheelKind<M> {
    /// Deliver a staged envelope to `pid`.
    Deliver(Box<M>),
    /// Promote `pid` (a receive/call deadline expired). `fire_at` is the
    /// recorded deadline the table re-validates against.
    Promote,
}

/// One scheduled entry. Bucketed at insert time by its fire tick and
/// fired when the exact `fire_at <= now`. `seq` is unique, breaking
/// ties in the fire order.
struct WheelEntry<M> {
    fire_at: Instant,
    kind: WheelKind<M>,
    pid: Pid,
    seq: u64,
}

/// Handle to a pending deadline entry, returned by
/// [`TimerWheel::insert_deadline`] and consumed by
/// [`TimerWheel::cancel`]. Records where the entry landed so the cancel
/// never searches: `slot` names the bucket for in-horizon entries, and
/// `None` means the overflow map, keyed by `(fire_at, seq)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerToken {
    fire_at: Instant,
    seq: u64,
    slot: Option<usize>,
}

/// A due entry handed back to the table, partitioned by kind.
pub enum Due<M> {
    /// Deliver `envelope` to `target_pid`.
    Deliver { envelope: M, target_pid: Pid },
    /// Promote the waiter `pid` whose deadline `fire_at` expired. The
    /// table re-validates it is still blocked on that deadline.
    Promote { pid: Pid, fire_at: Instant },
}

/// Payload of an overflow entry. The fire instant and `seq` live in
/// the map key, so cancellation is a keyed remove.
struct OverflowEntry<M> {
    kind: WheelKind<M>,
    pid: Pid,
}

/// A single hashed timing wheel with an overflow map. Holds both
/// `send_after` deliveries and receive/call deadlines.
pub struct TimerWheel<M> {
    /// Reference instant for tick math, fixed at the first insertion.
    base: Option<Instant>,
    /// Tick the cursor last advanced to (`tick(now)` of the last drain
    /// that took the slow path).
    cursor_tick: u64,
    /// Occupied-bucket bitmap, so `nearest` skips empty buckets.
    occupied: [u64; BITMAP_WORDS],
    /// Entries parked beyond the wheel horizon, soonest first.
    overflow: BTreeMap<(Instant, u64), OverflowEntry<M>>,
    /// Count of pending entries across wheel + overflow.
    pending: usize,
    /// Monotonic tie-breaker for stable ordering.
    seq: u64,
    /// The wheel buckets, grown to [`WHEEL_SLOTS`] on first insert.
    slots: Vec<Vec<WheelEntry<M>>>,
    /// Lower bound on every pending entry's `fire_at`, gating the
    /// nothing-due fast path in [`Self::drain_due`]. Tightened on
    /// insert and recomputed exactly after a slow-path drain. A cancel
    /// may leave it stale-low, which only costs one wasted slow drain,
    /// never a missed firing.
    soonest: Option<Instant>,
}

impl<M> TimerWheel<M> {
    pub const fn new() -> Self {
        Self {
            base: None,
            cursor_tick: 0,
            occupied: [0; BITMAP_WORDS],
            overflow: BTreeMap::new(),
            pending: 0,
            seq: 0,
            slots: Vec::new(),
            soonest: None,
        }
    }

    /// Whether any entry is pending.
    pub fn is_empty(&self) -> bool {
        self.pending == 0
    }

    /// Schedules `envelope` for delivery to `target_pid` at `fire_at`.
    /// Fire-and-forget: delayed sends are never cancelled, so the token
    /// is dropped.
    pub fn insert_deliver(&mut self, fire_at: Instant, target_pid: Pid, envelope: M) {
        self.insert(fire_at, target_pid, WheelKind::Deliver(Box::new(envelope)));
    }

    /// Records a receive/call deadline for `pid` at `fire_at`. The
    /// returned token lets the wake path cancel the entry.
    pub fn insert_deadline(&mut self, fire_at: Instant, pid: Pid) -> TimerToken {
        self.insert(fire_at, pid, WheelKind::Promote)
    }

    fn insert(&mut self, fire_at: Instant, pid: Pid, kind: WheelKind<M>) -> TimerToken {
        let base = self.ensure_base(fire_at);
        self.seq += 1;
        let target_tick = tick_of(base, fire_at).max(self.cursor_tick);
        let entry = WheelEntry {
            fire_at,
            kind,
            pid,
            seq: self.seq,
        };
        self.pending += 1;
        self.soonest = Some(self.soonest.map_or(fire_at, |soonest| soonest.min(fire_at)));
        let slot = if target_tick < self.cursor_tick + WHEEL_SLOTS {
            let idx = (target_tick % WHEEL_SLOTS) as usize;
            self.set_occupied(idx);
            self.slots[idx].push(entry);
            Some(idx)
        } else {
            self.overflow.insert(
                (fire_at, entry.seq),
                OverflowEntry {
                    kind: entry.kind,
                    pid,
                },
            );
            None
        };
        TimerToken {
            fire_at,
            seq: self.seq,
            slot,
        }
    }

    /// Removes the pending entry `token` refers to. A no-op when the
    /// entry already fired, so wake paths can cancel unconditionally.
    pub fn cancel(&mut self, token: TimerToken) {
        let removed = match token.slot {
            Some(idx) => {
                let bucket = &mut self.slots[idx];
                match bucket.iter().position(|entry| entry.seq == token.seq) {
                    Some(i) => {
                        bucket.swap_remove(i);
                        self.release_bucket_if_empty(idx);
                        true
                    }
                    None => false,
                }
            }
            None => self.overflow.remove(&(token.fire_at, token.seq)).is_some(),
        };
        if removed {
            self.pending -= 1;
        }
    }

    /// Removes and returns every entry due at `now`, soonest first.
    /// Promote entries surface for the table to re-validate, and
    /// deliver entries surface for the driver to route. Callers hit
    /// this on every scheduler iteration, so the nothing-due fast path
    /// (nothing pending, or the soonest entry is still in the future)
    /// returns without scanning or allocating.
    pub fn drain_due(&mut self, now: Instant) -> Vec<Due<M>> {
        if self.pending == 0 || self.soonest.is_some_and(|soonest| now < soonest) {
            return Vec::new();
        }
        let Some(base) = self.base else {
            return Vec::new();
        };
        let target = tick_of(base, now);
        let mut due: Vec<WheelEntry<M>> = Vec::new();

        // Overflow first: the soonest parked entries that have come due.
        while let Some((&(fire_at, _), _)) = self.overflow.first_key_value() {
            if fire_at > now {
                break;
            }
            let ((fire_at, seq), entry) = self.overflow.pop_first().unwrap();
            due.push(WheelEntry {
                fire_at,
                kind: entry.kind,
                pid: entry.pid,
                seq,
            });
        }

        // Wheel: scan ticks (cursor ..= target), capped at one rotation,
        // re-checking the exact fire_at. Stepping from the old cursor
        // guarantees no due entry is stranded behind the cursor.
        if !self.slots.is_empty() {
            let span = (target.saturating_sub(self.cursor_tick)).min(WHEEL_SLOTS);
            for step in 0..=span {
                let tick = self.cursor_tick + step;
                let idx = (tick % WHEEL_SLOTS) as usize;
                let mut i = 0;
                while i < self.slots[idx].len() {
                    if self.slots[idx][i].fire_at <= now {
                        due.push(self.slots[idx].swap_remove(i));
                    } else {
                        i += 1;
                    }
                }
                self.release_bucket_if_empty(idx);
            }
        }

        self.cursor_tick = target;
        self.pending -= due.len();
        self.soonest = self.nearest();
        due.sort_by_key(|entry| (entry.fire_at, entry.seq));
        due.into_iter()
            .map(|entry| match entry.kind {
                WheelKind::Deliver(envelope) => Due::Deliver {
                    envelope: *envelope,
                    target_pid: entry.pid,
                },
                WheelKind::Promote => Due::Promote {
                    pid: entry.pid,
                    fire_at: entry.fire_at,
                },
            })
            .collect()
    }

    /// Lower bound on the soonest pending fire instant, without a scan.
    /// May be stale-low after a cancel (costing one wasted drain), never
    /// stale-high, so a gate built on it can never miss a due entry.
    pub fn soonest_bound(&self) -> Option<Instant> {
        if self.pending == 0 {
            None
        } else {
            self.soonest
        }
    }

    /// The soonest pending fire instant, for sizing the idle park.
    pub fn nearest(&self) -> Option<Instant> {
        let mut best = self
            .overflow
            .first_key_value()
            .map(|(&(fire_at, _), _)| fire_at);
        for word in 0..BITMAP_WORDS {
            let mut bits = self.occupied[word];
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                let idx = word * 64 + bit;
                for entry in &self.slots[idx] {
                    best = Some(best.map_or(entry.fire_at, |cur| cur.min(entry.fire_at)));
                }
                bits &= bits - 1;
            }
        }
        best
    }

    /// Fixes the tick-math reference on first use and grows the bucket
    /// vector. Subsequent calls return the established base.
    fn ensure_base(&mut self, t: Instant) -> Instant {
        match self.base {
            Some(base) => base,
            None => {
                self.base = Some(t);
                self.slots = (0..WHEEL_SLOTS).map(|_| Vec::new()).collect();
                t
            }
        }
    }

    /// Clears the occupancy bit and returns burst capacity once a
    /// bucket empties (`Vec` never shrinks on its own).
    fn release_bucket_if_empty(&mut self, idx: usize) {
        if !self.slots[idx].is_empty() {
            return;
        }
        self.clear_occupied(idx);
        if self.slots[idx].capacity() > BUCKET_RETAIN {
            self.slots[idx].shrink_to(BUCKET_RETAIN);
        }
    }

    fn set_occupied(&mut self, idx: usize) {
        self.occupied[idx / 64] |= 1u64 << (idx % 64);
    }

    fn clear_occupied(&mut self, idx: usize) {
        self.occupied[idx / 64] &= !(1u64 << (idx % 64));
    }
}

impl<M> Default for TimerWheel<M> {
    fn default() -> Self {
        Self::new()
    }
}

/// Ticks between `base` and `t`, saturating at zero for `t < base`.
fn tick_of(base: Instant, t: Instant) -> u64 {
    (t.saturating_duration_since(base).as_nanos() as u64) / WHEEL_TICK_NANOS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pids(due: &[Due<()>]) -> Vec<Pid> {
        due.iter()
            .map(|d| match d {
                Due::Deliver { target_pid, .. } => *target_pid,
                Due::Promote { pid, .. } => *pid,
            })
            .collect()
    }

    #[test]
    fn drains_in_fire_order_only_when_due() {
        let mut wheel: TimerWheel<()> = TimerWheel::new();
        let base = Instant::now();
        wheel.insert_deliver(base + Duration::from_millis(30), 1, ());
        wheel.insert_deliver(base + Duration::from_millis(10), 2, ());
        wheel.insert_deliver(base + Duration::from_millis(20), 3, ());

        let due = wheel.drain_due(base + Duration::from_millis(25));
        assert_eq!(pids(&due), vec![2, 3], "soonest-first, only due entries");
        assert_eq!(
            wheel.nearest(),
            Some(base + Duration::from_millis(30)),
            "the 30ms entry is still pending"
        );
        assert!(!wheel.is_empty());

        let rest = wheel.drain_due(base + Duration::from_millis(40));
        assert_eq!(pids(&rest), vec![1]);
        assert!(wheel.is_empty());
        assert_eq!(wheel.nearest(), None);
    }

    #[test]
    fn far_future_entry_overflows_then_fires() {
        let mut wheel: TimerWheel<()> = TimerWheel::new();
        let base = Instant::now();
        // Well beyond the ~4s horizon.
        let far = base + Duration::from_secs(30);
        wheel.insert_deadline(far, 7);
        assert_eq!(wheel.nearest(), Some(far));

        assert!(wheel.drain_due(base + Duration::from_secs(10)).is_empty());
        let due = wheel.drain_due(far + Duration::from_millis(1));
        assert!(matches!(due.as_slice(), [Due::Promote { pid: 7, .. }]));
        assert!(wheel.is_empty());
    }

    #[test]
    fn deliver_and_promote_share_the_wheel() {
        let mut wheel: TimerWheel<()> = TimerWheel::new();
        let base = Instant::now();
        wheel.insert_deadline(base + Duration::from_millis(5), 100);
        wheel.insert_deliver(base + Duration::from_millis(15), 200, ());

        let due = wheel.drain_due(base + Duration::from_millis(20));
        assert!(matches!(due[0], Due::Promote { pid: 100, .. }));
        assert!(matches!(
            due[1],
            Due::Deliver {
                target_pid: 200,
                ..
            }
        ));
    }

    #[test]
    fn entry_earlier_than_base_still_fires() {
        // First insert fixes the base far in the future. A later, sooner
        // entry must still fire on the exact fire_at check.
        let mut wheel: TimerWheel<()> = TimerWheel::new();
        let now = Instant::now();
        wheel.insert_deadline(now + Duration::from_secs(5), 1);
        wheel.insert_deliver(now + Duration::from_millis(2), 2, ());

        let due = wheel.drain_due(now + Duration::from_millis(3));
        assert_eq!(pids(&due), vec![2]);
    }

    #[test]
    fn drained_buckets_release_burst_capacity() {
        // A call-heavy burst grows bucket Vecs. Once drained they must
        // give the capacity back or the peak becomes permanent RSS.
        let mut wheel: TimerWheel<()> = TimerWheel::new();
        let base = Instant::now();
        let fire = base + Duration::from_millis(10);
        for pid in 0..10_000 {
            wheel.insert_deadline(fire, pid);
        }

        let due = wheel.drain_due(fire + Duration::from_millis(1));
        assert_eq!(due.len(), 10_000);
        assert!(wheel.is_empty());
        let retained: usize = wheel.slots.iter().map(Vec::capacity).sum();
        assert!(
            retained <= WHEEL_SLOTS as usize * BUCKET_RETAIN,
            "bucket capacity after drain should be bounded, got {retained}",
        );
    }

    #[test]
    fn cancel_removes_slot_entry() {
        let mut wheel: TimerWheel<()> = TimerWheel::new();
        let base = Instant::now();
        // Within the wheel horizon, so the entry lives in a bucket.
        let token = wheel.insert_deadline(base + Duration::from_millis(50), 1);
        wheel.cancel(token);

        assert!(wheel.is_empty());
        assert_eq!(wheel.nearest(), None);
        assert!(wheel.drain_due(base + Duration::from_millis(60)).is_empty());
    }

    #[test]
    fn cancel_removes_overflow_entry() {
        let mut wheel: TimerWheel<()> = TimerWheel::new();
        let base = Instant::now();
        // Beyond the ~4s horizon, so the entry lives in the overflow map.
        let far = base + Duration::from_secs(30);
        let token = wheel.insert_deadline(far, 2);
        wheel.cancel(token);

        assert!(wheel.is_empty());
        assert_eq!(wheel.nearest(), None);
        assert!(wheel.drain_due(far + Duration::from_millis(1)).is_empty());
    }

    #[test]
    fn cancel_after_fire_is_a_noop() {
        let mut wheel: TimerWheel<()> = TimerWheel::new();
        let base = Instant::now();
        let token = wheel.insert_deadline(base + Duration::from_millis(5), 3);

        let due = wheel.drain_due(base + Duration::from_millis(10));
        assert!(matches!(due.as_slice(), [Due::Promote { pid: 3, .. }]));
        assert!(wheel.is_empty());

        wheel.cancel(token);
        assert!(wheel.is_empty(), "pending must not underflow");
    }

    #[test]
    fn cancelling_one_entry_leaves_the_rest() {
        let mut wheel: TimerWheel<()> = TimerWheel::new();
        let base = Instant::now();
        let fire = base + Duration::from_millis(20);
        let cancelled = wheel.insert_deadline(fire, 1);
        wheel.insert_deadline(fire, 2);
        wheel.cancel(cancelled);

        assert_eq!(wheel.nearest(), Some(fire), "survivor still pending");
        let due = wheel.drain_due(fire + Duration::from_millis(1));
        assert_eq!(pids(&due), vec![2], "only the survivor fires");
        assert!(wheel.is_empty());
    }

    #[test]
    fn fast_path_drains_do_not_strand_entries() {
        // Nothing-due drains skip the cursor advance. The eventual real
        // drain must still visit the entry's bucket and fire it.
        let mut wheel: TimerWheel<()> = TimerWheel::new();
        let base = Instant::now();
        let fire = base + Duration::from_millis(500);
        wheel.insert_deadline(fire, 9);

        for ms in 1..100 {
            assert!(
                wheel.drain_due(base + Duration::from_millis(ms)).is_empty(),
                "nothing is due yet",
            );
        }
        let due = wheel.drain_due(fire + Duration::from_millis(1));
        assert!(matches!(due.as_slice(), [Due::Promote { pid: 9, .. }]));
    }

    #[test]
    fn same_tick_entry_is_not_fired_early() {
        // Sub-tick precision: an entry later within the cursor's own tick
        // must wait for the exact fire_at rather than firing when the tick
        // is first entered, and must still fire (not be stranded behind
        // the cursor) on the next drain once the instant passes.
        let mut wheel: TimerWheel<()> = TimerWheel::new();
        let base = Instant::now();
        let fire = base + Duration::from_micros(10_500); // tick 10, mid-tick
        wheel.insert_deliver(fire, 1, ());

        let early = wheel.drain_due(base + Duration::from_millis(10));
        assert!(early.is_empty(), "must not fire before the exact instant");
        assert_eq!(wheel.nearest(), Some(fire), "still pending in its bucket");

        let due = wheel.drain_due(base + Duration::from_micros(10_600));
        assert_eq!(pids(&due), vec![1], "fires once the instant passes");
        assert!(wheel.is_empty());
    }
}
