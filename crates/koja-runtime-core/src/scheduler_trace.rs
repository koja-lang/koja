//! Bounded per-thread rings of scheduler lifecycle events, recorded by
//! [`crate::process_table::ProcessTable`] at its state-change chokepoints
//! and merged (by a shared sequence stamp) when dumped.
//!
//! The trace is the debugging companion to the invariant counters
//! ([`crate::process_table::ScheduleCounters`]): when a counter fixture
//! fails, re-run with `KOJA_SCHED_TRACE` set and the adapter dumps the
//! retained events at shutdown, oldest first, so the offending
//! interleaving can be read directly.
//!
//! Recording is unconditional, so it must not sit behind any shared
//! lock: every hot lifecycle edge records. Each recording thread writes
//! its own ring, reached through a thread-local handle after a one-time
//! registration; the ring's own mutex is uncontended until a dump reads
//! it. The shared `seq` stamp is what lets the dump interleave the rings
//! back into one global order.

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::process_table::ProcessState;

/// Ring size per recording thread. At 4096 events a ring covers the
/// tail of even a large storm while staying a fixed ~128 KiB.
const TRACE_CAPACITY: usize = 4096;

/// One recorded scheduler event.
#[derive(Clone, Copy)]
pub enum TraceEvent {
    /// An envelope landed in the target's mailbox.
    Delivered,
    /// A dead process's slot was reclaimed.
    Freed,
    /// A kill found the target `on_cpu`, deferring reclaim to its worker.
    KillDeferred,
    /// A park was refused because a kill already marked the target `Dead`.
    ParkRefused,
    /// An applied lifecycle edge.
    Transition {
        from: ProcessState,
        to: ProcessState,
    },
    /// An envelope bounced off a dead or stale target.
    Undeliverable,
}

impl fmt::Display for TraceEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TraceEvent::Delivered => write!(f, "delivered"),
            TraceEvent::Freed => write!(f, "freed"),
            TraceEvent::KillDeferred => write!(f, "kill deferred (on cpu)"),
            TraceEvent::ParkRefused => write!(f, "park refused (dead)"),
            TraceEvent::Transition { from, to } => write!(f, "{from:?} -> {to:?}"),
            TraceEvent::Undeliverable => write!(f, "undeliverable"),
        }
    }
}

/// A [`TraceEvent`] stamped with its target and a monotonic sequence
/// number (global across threads, so the merged dump is totally ordered).
#[derive(Clone, Copy)]
pub struct TraceEntry {
    pub event: TraceEvent,
    pub pid: i64,
    pub seq: u64,
}

/// One thread's ring. `entries` grows to [`TRACE_CAPACITY`] and is then
/// overwritten in place at `next`. Behind a mutex only its owner and the
/// dumper ever take, so the hot-path acquisition is uncontended.
struct TraceRing {
    entries: Vec<TraceEntry>,
    next: usize,
}

impl TraceRing {
    fn record(&mut self, entry: TraceEntry) {
        if self.entries.len() < TRACE_CAPACITY {
            self.entries.push(entry);
        } else {
            self.entries[self.next] = entry;
        }
        self.next = (self.next + 1) % TRACE_CAPACITY;
    }
}

/// Mints process-wide unique ids for [`SchedulerTrace`] instances, so a
/// thread-local ring handle can never resolve to a different trace that
/// reused a dropped one's address (eval tests build many tables).
static NEXT_TRACE_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// This thread's ring handles, keyed by trace id. A short flat list:
    /// a thread records against one table (rarely two, in tests).
    static THREAD_RINGS: RefCell<Vec<(u64, Weak<Mutex<TraceRing>>)>> = const { RefCell::new(Vec::new()) };
}

/// The trace: a shared sequence counter plus the registry of per-thread
/// rings, merged at dump time.
pub(crate) struct SchedulerTrace {
    /// Lazily minted unique id (0 until the first record), the
    /// thread-local registration key.
    id: AtomicU64,
    /// Every ring registered by a recording thread. Locked on
    /// registration (once per thread) and at dump, never per event.
    rings: Mutex<Vec<Arc<Mutex<TraceRing>>>>,
    seq: AtomicU64,
}

impl SchedulerTrace {
    pub(crate) const fn new() -> Self {
        Self {
            id: AtomicU64::new(0),
            rings: Mutex::new(Vec::new()),
            seq: AtomicU64::new(0),
        }
    }

    pub(crate) fn record(&self, pid: i64, event: TraceEvent) {
        let entry = TraceEntry {
            event,
            pid,
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
        };
        self.thread_ring().lock().unwrap().record(entry);
    }

    /// This thread's ring for this trace, registering one on first use.
    fn thread_ring(&self) -> Arc<Mutex<TraceRing>> {
        let id = self.trace_id();
        THREAD_RINGS.with(|rings| {
            let mut rings = rings.borrow_mut();
            if let Some(ring) = rings
                .iter()
                .find(|(ring_id, _)| *ring_id == id)
                .and_then(|(_, weak)| weak.upgrade())
            {
                return ring;
            }
            let ring = Arc::new(Mutex::new(TraceRing {
                entries: Vec::new(),
                next: 0,
            }));
            self.rings.lock().unwrap().push(Arc::clone(&ring));
            rings.retain(|(_, weak)| weak.strong_count() > 0);
            rings.push((id, Arc::downgrade(&ring)));
            ring
        })
    }

    /// The unique id, minted on first use (`new` stays `const`).
    fn trace_id(&self) -> u64 {
        let id = self.id.load(Ordering::Relaxed);
        if id != 0 {
            return id;
        }
        let minted = NEXT_TRACE_ID.fetch_add(1, Ordering::Relaxed);
        match self
            .id
            .compare_exchange(0, minted, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => minted,
            Err(existing) => existing,
        }
    }

    /// Every retained entry across all threads, oldest first.
    pub(crate) fn entries(&self) -> Vec<TraceEntry> {
        let mut merged: Vec<TraceEntry> = self
            .rings
            .lock()
            .unwrap()
            .iter()
            .flat_map(|ring| ring.lock().unwrap().entries.clone())
            .collect();
        merged.sort_by_key(|entry| entry.seq);
        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_the_newest_events_after_wraparound() {
        let trace = SchedulerTrace::new();
        for pid in 0..(TRACE_CAPACITY as i64 + 10) {
            trace.record(pid, TraceEvent::Delivered);
        }
        let seqs: Vec<u64> = trace.entries().iter().map(|entry| entry.seq).collect();
        assert_eq!(seqs.len(), TRACE_CAPACITY);
        assert_eq!(seqs[0], 11, "oldest surviving event");
        assert_eq!(*seqs.last().unwrap(), TRACE_CAPACITY as u64 + 10);
        assert!(seqs.windows(2).all(|pair| pair[1] == pair[0] + 1));
    }

    #[test]
    fn merges_rings_across_threads_in_sequence_order() {
        let trace = Arc::new(SchedulerTrace::new());
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for pid in 0..100 {
                        trace.record(pid, TraceEvent::Freed);
                    }
                });
            }
        });
        let entries = trace.entries();
        assert_eq!(entries.len(), 400);
        assert!(
            entries.windows(2).all(|pair| pair[0].seq < pair[1].seq),
            "merged dump is totally ordered by the shared stamp",
        );
    }

    #[test]
    fn two_traces_on_one_thread_stay_separate() {
        let first = SchedulerTrace::new();
        let second = SchedulerTrace::new();
        first.record(1, TraceEvent::Delivered);
        second.record(2, TraceEvent::Freed);
        assert_eq!(first.entries().len(), 1);
        assert_eq!(first.entries()[0].pid, 1);
        assert_eq!(second.entries().len(), 1);
        assert_eq!(second.entries()[0].pid, 2);
    }
}
