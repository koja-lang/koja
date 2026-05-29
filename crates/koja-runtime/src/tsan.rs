//! ThreadSanitizer fiber annotations for the cooperative scheduler.
//!
//! [`crate::scheduler`]'s `koja_context_switch` swaps stacks behind TSan's
//! back; without telling TSan about the switch its shadow stack faults
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
//! - [`capture_scheduler_fiber`] once at the top of each worker loop;
//! - [`create_process_fiber`] once per process-table slot, reused across slot
//!   reuse (see [`crate::process_table`]);
//! - [`switch_to_process`] right before context-switching into a process;
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
/// never dereferenced in that case — every operation below is a no-op).
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

/// Creates a fresh fiber modelling a process-table slot's stack.
pub(crate) fn create_process_fiber() -> Fiber {
    imp::create_fiber()
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

    unsafe extern "C" {
        fn __tsan_create_fiber(flags: u32) -> *mut c_void;
        fn __tsan_get_current_fiber() -> *mut c_void;
        fn __tsan_switch_to_fiber(fiber: *mut c_void, flags: u32);
    }

    pub(super) fn create_fiber() -> Fiber {
        Fiber(unsafe { __tsan_create_fiber(0) })
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
    pub(super) fn create_fiber() -> Fiber {
        Fiber::null()
    }

    #[inline(always)]
    pub(super) fn current_fiber() -> Fiber {
        Fiber::null()
    }

    #[inline(always)]
    pub(super) fn switch_to(_fiber: Fiber) {}
}
