//! Bounded ring buffer of scheduler lifecycle events, recorded by
//! [`crate::process_table::ProcessTable`] at its state-change chokepoints.
//!
//! The trace is the debugging companion to the invariant counters
//! ([`crate::process_table::ScheduleCounters`]): when a counter fixture
//! fails, re-run with `KOJA_SCHED_TRACE` set and the adapter dumps the
//! last [`TRACE_CAPACITY`] events at shutdown, oldest first, so the
//! offending interleaving can be read directly.

use std::fmt;

use crate::process_table::ProcessState;

/// Ring size. At 4096 events the buffer covers the tail of even a
/// large storm while staying a fixed ~128 KiB.
const TRACE_CAPACITY: usize = 4096;

/// One recorded scheduler event.
#[derive(Clone, Copy)]
pub enum TraceEvent {
    /// An envelope landed in the target's mailbox.
    Delivered,
    /// A dead process's slot was reclaimed.
    Freed,
    /// A kill found the target `on_cpu`; reclaim deferred to its worker.
    KillDeferred,
    /// A park was refused because a kill already marked the target `Dead`.
    ParkRefused,
    /// A lifecycle edge applied by `ProcessTable::transition`.
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
/// number (so wraparound is visible in the dump).
pub struct TraceEntry {
    pub event: TraceEvent,
    pub pid: i64,
    pub seq: u64,
}

/// The ring itself. `entries` grows to [`TRACE_CAPACITY`] and is then
/// overwritten in place at `next`.
pub(crate) struct SchedulerTrace {
    entries: Vec<TraceEntry>,
    next: usize,
    seq: u64,
}

impl SchedulerTrace {
    pub(crate) const fn new() -> Self {
        Self {
            entries: Vec::new(),
            next: 0,
            seq: 0,
        }
    }

    pub(crate) fn record(&mut self, pid: i64, event: TraceEvent) {
        self.seq += 1;
        let entry = TraceEntry {
            event,
            pid,
            seq: self.seq,
        };
        if self.entries.len() < TRACE_CAPACITY {
            self.entries.push(entry);
        } else {
            self.entries[self.next] = entry;
        }
        self.next = (self.next + 1) % TRACE_CAPACITY;
    }

    /// Recorded entries, oldest first.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &TraceEntry> {
        let split = if self.entries.len() < TRACE_CAPACITY {
            0
        } else {
            self.next
        };
        self.entries[split..].iter().chain(&self.entries[..split])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iterates_oldest_first_after_wraparound() {
        let mut trace = SchedulerTrace::new();
        for pid in 0..(TRACE_CAPACITY as i64 + 10) {
            trace.record(pid, TraceEvent::Delivered);
        }
        let seqs: Vec<u64> = trace.iter().map(|entry| entry.seq).collect();
        assert_eq!(seqs.len(), TRACE_CAPACITY);
        assert_eq!(seqs[0], 11, "oldest surviving event");
        assert_eq!(*seqs.last().unwrap(), TRACE_CAPACITY as u64 + 10);
        assert!(seqs.windows(2).all(|pair| pair[1] == pair[0] + 1));
    }
}
