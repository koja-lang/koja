//! Multi-threaded cooperative scheduler for Koja lightweight processes.
//!
//! N worker OS threads share a Mutex-protected process list. Each worker
//! runs a scheduling loop: grab a runnable process, context-switch into it,
//! and when it yields (via `receive`), switch back and look for more work.
//! Idle workers park on a Condvar and are woken by `send` or `spawn`.

use std::alloc;
use std::cell::{Cell, UnsafeCell};
use std::collections::VecDeque;
use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, Once};
use std::thread;
use std::time::{Duration, Instant};

use crate::ffi::{fflush, koja_context_switch, setvbuf};
use crate::memory;
use crate::process_table::ProcessTable;
use crate::tsan;
use crate::wire::{
    Envelope, IO_READY_BUF_SIZE, IO_READY_FD_OFFSET, IO_READY_VARIANT_OFFSET, LIFECYCLE_BUF_SIZE,
    TAG_HEADER_SIZE, TAG_IO_READY, TAG_LIFECYCLE,
};

// ---------------------------------------------------------------------------
// Signal handling state
// ---------------------------------------------------------------------------

static GOT_SIGTERM: AtomicBool = AtomicBool::new(false);
static GOT_SIGINT: AtomicBool = AtomicBool::new(false);
static GOT_SIGHUP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigterm(_sig: libc::c_int) {
    GOT_SIGTERM.store(true, Ordering::Relaxed);
}

extern "C" fn handle_sigint(_sig: libc::c_int) {
    GOT_SIGINT.store(true, Ordering::Relaxed);
}

extern "C" fn handle_sighup(_sig: libc::c_int) {
    GOT_SIGHUP.store(true, Ordering::Relaxed);
}

fn install_signals() {
    unsafe {
        let mut sa: libc::sigaction = mem::zeroed();
        sa.sa_flags = 0;
        libc::sigemptyset(&mut sa.sa_mask);

        sa.sa_sigaction = handle_sigterm as *const () as usize;
        libc::sigaction(libc::SIGTERM, &sa, ptr::null_mut());

        sa.sa_sigaction = handle_sigint as *const () as usize;
        libc::sigaction(libc::SIGINT, &sa, ptr::null_mut());

        sa.sa_sigaction = handle_sighup as *const () as usize;
        libc::sigaction(libc::SIGHUP, &sa, ptr::null_mut());

        // Unblock these signals in case the parent process (e.g. cargo test
        // linking LLVM) inherited a mask that blocks them.
        let mut unblock: libc::sigset_t = mem::zeroed();
        libc::sigemptyset(&mut unblock);
        libc::sigaddset(&mut unblock, libc::SIGTERM);
        libc::sigaddset(&mut unblock, libc::SIGINT);
        libc::sigaddset(&mut unblock, libc::SIGHUP);
        libc::sigprocmask(libc::SIG_UNBLOCK, &unblock, ptr::null_mut());
    }
}

/// Checks atomic signal flags and injects lifecycle messages into the main
/// process's mailbox. Called from the worker loop. Lifecycle variant indices
/// match the `Lifecycle` enum declaration order: Shutdown=0, Interrupt=1,
/// Reload=2. Only takes the lock when a signal actually fired.
fn poll_signals() {
    let term = GOT_SIGTERM.swap(false, Ordering::Relaxed);
    let int = GOT_SIGINT.swap(false, Ordering::Relaxed);
    let hup = GOT_SIGHUP.swap(false, Ordering::Relaxed);
    if !(term || int || hup) {
        return;
    }

    let main_pid = SCHED.lock().unwrap().main_pid();
    if term {
        send_lifecycle_to(main_pid, 0);
    }
    if int {
        send_lifecycle_to(main_pid, 1);
    }
    if hup {
        send_lifecycle_to(main_pid, 2);
    }
}

/// Internal helper: allocates a tagged lifecycle message buffer and
/// pushes it to the front of the target process's mailbox.
fn send_lifecycle_to(pid: i64, variant: i64) {
    let buf = unsafe {
        let buf = memory::alloc(LIFECYCLE_BUF_SIZE);
        ptr::write_bytes(buf, 0, LIFECYCLE_BUF_SIZE);
        *buf = TAG_LIFECYCLE;
        *buf.add(TAG_HEADER_SIZE) = variant as u8;
        buf
    };

    let envelope = Envelope::new(buf, LIFECYCLE_BUF_SIZE);
    {
        let mut guard = SCHED.lock().unwrap();
        if let Some(envelope) = guard.deliver_front(pid, envelope) {
            drop(envelope);
            return;
        }
    }

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

/// An owned byte buffer from the allocator funnel ([`memory::alloc`]),
/// freed on drop. Wraps the scheduler's hand-managed heap runs — a
/// process's initial state and a pending timer's staged message bytes —
/// so they reclaim by RAII rather than a matching manual free on every
/// path.
///
/// The empty value (`Default`) is null and drops as a no-op, so it also
/// serves as the placeholder left behind when ownership is moved out
/// via [`mem::take`].
pub(crate) struct OwnedBuf {
    ptr: *mut u8,
}

impl OwnedBuf {
    /// Wraps an allocation from [`memory::alloc`], or null for empty.
    pub(crate) fn new(ptr: *mut u8) -> Self {
        Self { ptr }
    }
}

impl Default for OwnedBuf {
    fn default() -> Self {
        Self {
            ptr: ptr::null_mut(),
        }
    }
}

impl Drop for OwnedBuf {
    fn drop(&mut self) {
        unsafe { memory::free(self.ptr) };
    }
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
/// Each process has its own stack, a FIFO mailbox of raw message buffers,
/// and a state machine driven by the scheduler. Processes live in a
/// generational slotmap ([`ProcessTable`]); a PID packs the slot index and
/// generation rather than being a bare `Vec` offset.
pub(crate) struct Process {
    /// Optional receive timeout. Set by `koja_rt_receive_timeout`, cleared
    /// on resume. The worker loop promotes `Blocked → Runnable` when the
    /// deadline passes.
    pub(crate) deadline: Option<Instant>,
    /// The compiled Koja function to call when first entering this process.
    func: ProcessFn,
    /// Heap-allocated initial state passed to `func` on first entry,
    /// owned by the process and freed when its resources are reclaimed.
    init_state: OwnedBuf,
    /// FIFO queue of message envelopes delivered via `send`.
    pub(crate) mailbox: VecDeque<Envelope>,
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
}

/// Process contains raw pointers that are heap-allocated and not
/// thread-affine, so cross-thread transfer is safe.
unsafe impl Send for Process {}

impl Process {
    /// Builds a freshly spawned process in the `Created` state. Called by
    /// [`ProcessTable::spawn`], which owns the slot/PID assignment.
    pub(crate) fn new(
        func: ProcessFn,
        init_state: OwnedBuf,
        stack: ProcessStack,
        sp: *mut u8,
    ) -> Self {
        Process {
            deadline: None,
            func,
            init_state,
            mailbox: VecDeque::new(),
            on_cpu: false,
            sp,
            stack,
            state: ProcessState::Created,
        }
    }

    /// The entry function and its heap-allocated initial state, read by
    /// [`process_trampoline`] on first entry.
    pub(crate) fn entry(&self) -> (ProcessFn, *const u8) {
        (self.func, self.init_state.ptr)
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
/// envelope's drop glue), unmaps the stack, and frees `init_state`; an
/// already-reclaimed `Reclaim` holds empty owners and drops as a no-op.
///
/// The fields are never read by name — they exist purely so their own
/// `Drop` runs at this controlled point — hence `allow(dead_code)`.
#[allow(dead_code)]
pub(crate) struct Reclaim {
    init_state: OwnedBuf,
    mailbox: VecDeque<Envelope>,
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

    {
        let mut guard = SCHED.lock().unwrap();
        guard.transition(pid, ProcessState::Dead);
    }

    WORK_AVAILABLE.notify_all();

    tsan::switch_to_scheduler();
    let yield_sp_ptr = YIELD_SP.with(|c| c.get());
    let sched_sp = unsafe { *SCHED_SP.with(|c| c.get()) };
    unsafe {
        koja_context_switch(yield_sp_ptr, sched_sp);
    }
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
/// On Linux, reads cgroup v2 CPU quota (`/sys/fs/cgroup/cpu.max`) so a
/// container with `cpu: 2` on a 96-core host only spawns 2 workers.
/// Falls back to [`std::thread::available_parallelism`] on macOS and
/// bare-metal Linux.
fn worker_count() -> usize {
    #[cfg(target_os = "linux")]
    {
        if let Ok(contents) = std::fs::read_to_string("/sys/fs/cgroup/cpu.max") {
            let parts: Vec<&str> = contents.trim().split_whitespace().collect();
            if parts.len() == 2 && parts[0] != "max" {
                if let (Ok(quota), Ok(period)) = (parts[0].parse::<u64>(), parts[1].parse::<u64>())
                {
                    if period > 0 {
                        let cpus = (quota / period).max(1).min(256) as usize;
                        return cpus;
                    }
                }
            }
        }
    }
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
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

/// Delivers every timer due at `now`. Each fired timer's staged payload is
/// copied into a fresh envelope and handed to the target; an undeliverable
/// timer (target gone or dead) drops its envelope. Dropping each drained
/// [`TimerEntry`] frees its staging buffer.
fn fire_due_timers(table: &mut ProcessTable, now: Instant) {
    for entry in table.take_due_timers(now) {
        let total = TAG_HEADER_SIZE + entry.msg_len;
        let buf = unsafe {
            let buf = memory::alloc(total);
            ptr::write_bytes(buf, 0, TAG_HEADER_SIZE);
            ptr::copy_nonoverlapping(entry.msg.ptr, buf.add(TAG_HEADER_SIZE), entry.msg_len);
            buf
        };
        let mut envelope = Envelope::new(buf, total);
        envelope.drop_glue = entry.drop_glue;
        if let Some(envelope) = table.deliver_back(entry.target_pid, envelope) {
            drop(envelope);
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

    install_signals();
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
/// process `Blocked`) until a message arrives. Returns `-1` only if
/// woken with an empty mailbox.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_receive(out: *mut u8, out_cap: i64) -> i64 {
    let pid = CURRENT_PID.with(|c| c.get());

    {
        let mut guard = SCHED.lock().unwrap();
        let popped = guard.get_mut(pid).and_then(|p| p.mailbox.pop_front());
        if let Some(envelope) = popped {
            drop(guard);
            return deliver_envelope(envelope, out, out_cap);
        }
        guard.transition(pid, ProcessState::Blocked);
    }

    tsan::switch_to_scheduler();
    let yield_sp_ptr = YIELD_SP.with(|c| c.get());
    let sched_sp = unsafe { *SCHED_SP.with(|c| c.get()) };
    unsafe {
        koja_context_switch(yield_sp_ptr, sched_sp);
    }

    let envelope = {
        let mut guard = SCHED.lock().unwrap();
        guard.get_mut(pid).and_then(|p| p.mailbox.pop_front())
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
        let popped = guard.get_mut(pid).and_then(|p| p.mailbox.pop_front());
        if let Some(envelope) = popped {
            drop(guard);
            return deliver_envelope(envelope, out, out_cap);
        }
        guard.transition(pid, ProcessState::Blocked);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
        if let Some(p) = guard.get_mut(pid) {
            p.deadline = Some(deadline);
        }
        guard.push_deadline(pid, deadline);
    }

    tsan::switch_to_scheduler();
    let yield_sp_ptr = YIELD_SP.with(|c| c.get());
    let sched_sp = unsafe { *SCHED_SP.with(|c| c.get()) };
    unsafe {
        koja_context_switch(yield_sp_ptr, sched_sp);
    }

    let envelope = {
        let mut guard = SCHED.lock().unwrap();
        guard.get_mut(pid).and_then(|p| {
            p.deadline = None;
            p.mailbox.pop_front()
        })
    };
    envelope.map_or(-1, |envelope| deliver_envelope(envelope, out, out_cap))
}

/// Returns the PID of the currently executing process on this worker
/// thread. Mapped from the thread-local [`CURRENT_PID`].
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_self() -> i64 {
    CURRENT_PID.with(|c| c.get())
}

/// Sends a message to the process identified by `pid`.
///
/// Copies `msg_len` bytes from `msg_ptr` into a heap-allocated buffer
/// with an 8-byte tag header (tag=0 for business messages). The payload
/// starts at offset 8 in the buffer. Appends to the target's mailbox
/// via `push_back`. If the target is `Blocked`, it is promoted to
/// `Runnable` and a worker is woken.
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
    let len = msg_len as usize;
    let total = TAG_HEADER_SIZE + len;
    let buf = unsafe {
        let buf = memory::alloc(total);
        ptr::write_bytes(buf, 0, TAG_HEADER_SIZE);
        ptr::copy_nonoverlapping(msg_ptr, buf.add(TAG_HEADER_SIZE), len);
        buf
    };

    let mut envelope = Envelope::new(buf, total);
    envelope.drop_glue = drop_glue;
    {
        let mut guard = SCHED.lock().unwrap();
        if let Some(envelope) = guard.deliver_back(pid, envelope) {
            drop(envelope);
            return;
        }
    }

    WORK_AVAILABLE.notify_one();
}

/// Sends a lifecycle event to the given process. Allocates a tagged
/// buffer with tag=1 (lifecycle) and the variant byte, inserted at
/// the front of the mailbox for priority delivery.
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
/// by the Fd struct (i64 descriptor) at offset 16. Appends to the target's
/// mailbox via `push_back`.
pub fn send_io_event(pid: i64, variant: u8, fd: i64) {
    let buf = unsafe {
        let buf = memory::alloc(IO_READY_BUF_SIZE);
        ptr::write_bytes(buf, 0, IO_READY_BUF_SIZE);
        *buf = TAG_IO_READY;
        *buf.add(IO_READY_VARIANT_OFFSET) = variant;
        *(buf.add(IO_READY_FD_OFFSET) as *mut i64) = fd;
        buf
    };

    let envelope = Envelope::new(buf, IO_READY_BUF_SIZE);
    {
        let mut guard = SCHED.lock().unwrap();
        if let Some(envelope) = guard.deliver_back(pid, envelope) {
            drop(envelope);
            return;
        }
    }

    WORK_AVAILABLE.notify_one();
}

/// Schedules a message to be delivered to `pid` after `delay_ms`
/// milliseconds. The message bytes are copied immediately; the
/// delivery happens in the worker loop when the timer fires.
///
/// `drop_glue` (null when the payload owns no nested heap) rides the
/// timer entry onto the fired envelope, so an undeliverable fire
/// (target gone) releases the payload's nested heap instead of leaking
/// it. See [`koja_rt_send`].
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
    let len = msg_len as usize;
    let msg_copy = unsafe {
        let buf = memory::alloc(len);
        ptr::copy_nonoverlapping(msg_ptr, buf, len);
        buf
    };

    let fire_at = Instant::now() + Duration::from_millis(delay_ms as u64);

    {
        let mut guard = SCHED.lock().unwrap();
        guard.push_timer(fire_at, pid, OwnedBuf::new(msg_copy), len, drop_glue);
    }

    WORK_AVAILABLE.notify_one();
}

/// Returns 1 if the process with the given PID is still alive (not `Dead`),
/// 0 otherwise. An out-of-range PID returns 0.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_is_process_alive(pid: i64) -> i64 {
    let guard = SCHED.lock().unwrap();
    match guard.get(pid) {
        Some(process) if process.state != ProcessState::Dead => 1,
        _ => 0,
    }
}

/// Immediately marks a process as `Dead`, reclaiming its stack and
/// initial state. No signal is sent -- the process gets no chance to run
/// cleanup. This is the "last resort" termination primitive.
///
/// If the target is currently executing on another worker (`on_cpu`),
/// all of its resources -- stack, initial state, and any undelivered
/// mailbox envelopes -- are reclaimed by that worker when it switches
/// out; otherwise they are freed here. Nested heap inside a discarded
/// payload is not yet freed (it waits on the deferred drop-glue work;
/// see the message/envelope-lifecycle design).
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_kill(pid: i64) {
    let reclaim = {
        let mut guard = SCHED.lock().unwrap();
        match guard.get(pid) {
            Some(process) if process.state != ProcessState::Dead => {}
            _ => return,
        }
        guard.transition(pid, ProcessState::Dead);
        // Reclaiming a stack a worker is still running on would be a
        // use-after-free; in that case let the owning worker reclaim on
        // switch-out (it sees `Dead` after persisting `sp`). The mailbox
        // rides along so its envelopes are freed exactly once.
        if guard.get(pid).is_some_and(|process| process.on_cpu) {
            None
        } else {
            guard.free(pid)
        }
    };
    if let Some(reclaim) = reclaim {
        drop(reclaim);
    }
}

/// Spawns a new lightweight process that will call `fn_ptr(state)`.
///
/// Allocates a stack, copies the initial state onto the heap, and
/// registers the process as `Created`. Wakes an idle worker via
/// [`WORK_AVAILABLE`]. Returns the new process's PID.
///
/// # Safety
/// `state_ptr` must point to `state_len` readable bytes (or be null if `state_len` is 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_spawn(
    fn_ptr: ProcessFn,
    state_ptr: *const u8,
    state_len: i64,
) -> i64 {
    ensure_runtime_init();

    let heap_state_len = if state_len > 0 && !state_ptr.is_null() {
        state_len as usize
    } else {
        0
    };
    let heap_state = if heap_state_len > 0 {
        unsafe {
            let buf = memory::alloc(heap_state_len);
            ptr::copy_nonoverlapping(state_ptr, buf, heap_state_len);
            buf
        }
    } else {
        ptr::null_mut()
    };

    let (stack, sp) = allocate_process_stack();

    let id = {
        let mut guard = SCHED.lock().unwrap();
        guard.spawn(fn_ptr, OwnedBuf::new(heap_state), stack, sp)
    };

    WORK_AVAILABLE.notify_one();
    id
}
