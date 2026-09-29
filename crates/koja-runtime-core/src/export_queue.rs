//! The bounded queue of finished span records waiting for an exporter.
//!
//! `Trace` pushes one record per finished sampled span, and an exporter
//! process pops them on its own tick. The queue never blocks and never
//! grows past [`EXPORT_CAPACITY`]. A push against a full queue drops
//! the record and counts the drop, so tracing cannot apply
//! backpressure to the traced program. The exporter reads the counter
//! and reports it.
//!
//! One mutex guards the queue. The design accepts that as the first
//! version and measures before building per-scheduler buffers.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Records the queue holds before it starts dropping.
pub const EXPORT_CAPACITY: usize = 4096;

/// A bounded FIFO of opaque records with a drop counter. `P` is the
/// backend's owned record representation. The queue never inspects it.
pub struct ExportQueue<P> {
    records: Mutex<VecDeque<P>>,
    dropped: AtomicU64,
}

impl<P> Default for ExportQueue<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P> ExportQueue<P> {
    /// An empty queue. `const` so a backend can hold one in a static.
    pub const fn new() -> Self {
        Self {
            records: Mutex::new(VecDeque::new()),
            dropped: AtomicU64::new(0),
        }
    }

    /// Appends `record`, or drops it when the queue is full. Returns
    /// `true` when the record was queued. A dropped record is released
    /// outside the lock.
    pub fn push(&self, record: P) -> bool {
        let rejected = {
            let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
            if records.len() >= EXPORT_CAPACITY {
                Some(record)
            } else {
                records.push_back(record);
                None
            }
        };
        match rejected {
            Some(record) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                drop(record);
                false
            }
            None => true,
        }
    }

    /// Removes and returns the oldest record, or `None` when empty.
    pub fn pop(&self) -> Option<P> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }

    /// Records dropped since the runtime started.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Records waiting to be popped.
    pub fn len(&self) -> usize {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether no record is waiting.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::{EXPORT_CAPACITY, ExportQueue};

    #[test]
    fn pops_in_push_order() {
        let queue = ExportQueue::new();
        assert!(queue.push(1));
        assert!(queue.push(2));
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn drops_past_capacity_and_counts() {
        let queue = ExportQueue::new();
        for n in 0..EXPORT_CAPACITY {
            assert!(queue.push(n));
        }
        assert!(!queue.push(EXPORT_CAPACITY));
        assert!(!queue.push(EXPORT_CAPACITY + 1));
        assert_eq!(queue.dropped(), 2);
        assert_eq!(queue.len(), EXPORT_CAPACITY);
        assert_eq!(queue.pop(), Some(0));
        assert!(queue.push(EXPORT_CAPACITY), "a pop frees one slot");
    }
}
