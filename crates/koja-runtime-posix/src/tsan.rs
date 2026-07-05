//! ThreadSanitizer fiber annotations for the cooperative scheduler.
//!
//! [`crate::scheduler`]'s `koja_context_switch` swaps stacks behind TSan's
//! back. Without telling TSan about the switch its shadow stack faults
//! (DEADLYSIGNAL) the first time a process yields from mid-function. The
//! `__tsan_*_fiber` API lets us model each process (and each worker's
//! scheduler context) as a fiber and announce every switch.
//!
//! Everything here is gated on the `koja_tsan` cfg, which only the `just tsan`
//! recipe sets (alongside `-Zsanitizer=thread`). Everywhere else every
//! function is a no-op and [`Fiber`] is an inert null handle, so normal and
//! release builds are untouched.
//!
//! Usage from the scheduler:
//! - [`capture_scheduler_fiber`] once at the top of each worker loop.
//! - [`slot_fiber`] to fetch a process-table slot's fiber, created lazily on
//!   first use and reused across the slot's successive occupants.
//! - [`switch_to_process`] right before context-switching into a process.
//! - [`switch_to_scheduler`] right before a process yields back.
//!
//! Fibers are bound to slots, not individual processes, and are never
//! destroyed: TSan's fiber machinery caps total fibers ever created, so a
//! spawn-and-die workload that recycled a fiber per process would exhaust the
//! pool. Reusing a slot's fiber across its successive (strictly sequential)
//! occupants keeps the live fiber count at the peak concurrency instead.

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr;

/// Opaque TSan fiber handle. Null when sanitization is off (the handle is
/// never dereferenced in that case, because every operation below is a
/// no-op).
#[derive(Clone, Copy)]
pub(crate) struct Fiber(#[allow(dead_code)] *mut c_void);

// A process resumes on whichever worker claims it next, so its fiber handle
// legitimately moves between threads.
unsafe impl Send for Fiber {}

impl Fiber {
    pub(crate) const fn null() -> Self {
        Self(ptr::null_mut())
    }
}

thread_local! {
    /// This worker's own TSan fiber, captured once at worker entry and
    /// switched back to whenever a process running on this worker yields.
    static SCHED_FIBER: Cell<Fiber> = const { Cell::new(Fiber::null()) };
}

/// Captures the calling worker's current fiber as its scheduler fiber. Call
/// once at the top of each worker loop.
pub(crate) fn capture_scheduler_fiber() {
    SCHED_FIBER.with(|c| c.set(imp::current_fiber()));
}

/// The TSan fiber bound to process-table slot `index`, created on first
/// use and reused across the slot's successive (strictly sequential)
/// occupants. The single fiber-creation site. See the module docs for why
/// fibers are slot-bound rather than per-process.
pub(crate) fn slot_fiber(index: usize) -> Fiber {
    imp::slot_fiber(index)
}

/// Announces a switch into `fiber`, immediately before a worker
/// context-switches into that process.
pub(crate) fn switch_to_process(fiber: Fiber) {
    imp::switch_to(fiber);
}

/// Announces a switch back to the calling worker's scheduler fiber,
/// immediately before a process yields via `koja_context_switch`.
pub(crate) fn switch_to_scheduler() {
    imp::switch_to(SCHED_FIBER.with(|c| c.get()));
}

#[cfg(koja_tsan)]
mod imp {
    use super::Fiber;
    use std::ffi::c_void;
    use std::sync::Mutex;

    unsafe extern "C" {
        fn __tsan_create_fiber(flags: u32) -> *mut c_void;
        fn __tsan_get_current_fiber() -> *mut c_void;
        fn __tsan_switch_to_fiber(fiber: *mut c_void, flags: u32);
    }

    /// Fibers indexed by process-table slot, lazily grown and never shrunk
    /// so a slot's fiber is created once and reused across its occupants.
    static SLOT_FIBERS: Mutex<Vec<Fiber>> = Mutex::new(Vec::new());

    pub(super) fn slot_fiber(index: usize) -> Fiber {
        let mut fibers = SLOT_FIBERS.lock().unwrap();
        while fibers.len() <= index {
            fibers.push(Fiber(unsafe { __tsan_create_fiber(0) }));
        }
        fibers[index]
    }

    pub(super) fn current_fiber() -> Fiber {
        Fiber(unsafe { __tsan_get_current_fiber() })
    }

    pub(super) fn switch_to(fiber: Fiber) {
        unsafe { __tsan_switch_to_fiber(fiber.0, 0) };
    }
}

#[cfg(not(koja_tsan))]
mod imp {
    use super::Fiber;

    #[inline(always)]
    pub(super) fn slot_fiber(_index: usize) -> Fiber {
        Fiber::null()
    }

    #[inline(always)]
    pub(super) fn current_fiber() -> Fiber {
        Fiber::null()
    }

    #[inline(always)]
    pub(super) fn switch_to(_fiber: Fiber) {}
}
