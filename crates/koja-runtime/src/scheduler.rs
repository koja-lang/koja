//! Multi-threaded cooperative scheduler for Koja lightweight processes.
//!
//! N worker OS threads share a Mutex-protected process list. Each worker
//! runs a scheduling loop: grab a runnable process, context-switch into it,
//! and when it yields (via `receive`), switch back and look for more work.
//! Idle workers park on a Condvar and are woken by `send` or `spawn`.

use std::alloc;
use std::cell::{Cell, UnsafeCell};
use std::collections::VecDeque;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::ffi::{fflush, koja_context_switch, setvbuf};
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
        let mut sa: libc::sigaction = std::mem::zeroed();
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
        let mut unblock: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut unblock);
        libc::sigaddset(&mut unblock, libc::SIGTERM);
        libc::sigaddset(&mut unblock, libc::SIGINT);
        libc::sigaddset(&mut unblock, libc::SIGHUP);
        libc::sigprocmask(libc::SIG_UNBLOCK, &unblock, ptr::null_mut());
    }
}

/// Checks atomic signal flags and injects lifecycle messages into PID 1's
/// mailbox. Called from the worker loop. Lifecycle variant indices match
/// the `Lifecycle` enum declaration order: Shutdown=0, Interrupt=1, Reload=2.
fn poll_signals() {
    if GOT_SIGTERM.swap(false, Ordering::Relaxed) {
        send_lifecycle_to(1, 0);
    }
    if GOT_SIGINT.swap(false, Ordering::Relaxed) {
        send_lifecycle_to(1, 1);
    }
    if GOT_SIGHUP.swap(false, Ordering::Relaxed) {
        send_lifecycle_to(1, 2);
    }
}

/// Internal helper: allocates a tagged lifecycle message buffer and
/// pushes it to the front of the target process's mailbox.
fn send_lifecycle_to(pid: i64, variant: i64) {
    let buf = unsafe {
        let layout = alloc::Layout::from_size_align(LIFECYCLE_BUF_SIZE, 8).unwrap();
        let buf = alloc::alloc(layout);
        ptr::write_bytes(buf, 0, LIFECYCLE_BUF_SIZE);
        *buf = TAG_LIFECYCLE;
        *buf.add(TAG_HEADER_SIZE) = variant as u8;
        buf
    };

    let envelope = Envelope::new(buf, LIFECYCLE_BUF_SIZE);
    {
        let mut guard = SCHED.lock().unwrap();
        let idx = (pid - 1) as usize;
        if !guard.deliverable(idx) {
            envelope.free();
            return;
        }
        guard.processes[idx].mailbox.push_front(envelope);
        if guard.processes[idx].state == ProcessState::Blocked {
            guard.processes[idx].state = ProcessState::Runnable;
        }
    }

    WORK_AVAILABLE.notify_all();
}

const STACK_SIZE: usize = 512 * 1024;

type ProcessFn = extern "C" fn(*const u8);

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

#[derive(PartialEq)]
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
/// the usable region. Held on each [`Process`] so the mapping can be
/// released when the process dies.
struct ProcessStack {
    /// Base of the whole mapping (start of the guard page).
    base: *mut u8,
    /// Total mapped bytes: guard page + usable stack.
    size: usize,
}

/// A single lightweight Koja process.
///
/// Each process has its own stack, a FIFO mailbox of raw message buffers,
/// and a state machine driven by the scheduler. PIDs start at 1 and map
/// to `Vec` indices as `pid - 1`.
pub(crate) struct Process {
    /// Optional receive timeout. Set by `koja_rt_receive_timeout`, cleared
    /// on resume. The worker loop promotes `Blocked → Runnable` when the
    /// deadline passes.
    deadline: Option<Instant>,
    /// The compiled Koja function to call when first entering this process.
    func: ProcessFn,
    /// Unique process identifier. PIDs start at 1; index is `id - 1`.
    id: i64,
    /// Heap-allocated initial state passed to `func` on first entry.
    init_state: *mut u8,
    /// Byte length of the `init_state` allocation, needed to deallocate
    /// it on death. `0` when `init_state` is null.
    init_state_len: usize,
    /// FIFO queue of message envelopes delivered via `send`.
    mailbox: VecDeque<Envelope>,
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
    on_cpu: bool,
    /// Saved stack pointer. Written by `koja_context_switch` when the
    /// process yields, read when a worker resumes it.
    sp: *mut u8,
    /// The process's `mmap`-backed stack, unmapped when the process dies.
    stack: ProcessStack,
    /// Current lifecycle state, driven by the scheduler and runtime intrinsics.
    pub(crate) state: ProcessState,
}

/// Process contains raw pointers that are heap-allocated and not
/// thread-affine, so cross-thread transfer is safe.
unsafe impl Send for Process {}

impl Process {
    /// Removes a dead process's reclaimable resources from its slot,
    /// nulling/zeroing/emptying them so the actual frees can happen
    /// after the `SCHED` lock is dropped. Idempotent: a second call
    /// returns an empty [`Reclaim`] whose [`Reclaim::free`] is a no-op,
    /// so a kill racing the worker loop reclaims at most once.
    fn take_resources(&mut self) -> Reclaim {
        let reclaim = Reclaim {
            init_state: self.init_state,
            init_state_len: self.init_state_len,
            mailbox: std::mem::take(&mut self.mailbox),
            stack: ProcessStack {
                base: self.stack.base,
                size: self.stack.size,
            },
        };
        self.init_state = ptr::null_mut();
        self.init_state_len = 0;
        self.stack = ProcessStack {
            base: ptr::null_mut(),
            size: 0,
        };
        reclaim
    }
}

/// Resources taken from a dead process, freed once the `SCHED` lock is
/// no longer held. Produced by [`Process::take_resources`].
struct Reclaim {
    init_state: *mut u8,
    init_state_len: usize,
    mailbox: VecDeque<Envelope>,
    stack: ProcessStack,
}

/// Reclaim owns raw pointers detached from a [`Process`]; freeing them
/// off the scheduler thread is sound.
unsafe impl Send for Reclaim {}

impl Reclaim {
    /// Frees every undelivered mailbox envelope, unmaps the stack, and
    /// deallocates `init_state`. All parts are empty/null-safe, so this
    /// is sound on an already-reclaimed `Reclaim`.
    fn free(mut self) {
        for envelope in self.mailbox.drain(..) {
            envelope.free();
        }
        free_process_stack(self.stack);
        if !self.init_state.is_null() && self.init_state_len > 0 {
            unsafe {
                let layout = alloc::Layout::from_size_align(self.init_state_len, 8).unwrap();
                alloc::dealloc(self.init_state, layout);
            }
        }
    }
}

/// A pending delayed message, delivered when `fire_at` has elapsed.
struct Timer {
    fire_at: Instant,
    target_pid: i64,
    msg_buf: *mut u8,
    msg_len: usize,
}

unsafe impl Send for Timer {}

/// Shared scheduler state protected by [`SCHED`].
///
/// All process metadata lives here. Workers lock the Mutex briefly to
/// find/claim a runnable process or update state, then release before
/// performing any context switch.
pub(crate) struct Scheduler {
    /// Monotonically increasing PID counter. Next spawned process gets this ID.
    next_id: i64,
    /// All known processes, indexed by `pid - 1`.
    pub(crate) processes: Vec<Process>,
    /// Pending timers created by `send_after`. Drained by the worker loop.
    timers: Vec<Timer>,
}

impl Scheduler {
    const fn new() -> Self {
        Scheduler {
            next_id: 1,
            processes: Vec::new(),
            timers: Vec::new(),
        }
    }

    /// Whether `idx` names a process that can still receive mail: in
    /// range and not yet `Dead`. Sends to a missing or dead target are
    /// dropped (and their envelopes freed) rather than queued onto a
    /// mailbox that will never be drained again.
    fn deliverable(&self, idx: usize) -> bool {
        idx < self.processes.len() && self.processes[idx].state != ProcessState::Dead
    }

    /// Cancels every pending timer aimed at `pid`, freeing its staged
    /// message buffer. Called when a process dies so a delayed `send`
    /// neither fires onto a dead mailbox nor leaks its staging buffer.
    fn cancel_timers_for(&mut self, pid: i64) {
        self.timers.retain(|timer| {
            if timer.target_pid != pid {
                return true;
            }
            unsafe {
                let layout = alloc::Layout::from_size_align(timer.msg_len, 8).unwrap();
                alloc::dealloc(timer.msg_buf, layout);
            }
            false
        });
    }
}

/// Global scheduler state. Workers hold this lock briefly to find or
/// update processes; the lock is always released before context-switching.
pub(crate) static SCHED: Mutex<Scheduler> = Mutex::new(Scheduler::new());

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

    let (func, init_state) = {
        let guard = SCHED.lock().unwrap();
        let idx = (pid - 1) as usize;
        (guard.processes[idx].func, guard.processes[idx].init_state)
    };

    unsafe {
        func(init_state);
        fflush(ptr::null_mut());
    }

    {
        let mut guard = SCHED.lock().unwrap();
        let idx = (pid - 1) as usize;
        guard.processes[idx].state = ProcessState::Dead;
    }

    WORK_AVAILABLE.notify_all();

    let yield_sp_ptr = YIELD_SP.with(|c| c.get());
    let sched_sp = unsafe { *SCHED_SP.with(|c| c.get()) };
    unsafe {
        koja_context_switch(yield_sp_ptr, sched_sp);
    }
}

/// Maps a fresh [`STACK_SIZE`] process stack with a `PROT_NONE` guard
/// page at the growth (low-address) end and initialises it so the first
/// context switch lands in [`process_trampoline`]. Returns the mapping
/// handle (for later [`free_process_stack`]) plus the initial stack
/// pointer. Aborts on mapping failure, matching the old allocator.
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

/// Releases a stack mapping previously returned by
/// [`allocate_process_stack`]. A null base (a process whose stack was
/// already reclaimed) is a no-op.
fn free_process_stack(stack: ProcessStack) {
    if stack.base.is_null() {
        return;
    }
    unsafe {
        libc::munmap(stack.base as *mut libc::c_void, stack.size);
    }
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

    loop {
        if SHUTDOWN.load(Ordering::Relaxed) {
            break;
        }

        poll_signals();

        let mut guard = SCHED.lock().unwrap();

        let now = Instant::now();
        for proc in guard.processes.iter_mut() {
            if proc.state == ProcessState::Blocked && proc.deadline.is_some_and(|dl| now >= dl) {
                proc.state = ProcessState::Runnable;
            }
        }

        let mut i = 0;
        while i < guard.timers.len() {
            if now >= guard.timers[i].fire_at {
                let timer = guard.timers.swap_remove(i);
                let total = TAG_HEADER_SIZE + timer.msg_len;
                let buf = unsafe {
                    let layout = alloc::Layout::from_size_align(total, 8).unwrap();
                    let buf = alloc::alloc(layout);
                    ptr::write_bytes(buf, 0, TAG_HEADER_SIZE);
                    ptr::copy_nonoverlapping(
                        timer.msg_buf,
                        buf.add(TAG_HEADER_SIZE),
                        timer.msg_len,
                    );
                    alloc::dealloc(
                        timer.msg_buf,
                        alloc::Layout::from_size_align(timer.msg_len, 8).unwrap(),
                    );
                    buf
                };
                let envelope = Envelope::new(buf, total);
                let idx = (timer.target_pid - 1) as usize;
                if guard.deliverable(idx) {
                    guard.processes[idx].mailbox.push_back(envelope);
                    if guard.processes[idx].state == ProcessState::Blocked {
                        guard.processes[idx].state = ProcessState::Runnable;
                    }
                } else {
                    // Target vanished or died between scheduling and firing;
                    // don't push onto a dead mailbox or leak the buffer.
                    envelope.free();
                }
            } else {
                i += 1;
            }
        }

        let found = guard.processes.iter().position(|p| {
            !p.on_cpu && (p.state == ProcessState::Created || p.state == ProcessState::Runnable)
        });

        if let Some(i) = found {
            guard.processes[i].on_cpu = true;
            guard.processes[i].state = ProcessState::Running;
            let pid = guard.processes[i].id;
            let proc_sp = guard.processes[i].sp;
            drop(guard);

            CURRENT_PID.with(|c| c.set(pid));
            unsafe {
                koja_context_switch(sched_sp_ptr, proc_sp);
            }

            let saved_sp = unsafe { *yield_sp_ptr };
            let mut guard = SCHED.lock().unwrap();
            let idx = (pid - 1) as usize;
            // A process that just ran its final switch-out is now `Dead`.
            // Detach its stack + init_state here (under the lock) so the
            // unmap/dealloc can run after the lock is dropped. Safe: the
            // process already switched off its own stack to get here.
            let mut reclaim = None;
            if idx < guard.processes.len() {
                guard.processes[idx].sp = saved_sp;
                guard.processes[idx].on_cpu = false;
                if guard.processes[idx].state == ProcessState::Dead {
                    reclaim = Some(guard.processes[idx].take_resources());
                    guard.cancel_timers_for(pid);
                }
            }

            let shutdown =
                !guard.processes.is_empty() && guard.processes[0].state == ProcessState::Dead;
            if shutdown {
                SHUTDOWN.store(true, Ordering::Relaxed);
            }
            drop(guard);

            if shutdown {
                WORK_AVAILABLE.notify_all();
            }
            if let Some(reclaim) = reclaim {
                reclaim.free();
            }
            if shutdown {
                break;
            }
            continue;
        }

        if !guard.processes.is_empty() && guard.processes[0].state == ProcessState::Dead {
            SHUTDOWN.store(true, Ordering::Relaxed);
            drop(guard);
            WORK_AVAILABLE.notify_all();
            break;
        }

        let any_alive = guard
            .processes
            .iter()
            .any(|p| p.state != ProcessState::Dead);
        if !any_alive {
            SHUTDOWN.store(true, Ordering::Relaxed);
            drop(guard);
            WORK_AVAILABLE.notify_all();
            break;
        }

        let any_active = guard
            .processes
            .iter()
            .any(|p| p.state == ProcessState::Running || p.state == ProcessState::WaitingIo);

        let nearest_deadline = guard
            .processes
            .iter()
            .filter(|p| p.state == ProcessState::Blocked)
            .filter_map(|p| p.deadline)
            .min();
        let nearest_timer = guard.timers.iter().map(|t| t.fire_at).min();
        let nearest = match (nearest_deadline, nearest_timer) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };

        let timeout = if any_active {
            nearest
                .map(|dl| dl.saturating_duration_since(now))
                .unwrap_or(Duration::from_millis(10))
        } else {
            nearest
                .map(|dl| dl.saturating_duration_since(now))
                .unwrap_or(Duration::from_millis(100))
        };
        let _ = WORK_AVAILABLE.wait_timeout(guard, timeout);
    }
}

// ---------------------------------------------------------------------------
// Runtime intrinsics (C ABI)
// ---------------------------------------------------------------------------

/// Called by the compiled Koja program after `main` returns.
///
/// Initializes the I/O reactor, spawns `worker_count() - 1` worker OS
/// threads plus a dedicated reactor thread, and runs the scheduling
/// loop on the current thread. Blocks until all threads finish, i.e.
/// until the main process (PID 1) dies and [`SHUTDOWN`] is set.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_main_done() {
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
}

/// Blocking receive. Returns the first message in the current
/// process's mailbox, or context-switches back to the scheduler
/// (marking the process `Blocked`) until a message arrives.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_receive() -> *const u8 {
    let pid = CURRENT_PID.with(|c| c.get());
    let idx = (pid - 1) as usize;

    {
        let mut guard = SCHED.lock().unwrap();
        if let Some(envelope) = guard.processes[idx].mailbox.pop_front() {
            return envelope.buffer as *const u8;
        }
        guard.processes[idx].state = ProcessState::Blocked;
    }

    let yield_sp_ptr = YIELD_SP.with(|c| c.get());
    let sched_sp = unsafe { *SCHED_SP.with(|c| c.get()) };
    unsafe {
        koja_context_switch(yield_sp_ptr, sched_sp);
    }

    let mut guard = SCHED.lock().unwrap();
    guard.processes[idx]
        .mailbox
        .pop_front()
        .map(|envelope| envelope.buffer as *const u8)
        .unwrap_or(ptr::null())
}

/// Receive with a timeout. Like [`koja_rt_receive`] but sets a
/// deadline `timeout_ms` milliseconds in the future. If no message
/// arrives before the deadline, the worker loop promotes the process
/// back to `Runnable` and this returns null.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_receive_timeout(timeout_ms: i64) -> *const u8 {
    let pid = CURRENT_PID.with(|c| c.get());
    let idx = (pid - 1) as usize;

    {
        let mut guard = SCHED.lock().unwrap();
        if let Some(envelope) = guard.processes[idx].mailbox.pop_front() {
            return envelope.buffer as *const u8;
        }
        guard.processes[idx].state = ProcessState::Blocked;
        guard.processes[idx].deadline =
            Some(Instant::now() + Duration::from_millis(timeout_ms as u64));
    }

    let yield_sp_ptr = YIELD_SP.with(|c| c.get());
    let sched_sp = unsafe { *SCHED_SP.with(|c| c.get()) };
    unsafe {
        koja_context_switch(yield_sp_ptr, sched_sp);
    }

    let mut guard = SCHED.lock().unwrap();
    guard.processes[idx].deadline = None;
    guard.processes[idx]
        .mailbox
        .pop_front()
        .map(|envelope| envelope.buffer as *const u8)
        .unwrap_or(ptr::null())
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
/// # Safety
/// `msg_ptr` must point to `msg_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_send(pid: i64, msg_ptr: *const u8, msg_len: i64) {
    let len = msg_len as usize;
    let total = TAG_HEADER_SIZE + len;
    let buf = unsafe {
        let layout = alloc::Layout::from_size_align(total, 8).unwrap();
        let buf = alloc::alloc(layout);
        ptr::write_bytes(buf, 0, TAG_HEADER_SIZE);
        ptr::copy_nonoverlapping(msg_ptr, buf.add(TAG_HEADER_SIZE), len);
        buf
    };

    let envelope = Envelope::new(buf, total);
    {
        let mut guard = SCHED.lock().unwrap();
        let idx = (pid - 1) as usize;
        if !guard.deliverable(idx) {
            envelope.free();
            return;
        }
        guard.processes[idx].mailbox.push_back(envelope);
        if guard.processes[idx].state == ProcessState::Blocked {
            guard.processes[idx].state = ProcessState::Runnable;
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
        let layout = alloc::Layout::from_size_align(IO_READY_BUF_SIZE, 8).unwrap();
        let buf = alloc::alloc(layout);
        ptr::write_bytes(buf, 0, IO_READY_BUF_SIZE);
        *buf = TAG_IO_READY;
        *buf.add(IO_READY_VARIANT_OFFSET) = variant;
        *(buf.add(IO_READY_FD_OFFSET) as *mut i64) = fd;
        buf
    };

    let envelope = Envelope::new(buf, IO_READY_BUF_SIZE);
    {
        let mut guard = SCHED.lock().unwrap();
        let idx = (pid - 1) as usize;
        if !guard.deliverable(idx) {
            envelope.free();
            return;
        }
        guard.processes[idx].mailbox.push_back(envelope);
        if guard.processes[idx].state == ProcessState::Blocked {
            guard.processes[idx].state = ProcessState::Runnable;
        }
    }

    WORK_AVAILABLE.notify_one();
}

/// Schedules a message to be delivered to `pid` after `delay_ms`
/// milliseconds. The message bytes are copied immediately; the
/// delivery happens in the worker loop when the timer fires.
///
/// # Safety
/// `msg_ptr` must point to `msg_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn koja_rt_send_after(
    pid: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    delay_ms: i64,
) {
    let len = msg_len as usize;
    let msg_copy = unsafe {
        let layout = alloc::Layout::from_size_align(len, 8).unwrap();
        let buf = alloc::alloc(layout);
        ptr::copy_nonoverlapping(msg_ptr, buf, len);
        buf
    };

    let fire_at = Instant::now() + Duration::from_millis(delay_ms as u64);

    {
        let mut guard = SCHED.lock().unwrap();
        guard.timers.push(Timer {
            fire_at,
            target_pid: pid,
            msg_buf: msg_copy,
            msg_len: len,
        });
    }

    WORK_AVAILABLE.notify_one();
}

/// Returns 1 if the process with the given PID is still alive (not `Dead`),
/// 0 otherwise. An out-of-range PID returns 0.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_is_process_alive(pid: i64) -> i64 {
    let guard = SCHED.lock().unwrap();
    let idx = (pid - 1) as usize;
    if idx >= guard.processes.len() {
        return 0;
    }
    if guard.processes[idx].state == ProcessState::Dead {
        0
    } else {
        1
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
        let idx = (pid - 1) as usize;
        if idx >= guard.processes.len() {
            return;
        }
        if guard.processes[idx].state == ProcessState::Dead {
            return;
        }
        guard.processes[idx].state = ProcessState::Dead;
        guard.cancel_timers_for(pid);
        // Reclaiming a stack a worker is still running on would be a
        // use-after-free; in that case let the owning worker reclaim on
        // switch-out (it sees `Dead` after persisting `sp`). The mailbox
        // rides along so its envelopes are freed exactly once.
        if guard.processes[idx].on_cpu {
            None
        } else {
            Some(guard.processes[idx].take_resources())
        }
    };
    if let Some(reclaim) = reclaim {
        reclaim.free();
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
    let heap_state_len = if state_len > 0 && !state_ptr.is_null() {
        state_len as usize
    } else {
        0
    };
    let heap_state = if heap_state_len > 0 {
        unsafe {
            let layout = alloc::Layout::from_size_align(heap_state_len, 8).unwrap();
            let buf = alloc::alloc(layout);
            ptr::copy_nonoverlapping(state_ptr, buf, heap_state_len);
            buf
        }
    } else {
        ptr::null_mut()
    };

    let (stack, sp) = allocate_process_stack();

    let id = {
        let mut guard = SCHED.lock().unwrap();
        let id = guard.next_id;
        guard.next_id += 1;
        guard.processes.push(Process {
            deadline: None,
            func: fn_ptr,
            id,
            init_state: heap_state,
            init_state_len: heap_state_len,
            mailbox: VecDeque::new(),
            on_cpu: false,
            sp,
            stack,
            state: ProcessState::Created,
        });
        id
    };

    WORK_AVAILABLE.notify_one();
    id
}
