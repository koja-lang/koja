//! Multi-threaded cooperative scheduler for Koja lightweight processes.
//!
//! N worker OS threads share a Mutex-protected process list. Each worker
//! runs a scheduling loop: grab a runnable process, context-switch into it,
//! and when it yields (via `receive`), switch back and look for more work.
//! Idle workers park on a Condvar and are woken by `send` or `spawn`.

use std::alloc;
use std::cell::{Cell, UnsafeCell};
use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Condvar, Mutex, Once};
use std::thread;
use std::time::{Duration, Instant};

use crate::ffi::{fflush, koja_context_switch, setvbuf};
use crate::mailbox::{Mailbox, WaitTarget};
use crate::memory;
use crate::process_table::ProcessTable;
use crate::tsan;
use crate::wire::{
    Envelope, IO_READY_BUF_SIZE, IO_READY_FD_OFFSET, IO_READY_VARIANT_OFFSET, LIFECYCLE_BUF_SIZE,
    OwnedPayload, TAG_BUSINESS, TAG_HEADER_SIZE, TAG_IO_READY, TAG_LIFECYCLE, TAG_REPLY,
};

/// Checks the shared signal flags ([`crate::signals`]) and injects
/// lifecycle messages into the main process's system queue. Called
/// from the worker loop. Only takes the lock when a signal actually
/// fired.
fn poll_signals() {
    let fired = crate::signals::drain();
    if fired.is_empty() {
        return;
    }

    let main_pid = SCHED.lock().unwrap().main_pid();
    for variant in fired {
        send_lifecycle_to(main_pid, variant);
    }
}

/// Internal helper: allocates a tagged lifecycle message buffer and
/// delivers it to the target process's system queue.
fn send_lifecycle_to(pid: i64, variant: i64) {
    let buf = unsafe {
        let buf = memory::alloc(LIFECYCLE_BUF_SIZE);
        ptr::write_bytes(buf, 0, LIFECYCLE_BUF_SIZE);
        *buf = TAG_LIFECYCLE;
        *buf.add(TAG_HEADER_SIZE) = variant as u8;
        buf
    };

    deliver_or_discard(pid, Envelope::new(buf, LIFECYCLE_BUF_SIZE));
    WORK_AVAILABLE.notify_all();
}

const STACK_SIZE: usize = 512 * 1024;

pub(crate) type ProcessFn = extern "C" fn(*const u8);

// ---------------------------------------------------------------------------
// Platform-specific initial-frame layout constants
// ---------------------------------------------------------------------------

// Apple Silicon
#[cfg(target_arch = "aarch64")]
const INIT_FRAME_SIZE: usize = 160;
#[cfg(target_arch = "aarch64")]
const RET_ADDR_OFFSET: usize = 88;

// x86_64 (SysV ABI)
#[cfg(target_arch = "x86_64")]
const INIT_FRAME_SIZE: usize = 64;
#[cfg(target_arch = "x86_64")]
const RET_ADDR_OFFSET: usize = 48;

// ---------------------------------------------------------------------------
// Process & scheduler state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessState {
    /// Newly spawned, not yet entered by any worker.
    Created,
    /// Ready to run; waiting for a worker to pick it up.
    Runnable,
    /// Currently executing on a worker thread.
    Running,
    /// Waiting for a message (via `receive`). Becomes `Runnable` when a
    /// message arrives or its deadline expires.
    Blocked,
    /// Waiting for I/O readiness on a file descriptor. The reactor
    /// thread promotes this to `Runnable` when the fd is ready.
    WaitingIo,
    /// Function returned; process will not be scheduled again.
    Dead,
}

/// An `mmap`-backed process stack: a `PROT_NONE` guard page at the
/// lowest address (the growth end, since stacks grow down) followed by
/// the usable region. Held on each [`Process`] so the mapping is
/// `munmap`ped on drop when the process's resources are reclaimed.
pub(crate) struct ProcessStack {
    /// Base of the whole mapping (start of the guard page).
    base: *mut u8,
    /// Total mapped bytes: guard page + usable stack.
    size: usize,
}

impl ProcessStack {
    /// The empty placeholder left behind when a stack's ownership is
    /// moved out (see [`Process::take_resources`]). A null base drops as
    /// a no-op.
    pub(crate) const fn null() -> Self {
        Self {
            base: ptr::null_mut(),
            size: 0,
        }
    }
}

impl Drop for ProcessStack {
    fn drop(&mut self) {
        if self.base.is_null() {
            return;
        }
        unsafe {
            libc::munmap(self.base as *mut libc::c_void, self.size);
        }
    }
}

/// A single lightweight Koja process.
///
/// Each process has its own stack, a routed [`Mailbox`] of message
/// envelopes, and a state machine driven by the scheduler. Processes live
/// in a generational slotmap ([`ProcessTable`]); a PID packs the slot
/// index and generation rather than being a bare `Vec` offset.
pub(crate) struct Process {
    /// Optional wake deadline. Set by `koja_rt_receive_timeout` and
    /// `koja_rt_call_receive`, cleared on resume. The worker loop promotes
    /// `Blocked → Runnable` when the deadline passes.
    pub(crate) deadline: Option<Instant>,
    /// The compiled Koja function to call when first entering this process.
    func: ProcessFn,
    /// Heap-allocated initial state passed to `func` on first entry. Owned
    /// by the process: its payload drop glue runs (releasing the config's
    /// nested heap) when the process's resources are reclaimed.
    init_state: OwnedPayload,
    /// Routed message queues plus the one-shot reply slot.
    pub(crate) mailbox: Mailbox<Envelope>,
    /// Claim flag: `true` from the moment a worker switches into this
    /// process until that same worker has persisted the post-yield `sp`.
    ///
    /// A yielding process publishes a resumable state (`Blocked` /
    /// `WaitingIo`) under the lock *before* the context switch saves its
    /// new `sp`, so there is a window where the process looks schedulable
    /// but `sp` still points at the previous yield's (now-clobbered)
    /// frame. Gating pickup on `!on_cpu` keeps any other worker from
    /// resuming that stale frame until the owning worker writes the
    /// correct `sp` and clears the flag.
    pub(crate) on_cpu: bool,
    /// Saved stack pointer. Written by `koja_context_switch` when the
    /// process yields, read when a worker resumes it.
    pub(crate) sp: *mut u8,
    /// The process's `mmap`-backed stack, unmapped when the process dies.
    stack: ProcessStack,
    /// Current lifecycle state, driven by the scheduler and runtime intrinsics.
    pub(crate) state: ProcessState,
    /// What a `Blocked` process is waiting on, so delivery only wakes it
    /// for traffic that can satisfy the wait (a business message must not
    /// wake a caller parked on its reply slot, and vice versa). Only
    /// meaningful while `state` is `Blocked`.
    pub(crate) waiting: WaitTarget,
}

/// Process contains raw pointers that are heap-allocated and not
/// thread-affine, so cross-thread transfer is safe.
unsafe impl Send for Process {}

impl Process {
    /// Builds a freshly spawned process in the `Created` state. Called by
    /// [`ProcessTable::spawn`], which owns the slot/PID assignment.
    pub(crate) fn new(
        func: ProcessFn,
        init_state: OwnedPayload,
        stack: ProcessStack,
        sp: *mut u8,
    ) -> Self {
        Process {
            deadline: None,
            func,
            init_state,
            mailbox: Mailbox::default(),
            on_cpu: false,
            sp,
            stack,
            state: ProcessState::Created,
            waiting: WaitTarget::Receive,
        }
    }

    /// The entry function and its heap-allocated initial state, read by
    /// [`process_trampoline`] on first entry.
    pub(crate) fn entry(&self) -> (ProcessFn, *const u8) {
        (self.func, self.init_state.as_ptr())
    }

    /// Moves a dead process's reclaimable resources out of its slot,
    /// leaving empty/null placeholders so the actual frees happen when
    /// the returned [`Reclaim`] is dropped — after the `SCHED` lock is
    /// released. Idempotent: a second call returns an empty `Reclaim`
    /// (already-empty owners drop as no-ops), so a kill racing the
    /// worker loop reclaims at most once.
    pub(crate) fn take_resources(&mut self) -> Reclaim {
        Reclaim {
            init_state: mem::take(&mut self.init_state),
            mailbox: mem::take(&mut self.mailbox),
            stack: mem::replace(&mut self.stack, ProcessStack::null()),
        }
    }
}

/// Resources moved out of a dead process, freed when this value is
/// dropped — which the reclaim sites do only after the `SCHED` lock is
/// released. Produced by [`Process::take_resources`]. Each field is an
/// RAII owner, so dropping a `Reclaim` drains the mailbox (running each
/// envelope's drop glue), unmaps the stack, and releases `init_state`
/// (running its config drop glue); an already-reclaimed `Reclaim` holds
/// empty owners and drops as a no-op.
///
/// The fields are never read by name — they exist purely so their own
/// `Drop` runs at this controlled point — hence `allow(dead_code)`.
#[allow(dead_code)]
pub(crate) struct Reclaim {
    init_state: OwnedPayload,
    mailbox: Mailbox<Envelope>,
    stack: ProcessStack,
}

/// Reclaim owns heap detached from a [`Process`]; freeing it off the
/// scheduler thread is sound.
unsafe impl Send for Reclaim {}

/// Global scheduler state. Workers hold this lock briefly to find or
/// update processes; the lock is always released before context-switching.
pub(crate) static SCHED: Mutex<ProcessTable> = Mutex::new(ProcessTable::new());

/// Condvar paired with [`SCHED`]. Workers park here when idle.
/// Woken by `koja_rt_send`, `koja_rt_spawn`, the reactor, and on shutdown.
pub(crate) static WORK_AVAILABLE: Condvar = Condvar::new();

/// Set to `true` when the runtime should tear down. Once true, all
/// workers and the reactor thread exit their loops and join.
pub(crate) static SHUTDOWN: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Per-worker thread-local state
//
// CURRENT_PID: which process this worker thread is currently executing.
// SCHED_SP:    the worker's scheduler stack pointer, saved by
//              koja_context_switch when entering a process, read by the
//              process when yielding back. UnsafeCell so the assembly can
//              write directly into it.
// YIELD_SP:    the process's stack pointer, saved by koja_context_switch
//              when the process yields. The worker reads it afterward to
//              persist the value into the Mutex-protected process list.
// ---------------------------------------------------------------------------

thread_local! {
    pub(crate) static CURRENT_PID: Cell<i64> = const { Cell::new(-1) };
    pub(crate) static SCHED_SP: UnsafeCell<*mut u8> = const { UnsafeCell::new(ptr::null_mut()) };
    pub(crate) static YIELD_SP: UnsafeCell<*mut u8> = const { UnsafeCell::new(ptr::null_mut()) };
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Yields the current process back to its worker's scheduling loop,
/// returning when the worker resumes this process.
fn yield_to_scheduler() {
    tsan::switch_to_scheduler();
    let yield_sp_ptr = YIELD_SP.with(|c| c.get());
    let sched_sp = unsafe { *SCHED_SP.with(|c| c.get()) };
    unsafe {
        koja_context_switch(yield_sp_ptr, sched_sp);
    }
}

/// Routes `envelope` to `pid` under the scheduler lock, then drops any
/// leftover (an undeliverable envelope, or a stale reply displaced from
/// the reply slot) after the lock is released — payload drop glue is
/// arbitrary emitted code and shouldn't run while holding `SCHED`.
fn deliver_or_discard(pid: i64, envelope: Envelope) {
    let leftover = SCHED.lock().unwrap().deliver(pid, envelope);
    drop(leftover);
}

/// Prepares a fresh process stack so the first `koja_context_switch`
/// into it will "return" to `entry`. Zeroes the initial frame and
/// writes the trampoline address at the platform-specific return slot.
unsafe fn init_process_stack(stack_top: *mut u8, entry: unsafe extern "C" fn()) -> *mut u8 {
    unsafe {
        let sp = stack_top.sub(INIT_FRAME_SIZE);
        ptr::write_bytes(sp, 0, INIT_FRAME_SIZE);
        let ret_slot = sp.add(RET_ADDR_OFFSET) as *mut usize;
        *ret_slot = entry as usize;
        sp
    }
}

/// Entry point for every new process. Runs on the process's own stack
/// after the first context switch into it. Reads the process function
/// and initial state from the shared scheduler, calls the function,
/// marks the process dead, and yields back to the worker.
unsafe extern "C" fn process_trampoline() {
    let pid = CURRENT_PID.with(|c| c.get());

    let Some((func, init_state)) = SCHED.lock().unwrap().get(pid).map(Process::entry) else {
        return;
    };

    unsafe {
        func(init_state);
        fflush(ptr::null_mut());
    }

    SCHED.lock().unwrap().mark_dead_if_alive(pid);

    WORK_AVAILABLE.notify_all();
    yield_to_scheduler();
}

/// Maps a fresh [`STACK_SIZE`] process stack with a `PROT_NONE` guard
/// page at the growth (low-address) end and initialises it so the first
/// context switch lands in [`process_trampoline`]. Returns the mapping
/// handle (a [`ProcessStack`] that `munmap`s itself on drop) plus the
/// initial stack pointer. Aborts on mapping failure, matching the old
/// allocator.
fn allocate_process_stack() -> (ProcessStack, *mut u8) {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let size = page + STACK_SIZE;
    let oom = || alloc::handle_alloc_error(alloc::Layout::from_size_align(size, page).unwrap());

    let base = unsafe {
        libc::mmap(
            ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if base == libc::MAP_FAILED {
        oom();
    }

    // Guard the lowest page: a downward-growing stack that overruns its
    // usable region faults here instead of corrupting adjacent memory.
    if unsafe { libc::mprotect(base, page, libc::PROT_NONE) } != 0 {
        unsafe { libc::munmap(base, size) };
        oom();
    }

    let base = base as *mut u8;
    // `mmap` returns page-aligned memory, so `base + size` is 16-aligned.
    let stack_top = unsafe { base.add(size) };
    let sp = unsafe { init_process_stack(stack_top, process_trampoline) };
    (ProcessStack { base, size }, sp)
}

/// Determines how many worker threads to run.
///
/// On Linux, prefers the cgroup CPU quota so a container with `cpu: 2`
/// on a 96-core host only spawns 2 workers. Falls back to
/// [`std::thread::available_parallelism`] on macOS and bare-metal
/// Linux.
fn worker_count() -> usize {
    #[cfg(target_os = "linux")]
    if let Some(cpus) = cgroup_cpu_quota() {
        return cpus;
    }
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Reads the cgroup v2 CPU quota from `/sys/fs/cgroup/cpu.max`
/// (`"<quota> <period>"`, or `"max <period>"` when unlimited).
#[cfg(target_os = "linux")]
fn cgroup_cpu_quota() -> Option<usize> {
    let contents = std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok()?;
    let parts: Vec<&str> = contents.split_whitespace().collect();
    if parts.len() != 2 || parts[0] == "max" {
        return None;
    }
    let quota = parts[0].parse::<u64>().ok()?;
    let period = parts[1].parse::<u64>().ok()?;
    if period == 0 {
        return None;
    }
    Some((quota / period).clamp(1, 256) as usize)
}

/// Core scheduling loop run by every worker thread.
///
/// Each iteration: lock the scheduler, check for expired deadlines,
/// pick a runnable process, unlock, context-switch into it, and on
/// return persist the process's saved stack pointer. When no work is
/// available, the worker parks on [`WORK_AVAILABLE`] until woken by
/// `spawn`, `send`, or a deadline timeout. Exits when [`SHUTDOWN`] is
/// set (main process died or all processes are dead).
fn worker_loop() {
    let sched_sp_ptr = SCHED_SP.with(|c| c.get());
    let yield_sp_ptr = YIELD_SP.with(|c| c.get());
    tsan::capture_scheduler_fiber();

    loop {
        if SHUTDOWN.load(Ordering::Relaxed) {
            break;
        }

        poll_signals();

        let mut guard = SCHED.lock().unwrap();

        let now = Instant::now();
        guard.promote_due_deadlines(now);
        fire_due_timers(&mut guard, now);

        if let Some((pid, proc_sp, proc_fiber)) = guard.claim_next() {
            drop(guard);

            CURRENT_PID.with(|c| c.set(pid));
            tsan::switch_to_process(proc_fiber);
            unsafe {
                koja_context_switch(sched_sp_ptr, proc_sp);
            }

            let saved_sp = unsafe { *yield_sp_ptr };
            let mut guard = SCHED.lock().unwrap();
            // Persist the saved `sp`, release the `on_cpu` claim, and reclaim
            // the slot if the process died. Detaching resources here (under
            // the lock) lets the unmap/dealloc run after the lock is dropped.
            let reclaim = guard.after_switch(pid, saved_sp);

            let shutdown = guard.should_shutdown();
            if shutdown {
                SHUTDOWN.store(true, Ordering::Relaxed);
            }
            drop(guard);

            if shutdown {
                WORK_AVAILABLE.notify_all();
            }
            drop(reclaim);
            if shutdown {
                break;
            }
            continue;
        }

        if guard.should_shutdown() {
            SHUTDOWN.store(true, Ordering::Relaxed);
            drop(guard);
            WORK_AVAILABLE.notify_all();
            break;
        }

        let any_active = guard.any_active();
        let nearest = guard.nearest_wakeup();
        let idle_park = Duration::from_millis(if any_active { 10 } else { 100 });
        let timeout = nearest
            .map(|dl| dl.saturating_duration_since(now))
            .unwrap_or(idle_park);
        let _ = WORK_AVAILABLE.wait_timeout(guard, timeout);
    }
}

/// Delivers every timer due at `now`. The envelope was staged in wire
/// format at schedule time, so firing is a plain delivery; an
/// undeliverable timer (target gone or dead) drops its envelope,
/// running its payload drop glue.
fn fire_due_timers(table: &mut ProcessTable, now: Instant) {
    for entry in table.take_due_timers(now) {
        if let Some(undelivered) = table.deliver(entry.target_pid, entry.envelope) {
            drop(undelivered);
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime intrinsics (C ABI)
// ---------------------------------------------------------------------------

static RUNTIME_INIT: Once = Once::new();

/// One-time process-global runtime initialization. Installs the panic hook
/// that converts any Rust panic — on any thread, before unwinding — into a
/// clean diagnostic abort, so a panic can never unwind across the C-ABI or
/// poison the scheduler lock. Called at the head of every runtime entry
/// point (`koja_rt_spawn` is the first one a program reaches), so the hook
/// is live before any worker thread is spawned or any `SCHED` lock is taken.
fn ensure_runtime_init() {
    RUNTIME_INIT.call_once(crate::panic::install_panic_hook);
}

/// Called by the compiled Koja program after `main` returns.
///
/// Initializes the I/O reactor, spawns `worker_count() - 1` worker OS
/// threads plus a dedicated reactor thread, and runs the scheduling
/// loop on the current thread. Blocks until all threads finish, i.e.
/// until the main process (PID 1) dies and [`SHUTDOWN`] is set.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_main_done() {
    ensure_runtime_init();

    // Force line-buffered stdout so output is visible immediately even
    // when stdout is a pipe (e.g. when spawned by a test harness).
    unsafe {
        #[cfg(target_os = "macos")]
        unsafe extern "C" {
            #[link_name = "__stdoutp"]
            static stdout_ptr: *mut u8;
        }
        #[cfg(target_os = "linux")]
        unsafe extern "C" {
            #[link_name = "stdout"]
            static stdout_ptr: *mut u8;
        }
        const _IOLBF: i32 = 1;
        setvbuf(stdout_ptr, ptr::null_mut(), _IOLBF, 0);
    }

    crate::signals::install();
    crate::reactor::init();

    let n = worker_count();
    let mut handles = Vec::with_capacity(n);

    handles.push(thread::spawn(crate::reactor::reactor_loop));

    for _ in 1..n {
        handles.push(thread::spawn(worker_loop));
    }

    worker_loop();

    crate::reactor::notify();

    for h in handles {
        let _ = h.join();
    }

    maybe_report_live_heap();
    maybe_dump_sched_trace();
}

/// When `KOJA_HEAP_REPORT` is set, print the runtime's net live-block
/// count at shutdown. Informational only — it does *not* alter the exit
/// code: runtime-internal allocations (and any orphaned live processes)
/// are still counted here, so a nonzero total is expected for real
/// programs. The robust leak guard is the steady-state delta check in
/// the `lang_ownership` fixtures (see [`crate::memory::koja_rt_live_blocks`]).
fn maybe_report_live_heap() {
    if std::env::var_os("KOJA_HEAP_REPORT").is_some() {
        eprintln!(
            "koja: live heap blocks at shutdown: {}",
            crate::memory::koja_rt_live_blocks(),
        );
    }
}

/// When `KOJA_SCHED_TRACE` is set, dump the scheduler's lifecycle event
/// ring (oldest first) and counter totals at shutdown. The debugging
/// companion to `koja_rt_sched_violations`: when a race fixture fails,
/// re-run with this set and read the offending interleaving directly.
fn maybe_dump_sched_trace() {
    if std::env::var_os("KOJA_SCHED_TRACE").is_none() {
        return;
    }
    let guard = SCHED.lock().unwrap();
    for entry in guard.trace_entries() {
        eprintln!(
            "koja sched: {:>8} pid {:#012x} {}",
            entry.seq, entry.pid, entry.event,
        );
    }
    let counters = guard.counters();
    eprintln!(
        "koja sched: violations={} parks_refused={} kills_deferred={} \
         stale_claims_skipped={} stale_deadlines_skipped={} undeliverable_envelopes={}",
        counters.violations,
        counters.parks_refused,
        counters.kills_deferred,
        counters.stale_claims_skipped,
        counters.stale_deadlines_skipped,
        counters.undeliverable_envelopes,
    );
}

/// Hands a delivered envelope to the receiving frame: copies its
/// payload into `out`, frees the transport buffer, and returns the
/// wire tag. The copy is clamped to `min(payload, out_cap)` so an
/// oversized payload region can't overflow the receiver's slot and an
/// oversized slot can't over-read the buffer. The nested Koja heap the
/// payload may reference now belongs to the receiver, so this frees
/// only the transport buffer (never `drop_glue`).
fn deliver_envelope(envelope: Envelope, out: *mut u8, out_cap: i64) -> i64 {
    let tag = unsafe { *envelope.buffer } as i64;
    let copy_len = (envelope.length - TAG_HEADER_SIZE).min(out_cap.max(0) as usize);
    unsafe {
        ptr::copy_nonoverlapping(envelope.buffer.add(TAG_HEADER_SIZE), out, copy_len);
    }
    envelope.free_transport();
    tag
}

/// Blocking receive. Copies the next message's payload into `out` (at
/// most `out_cap` bytes), frees the transport buffer, and returns its
/// wire tag; context-switches back to the scheduler (marking the
/// process `Blocked`) until a message arrives. System traffic is
/// drained before business traffic (see [`Mailbox::pop_received`]).
/// Returns `-1` only if woken with empty queues, which no live wake
/// path produces.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_receive(out: *mut u8, out_cap: i64) -> i64 {
    let pid = CURRENT_PID.with(|c| c.get());

    {
        let mut guard = SCHED.lock().unwrap();
        let popped = guard.get_mut(pid).and_then(|p| p.mailbox.pop_received());
        if let Some(envelope) = popped {
            drop(guard);
            return deliver_envelope(envelope, out, out_cap);
        }
        guard.try_park(pid, WaitTarget::Receive, None);
    }

    yield_to_scheduler();

    let envelope = {
        let mut guard = SCHED.lock().unwrap();
        guard.get_mut(pid).and_then(|p| p.mailbox.pop_received())
    };
    envelope.map_or(-1, |envelope| deliver_envelope(envelope, out, out_cap))
}

/// Receive with a timeout. Like [`koja_rt_receive`] but sets a
/// deadline `timeout_ms` milliseconds in the future. If no message
/// arrives before the deadline, the worker loop promotes the process
/// back to `Runnable` and this returns `-1`.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_receive_timeout(out: *mut u8, out_cap: i64, timeout_ms: i64) -> i64 {
    let pid = CURRENT_PID.with(|c| c.get());

    {
        let mut guard = SCHED.lock().unwrap();
        let popped = guard.get_mut(pid).and_then(|p| p.mailbox.pop_received());
        if let Some(envelope) = popped {
            drop(guard);
            return deliver_envelope(envelope, out, out_cap);
        }
        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
        guard.try_park(pid, WaitTarget::Receive, Some(deadline));
    }

    yield_to_scheduler();

    let envelope = {
        let mut guard = SCHED.lock().unwrap();
        guard.get_mut(pid).and_then(|p| {
            p.deadline = None;
            p.mailbox.pop_received()
        })
    };
    envelope.map_or(-1, |envelope| deliver_envelope(envelope, out, out_cap))
}

/// Monotonic source of call-correlation tokens. Global rather than
/// per-process so tokens never repeat for the lifetime of the runtime —
/// a stale reply can never collide with a later call's token.
static CALL_TOKEN: AtomicI64 = AtomicI64::new(0);

/// Mints a fresh correlation token for a `Ref.call`. The caller stamps
/// it into the outgoing request's `ReplyTo` and then waits for it via
/// [`koja_rt_call_receive`].
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_call_token() -> i64 {
    CALL_TOKEN.fetch_add(1, Ordering::Relaxed) + 1
}

/// Blocks until the reply correlated with `token` lands in this
/// process's reply slot, copies its payload into `out` (at most
/// `out_cap` bytes), and returns `0`; returns `-1` if `timeout_ms`
/// elapses first. A slotted reply with a different token is a stale
/// leftover from an earlier call that timed out — it is dropped (running
/// its payload drop glue) and the wait continues. Queue traffic is left
/// untouched: calls are atomic, so the caller resumes handling its
/// mailbox only after the call completes.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_call_receive(
    token: i64,
    out: *mut u8,
    out_cap: i64,
    timeout_ms: i64,
) -> i64 {
    let pid = CURRENT_PID.with(|c| c.get());
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);

    loop {
        let stale = {
            let mut guard = SCHED.lock().unwrap();
            match guard.get_mut(pid).and_then(|p| p.mailbox.take_reply()) {
                Some(envelope) if envelope.reply_token == token => {
                    if let Some(p) = guard.get_mut(pid) {
                        p.deadline = None;
                    }
                    drop(guard);
                    deliver_envelope(envelope, out, out_cap);
                    return 0;
                }
                Some(stale) => Some(stale),
                None => {
                    if Instant::now() >= deadline {
                        if let Some(p) = guard.get_mut(pid) {
                            p.deadline = None;
                        }
                        return -1;
                    }
                    guard.try_park(pid, WaitTarget::Reply, Some(deadline));
                    None
                }
            }
        };

        match stale {
            Some(stale) => drop(stale),
            None => yield_to_scheduler(),
        }
    }
}

/// Returns the PID of the currently executing process on this worker
/// thread. Mapped from the thread-local [`CURRENT_PID`].
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_self() -> i64 {
    CURRENT_PID.with(|c| c.get())
}

/// Sends a business message to the process identified by `pid`.
///
/// Copies `msg_len` bytes from `msg_ptr` into a fresh envelope (tag=0)
/// and routes it to the target's business queue. If the target is
/// parked waiting on its queues, it is promoted to `Runnable` and a
/// worker is woken.
///
/// `drop_glue` (null when the payload owns no nested heap) releases the
/// payload's nested Koja heap if the envelope is ever discarded
/// undelivered (sent-to-dead, mailbox cleared on process death). The
/// delivered-receive path moves the payload into the receiver and frees
/// only the transport buffer (never runs the glue) — see
/// [`crate::wire::Envelope`].
///
/// # Safety
/// `msg_ptr` must point to `msg_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_send(
    pid: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
) {
    let envelope =
        unsafe { Envelope::from_payload(TAG_BUSINESS, msg_ptr, msg_len as usize, drop_glue) };
    deliver_or_discard(pid, envelope);
    WORK_AVAILABLE.notify_one();
}

/// Sends a reply for the in-flight call identified by `token` back to
/// the caller `pid`. Like [`koja_rt_send`] but the envelope is tagged
/// `TAG_REPLY` and routed to the caller's one-shot reply slot, where
/// `koja_rt_call_receive` correlates it by token; it never enters the
/// receive queues. A stale occupant displaced from the slot is dropped
/// here, releasing its payload.
///
/// # Safety
/// `msg_ptr` must point to `msg_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_reply(
    pid: i64,
    token: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
) {
    let mut envelope =
        unsafe { Envelope::from_payload(TAG_REPLY, msg_ptr, msg_len as usize, drop_glue) };
    envelope.reply_token = token;
    deliver_or_discard(pid, envelope);
    WORK_AVAILABLE.notify_one();
}

/// Sends a lifecycle event to the given process, routed to its system
/// queue so `receive` sees it before any queued business traffic.
///
/// Variant indices: 0=Shutdown, 1=Interrupt, 2=Reload.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_send_lifecycle(pid: i64, variant: i64) {
    send_lifecycle_to(pid, variant);
}

/// Sends an IOReady event to the process identified by `pid`.
///
/// Constructs a tagged buffer: tag=2 (IO event), then the IOReady enum
/// layout: variant byte (0=Read, 1=Write, 2=Error) at offset 8, followed
/// by the Fd struct (i64 descriptor) at offset 16. Routed to the
/// target's business queue.
pub fn send_io_event(pid: i64, variant: u8, fd: i64) {
    let buf = unsafe {
        let buf = memory::alloc(IO_READY_BUF_SIZE);
        ptr::write_bytes(buf, 0, IO_READY_BUF_SIZE);
        *buf = TAG_IO_READY;
        *buf.add(IO_READY_VARIANT_OFFSET) = variant;
        *(buf.add(IO_READY_FD_OFFSET) as *mut i64) = fd;
        buf
    };

    deliver_or_discard(pid, Envelope::new(buf, IO_READY_BUF_SIZE));
    WORK_AVAILABLE.notify_one();
}

/// Schedules a message to be delivered to `pid` after `delay_ms`
/// milliseconds. The message is staged as a finished envelope
/// immediately; the worker loop delivers it when the timer fires.
///
/// `drop_glue` (null when the payload owns no nested heap) rides the
/// staged envelope, so an unfired or undeliverable timer releases the
/// payload's nested heap instead of leaking it. See [`koja_rt_send`].
///
/// # Safety
/// `msg_ptr` must point to `msg_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_send_after(
    pid: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    delay_ms: i64,
    drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
) {
    let envelope =
        unsafe { Envelope::from_payload(TAG_BUSINESS, msg_ptr, msg_len as usize, drop_glue) };
    let fire_at = Instant::now() + Duration::from_millis(delay_ms as u64);

    {
        let mut guard = SCHED.lock().unwrap();
        guard.push_timer(fire_at, pid, envelope);
    }

    WORK_AVAILABLE.notify_one();
}

/// Returns 1 if the process with the given PID is still alive (not `Dead`),
/// 0 otherwise. An out-of-range PID returns 0.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_is_process_alive(pid: i64) -> i64 {
    SCHED.lock().unwrap().is_alive(pid) as i64
}

/// Immediately marks a process as `Dead`, reclaiming its stack and
/// initial state. No signal is sent -- the process gets no chance to run
/// cleanup. This is the "last resort" termination primitive.
///
/// If the target is currently executing on another worker (`on_cpu`),
/// all of its resources -- stack, initial state, and any undelivered
/// mailbox envelopes -- are reclaimed by that worker when it switches
/// out; otherwise they are freed here. Either way the [`Reclaim`] drop
/// runs every discarded envelope's payload glue and the init-state
/// config glue, so nested heap is released too.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_kill(pid: i64) {
    let reclaim = SCHED.lock().unwrap().kill(pid);
    drop(reclaim);
}

/// Count of illegal lifecycle edges the scheduler has applied — zero in
/// a correct runtime, in release builds too. The machine oracle for the
/// race-regression lang fixtures (see `tests/lang/memory/`).
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_sched_violations() -> i64 {
    SCHED.lock().unwrap().counters().violations as i64
}

/// Count of parks refused over a kill tombstone. Positive evidence that
/// the kill-vs-park window was actually hit during a storm fixture.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_parks_refused() -> i64 {
    SCHED.lock().unwrap().counters().parks_refused as i64
}

/// Spawns a new lightweight process that will call `fn_ptr(state)`.
///
/// Allocates a stack, copies the initial state onto the heap, and
/// registers the process as `Created`. Wakes an idle worker via
/// [`WORK_AVAILABLE`]. Returns the new process's PID.
///
/// The process owns its config copy: `drop_glue` (null when the config
/// owns no nested heap) runs over it when the process's resources are
/// reclaimed — whether it exited normally, was killed, or never ran.
///
/// # Safety
/// `state_ptr` must point to `state_len` readable bytes (or be null if `state_len` is 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_spawn(
    fn_ptr: ProcessFn,
    state_ptr: *const u8,
    state_len: i64,
    drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
) -> i64 {
    ensure_runtime_init();

    let init_state = if state_len > 0 && !state_ptr.is_null() {
        let len = state_len as usize;
        let buf = memory::alloc(len);
        unsafe { ptr::copy_nonoverlapping(state_ptr, buf, len) };
        OwnedPayload::new(buf, drop_glue)
    } else {
        OwnedPayload::default()
    };

    let (stack, sp) = allocate_process_stack();

    let id = {
        let mut guard = SCHED.lock().unwrap();
        guard.spawn(fn_ptr, init_state, stack, sp)
    };

    WORK_AVAILABLE.notify_one();
    id
}
