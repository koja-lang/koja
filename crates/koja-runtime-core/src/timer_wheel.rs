//! The scheduler's timing structure: a single hashed timing wheel plus
//! an overflow heap, unifying the two previously-separate concerns —
//! delayed message delivery (`send_after`) and receive/call deadline
//! promotion — into one keyspace keyed by fire instant.
//!
//! ## Why a wheel
//!
//! Arming and disarming a timer is `O(1)` (an index + a list push)
//! instead of the `O(log n)` of a binary heap; firing is "drain a few
//! buckets" bounded by how many timers actually expire, not by `log n`.
//! At process scale (every `call` arms a deadline, periodic tasks
//! re-arm) that constant matters.
//!
//! ## Structure
//!
//! A circular array of [`WHEEL_SLOTS`] buckets at [`WHEEL_TICK`]
//! resolution covers a horizon of `WHEEL_SLOTS * WHEEL_TICK`. An entry
//! due at instant `t` lands in bucket `tick(t) % WHEEL_SLOTS`. Anything
//! beyond the horizon waits in the `overflow` min-heap and is drained
//! when it comes due. An `occupied` bitmap makes [`TimerWheel::nearest`]
//! skip empty buckets.
//!
//! ## Correctness
//!
//! Bucketing is by *tick* (millisecond granularity), but every drain
//! re-checks the exact `fire_at <= now`, so firing precision matches the
//! old heap exactly — the ticks only decide distribution and cursor
//! stepping, never whether an entry is due. The load-bearing invariant:
//! each [`TimerWheel::drain_due`] scans buckets from the old cursor tick
//! through `tick(now)` inclusive (capped at one full rotation), so any
//! entry whose `fire_at <= now` is visited before the cursor passes its
//! bucket — nothing is stranded behind the cursor for a rotation.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::{Duration, Instant};

use crate::protocol::Pid;

/// Bucket resolution. Sub-tick precision is recovered by the exact
/// `fire_at` re-check on drain.
const WHEEL_TICK: Duration = Duration::from_millis(1);
/// Number of buckets; the wheel covers `WHEEL_SLOTS * WHEEL_TICK`
/// (~4 s) before an entry falls to the overflow heap.
const WHEEL_SLOTS: u64 = 4096;
/// `WHEEL_TICK` in nanoseconds, so tick math is a `u64` divide rather
/// than a 128-bit one.
const WHEEL_TICK_NANOS: u64 = WHEEL_TICK.as_nanos() as u64;
/// `u64` words backing the occupied-bucket bitmap.
const BITMAP_WORDS: usize = (WHEEL_SLOTS / 64) as usize;

/// A pending delayed message (`send_after`), surfaced by
/// [`TimerWheel::drain_due`] for the driver to deliver. The message is
/// staged at schedule time, so firing is just a delivery; an
/// undeliverable entry reclaims its payload by dropping it.
pub struct TimerEntry<M> {
    pub envelope: M,
    pub target_pid: Pid,
}

/// What a fired wheel entry asks the table to do.
enum WheelKind<M> {
    /// Deliver a staged envelope to `pid`.
    Deliver(M),
    /// Promote `pid` (a receive/call deadline expired); `fire_at` is the
    /// recorded deadline the table re-validates against.
    Promote,
}

/// One scheduled entry. Bucketed at insert time by its fire tick; fired
/// when the exact `fire_at <= now`. Ordered by `(fire_at, seq)` for the
/// overflow heap — `seq` is unique, so the order is total and `Eq` holds
/// only between an entry and itself.
struct WheelEntry<M> {
    fire_at: Instant,
    kind: WheelKind<M>,
    pid: Pid,
    seq: u64,
}

impl<M> PartialEq for WheelEntry<M> {
    fn eq(&self, other: &Self) -> bool {
        self.fire_at == other.fire_at && self.seq == other.seq
    }
}

impl<M> Eq for WheelEntry<M> {}

impl<M> PartialOrd for WheelEntry<M> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<M> Ord for WheelEntry<M> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.fire_at, self.seq).cmp(&(other.fire_at, other.seq))
    }
}

/// A due entry handed back to the table, partitioned by kind.
pub enum Due<M> {
    /// Deliver `envelope` to `target_pid`.
    Deliver { envelope: M, target_pid: Pid },
    /// Promote the waiter `pid` whose deadline `fire_at` expired; the
    /// table re-validates it is still blocked on that deadline.
    Promote { pid: Pid, fire_at: Instant },
}

/// A single hashed timing wheel with an overflow heap. Holds both
/// `send_after` deliveries and receive/call deadlines.
pub struct TimerWheel<M> {
    /// Reference instant for tick math, fixed at the first insertion.
    base: Option<Instant>,
    /// Tick the cursor last advanced to (`tick(last drained now)`).
    cursor_tick: u64,
    /// Occupied-bucket bitmap, so `nearest` skips empty buckets.
    occupied: [u64; BITMAP_WORDS],
    /// Entries parked beyond the wheel horizon, soonest first.
    overflow: BinaryHeap<Reverse<WheelEntry<M>>>,
    /// Count of pending entries across wheel + overflow.
    pending: usize,
    /// Monotonic tie-breaker for stable ordering.
    seq: u64,
    /// The wheel buckets; grown to [`WHEEL_SLOTS`] on first insert.
    slots: Vec<Vec<WheelEntry<M>>>,
}

impl<M> TimerWheel<M> {
    pub const fn new() -> Self {
        Self {
            base: None,
            cursor_tick: 0,
            occupied: [0; BITMAP_WORDS],
            overflow: BinaryHeap::new(),
            pending: 0,
            seq: 0,
            slots: Vec::new(),
        }
    }

    /// Whether any entry is pending.
    pub fn is_empty(&self) -> bool {
        self.pending == 0
    }

    /// Schedules `envelope` for delivery to `target_pid` at `fire_at`.
    pub fn insert_deliver(&mut self, fire_at: Instant, target_pid: Pid, envelope: M) {
        self.insert(fire_at, target_pid, WheelKind::Deliver(envelope));
    }

    /// Records a receive/call deadline for `pid` at `fire_at`.
    pub fn insert_deadline(&mut self, fire_at: Instant, pid: Pid) {
        self.insert(fire_at, pid, WheelKind::Promote);
    }

    fn insert(&mut self, fire_at: Instant, pid: Pid, kind: WheelKind<M>) {
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
        if target_tick < self.cursor_tick + WHEEL_SLOTS {
            let idx = (target_tick % WHEEL_SLOTS) as usize;
            self.set_occupied(idx);
            self.slots[idx].push(entry);
        } else {
            self.overflow.push(Reverse(entry));
        }
    }

    /// Removes and returns every entry due at `now`, soonest first.
    /// Promote entries surface for the table to re-validate; deliver
    /// entries surface for the driver to route.
    pub fn drain_due(&mut self, now: Instant) -> Vec<Due<M>> {
        let Some(base) = self.base else {
            return Vec::new();
        };
        let target = tick_of(base, now);
        let mut due: Vec<WheelEntry<M>> = Vec::new();

        // Overflow first: the soonest parked entries that have come due.
        while let Some(Reverse(top)) = self.overflow.peek() {
            if top.fire_at <= now {
                due.push(self.overflow.pop().unwrap().0);
            } else {
                break;
            }
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
                if self.slots[idx].is_empty() {
                    self.clear_occupied(idx);
                }
            }
        }

        self.cursor_tick = target;
        self.pending -= due.len();
        due.sort_by_key(|entry| (entry.fire_at, entry.seq));
        due.into_iter()
            .map(|entry| match entry.kind {
                WheelKind::Deliver(envelope) => Due::Deliver {
                    envelope,
                    target_pid: entry.pid,
                },
                WheelKind::Promote => Due::Promote {
                    pid: entry.pid,
                    fire_at: entry.fire_at,
                },
            })
            .collect()
    }

    /// The soonest pending fire instant, for sizing the idle park.
    pub fn nearest(&self) -> Option<Instant> {
        let mut best = self.overflow.peek().map(|Reverse(top)| top.fire_at);
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
        // First insert fixes the base far in the future; a later, sooner
        // entry must still fire on the exact fire_at check.
        let mut wheel: TimerWheel<()> = TimerWheel::new();
        let now = Instant::now();
        wheel.insert_deadline(now + Duration::from_secs(5), 1);
        wheel.insert_deliver(now + Duration::from_millis(2), 2, ());

        let due = wheel.drain_due(now + Duration::from_millis(3));
        assert_eq!(pids(&due), vec![2]);
    }

    #[test]
    fn same_tick_entry_is_not_fired_early() {
        // Sub-tick precision: an entry later within the cursor's own tick
        // must wait for the exact fire_at rather than firing when the tick
        // is first entered — and must still fire (not be stranded behind
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
