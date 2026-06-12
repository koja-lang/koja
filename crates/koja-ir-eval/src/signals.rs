//! Eval-side view of OS lifecycle signals. The latching handlers
//! live in [`koja_runtime::signals`] (shared with the LLVM
//! scheduler); this module adds the per-thread pending queue the
//! interpreter's `receive` polls, so a burst of signals drained in
//! one poll is delivered one message at a time — mirroring the
//! runtime, where each fired signal becomes its own mailbox
//! envelope.

use std::cell::RefCell;
use std::collections::VecDeque;

thread_local! {
    static PENDING: RefCell<VecDeque<i64>> = const { RefCell::new(VecDeque::new()) };
}

/// Install the shared SIGTERM / SIGINT / SIGHUP handlers. Called
/// once per `run_program` — project mode is the only shape with a
/// process entry that can `receive` lifecycle events.
pub(crate) fn install() {
    koja_runtime::signals::install();
}

/// Pop the next pending `Lifecycle` variant index (Shutdown=0,
/// Interrupt=1, Reload=2), refreshing the queue from the shared
/// signal flags first.
pub(crate) fn next_lifecycle() -> Option<i64> {
    PENDING.with(|pending| {
        let mut queue = pending.borrow_mut();
        queue.extend(koja_runtime::signals::drain());
        queue.pop_front()
    })
}
