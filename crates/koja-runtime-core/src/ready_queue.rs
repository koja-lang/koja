//! The cooperative driver's ready queue: one FIFO level per
//! [`Priority`], with per-level aging so no level starves.
//!
//! Moved out of the process table when it sharded: the table returns
//! [`Wake`](crate::process_table::Wake) facts and the queue owner routes
//! them. The native adapter routes into its work-stealing deques
//! instead, so only cooperative backends use this.

use std::collections::VecDeque;

use crate::process_table::Wake;
use crate::protocol::Pid;

/// Priority levels, indexed by `Priority as usize`.
const LEVELS: usize = 3;

/// How many times a non-empty level may be passed over before it
/// preempts, bounding every level's wait so no priority is starved.
pub const STARVATION_THRESHOLD: u32 = 8;

/// A priority ready queue with starvation aging. FIFO within a level.
#[derive(Default)]
pub struct ReadyQueue {
    ages: [u32; LEVELS],
    levels: [VecDeque<Pid>; LEVELS],
}

impl ReadyQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueues a wake fact at its priority's level.
    pub fn push(&mut self, wake: Wake) {
        self.levels[wake.priority as usize].push_back(wake.pid);
    }

    /// Pops the next candidate PID. Serves the highest-priority non-empty
    /// level, except that any non-empty level passed over
    /// [`STARVATION_THRESHOLD`] times preempts (most-aged first, ties
    /// broken toward higher priority). The candidate may be stale, so the
    /// caller validates it with `ProcessTable::try_claim`.
    pub fn pop(&mut self) -> Option<Pid> {
        let level = self.next_level()?;
        self.levels[level].pop_front()
    }

    /// Chooses which level [`pop`](Self::pop) serves and ages the rest.
    fn next_level(&mut self) -> Option<usize> {
        let highest = (0..LEVELS)
            .rev()
            .find(|&level| !self.levels[level].is_empty())?;
        let chosen = (0..LEVELS)
            .filter(|&level| {
                !self.levels[level].is_empty() && self.ages[level] >= STARVATION_THRESHOLD
            })
            .max_by_key(|&level| (self.ages[level], level))
            .unwrap_or(highest);
        for level in 0..LEVELS {
            if level == chosen || self.levels[level].is_empty() {
                self.ages[level] = 0;
            } else {
                self.ages[level] += 1;
            }
        }
        Some(chosen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_table::Priority;

    fn wake(pid: Pid, priority: Priority) -> Wake {
        Wake { pid, priority }
    }

    #[test]
    fn fifo_within_a_level() {
        let mut queue = ReadyQueue::new();
        for pid in 1..=3 {
            queue.push(wake(pid, Priority::Normal));
        }
        let order: Vec<Pid> = std::iter::from_fn(|| queue.pop()).collect();
        assert_eq!(order, vec![1, 2, 3]);
    }

    #[test]
    fn higher_priority_served_first() {
        let mut queue = ReadyQueue::new();
        queue.push(wake(1, Priority::Normal));
        queue.push(wake(2, Priority::High));
        queue.push(wake(3, Priority::Low));
        let order: Vec<Pid> = std::iter::from_fn(|| queue.pop()).collect();
        assert_eq!(order, vec![2, 1, 3]);
    }

    #[test]
    fn low_priority_served_within_starvation_bound() {
        let mut queue = ReadyQueue::new();
        queue.push(wake(1, Priority::Low));
        queue.push(wake(2, Priority::High));

        // Keep the High level continuously non-empty by re-queuing its
        // process on each pop. Low must still be served within the bound.
        let mut pops = 0;
        loop {
            let pid = queue.pop().expect("a pid is always ready");
            pops += 1;
            if pid == 1 {
                break;
            }
            queue.push(wake(pid, Priority::High));
            assert!(
                pops <= STARVATION_THRESHOLD + 1,
                "low priority starved past the bound"
            );
        }
    }

    #[test]
    fn all_levels_served_when_high_and_low_stay_busy() {
        // Regression: with both High and Low continuously non-empty, a
        // "highest or forced-lowest" scheme never serves the middle Normal
        // level. Per-level aging must reach it.
        let mut queue = ReadyQueue::new();
        queue.push(wake(1, Priority::Low));
        queue.push(wake(2, Priority::Normal));
        queue.push(wake(3, Priority::High));

        let mut pops = 0;
        let mut normal_seen = false;
        let mut low_seen = false;
        loop {
            let pid = queue.pop().expect("a pid is always ready");
            pops += 1;
            match pid {
                2 => normal_seen = true,
                1 => {
                    low_seen = true;
                    queue.push(wake(1, Priority::Low));
                }
                _ => queue.push(wake(3, Priority::High)),
            }
            if normal_seen && low_seen {
                break;
            }
            assert!(
                pops <= STARVATION_THRESHOLD + LEVELS as u32,
                "a level was starved past the aging bound"
            );
        }
    }
}
