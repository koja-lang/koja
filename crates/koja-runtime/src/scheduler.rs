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

// ---------------------------------------------------------------------------
// Mailbox tag and IOReady layout constants
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub(crate) const TAG_BUSINESS: u8 = 0;
pub(crate) const TAG_LIFECYCLE: u8 = 1;
pub(crate) const TAG_IO_READY: u8 = 2;

pub(crate) const TAG_HEADER_SIZE: usize = 8;

pub(crate) const LIFECYCLE_BUF_SIZE: usize = 16;

pub(crate) const IO_READY_BUF_SIZE: usize = 24;
pub(crate) const IO_READY_VARIANT_OFFSET: usize = 8;
pub(crate) const IO_READY_FD_OFFSET: usize = 16;

pub(crate) const IO_READY_READ: u8 = 0;
pub(crate) const IO_READY_WRITE: u8 = 1;
pub(crate) const IO_READY_ERROR: u8 = 2;

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

    {
        let mut guard = SCHED.lock().unwrap();
        let idx = (pid - 1) as usize;
        if idx >= guard.processes.len() {
            return;
        }
        guard.processes[idx].mailbox.push_front(buf);
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
    /// FIFO queue of heap-allocated message buffers delivered via `send`.
    mailbox: VecDeque<*mut u8>,
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
    /// Current lifecycle state, driven by the scheduler and runtime intrinsics.
    pub(crate) state: ProcessState,
}

/// Process contains raw pointers that are heap-allocated and not
/// thread-affine, so cross-thread transfer is safe.
unsafe impl Send for Process {}

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

/// Heap-allocates a [`STACK_SIZE`] byte stack (16-byte aligned) and
/// initialises it so the first context switch lands in
/// [`process_trampoline`].
fn allocate_process_stack() -> *mut u8 {
    unsafe {
        let layout = alloc::Layout::from_size_align(STACK_SIZE, 16).unwrap();
        let base = alloc::alloc(layout);
        if base.is_null() {
            alloc::handle_alloc_error(layout);
        }
        let stack_top = base.add(STACK_SIZE);
        let stack_top = ((stack_top as usize) & !15) as *mut u8;
        init_process_stack(stack_top, process_trampoline)
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
                let idx = (timer.target_pid - 1) as usize;
                if idx < guard.processes.len() {
                    guard.processes[idx].mailbox.push_back(buf);
                    if guard.processes[idx].state == ProcessState::Blocked {
                        guard.processes[idx].state = ProcessState::Runnable;
                    }
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
            if idx < guard.processes.len() {
                guard.processes[idx].sp = saved_sp;
                guard.processes[idx].on_cpu = false;
            }

            if !guard.processes.is_empty() && guard.processes[0].state == ProcessState::Dead {
                SHUTDOWN.store(true, Ordering::Relaxed);
                drop(guard);
                WORK_AVAILABLE.notify_all();
                break;
            }
            drop(guard);
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
        if let Some(msg) = guard.processes[idx].mailbox.pop_front() {
            return msg as *const u8;
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
        .map(|p| p as *const u8)
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
        if let Some(msg) = guard.processes[idx].mailbox.pop_front() {
            return msg as *const u8;
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
        .map(|p| p as *const u8)
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

    {
        let mut guard = SCHED.lock().unwrap();
        let idx = (pid - 1) as usize;
        if idx >= guard.processes.len() {
            return;
        }
        guard.processes[idx].mailbox.push_back(buf);
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

    {
        let mut guard = SCHED.lock().unwrap();
        let idx = (pid - 1) as usize;
        if idx >= guard.processes.len() {
            return;
        }
        guard.processes[idx].mailbox.push_back(buf);
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

/// Immediately marks a process as `Dead`. Its mailbox is drained and
/// its stack is deallocated. No signal is sent -- the process gets no
/// chance to run cleanup. This is the "last resort" termination primitive.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_kill(pid: i64) {
    let mut guard = SCHED.lock().unwrap();
    let idx = (pid - 1) as usize;
    if idx >= guard.processes.len() {
        return;
    }
    let proc = &mut guard.processes[idx];
    if proc.state == ProcessState::Dead {
        return;
    }
    proc.state = ProcessState::Dead;
    proc.mailbox.clear();
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
    let heap_state = if state_len > 0 && !state_ptr.is_null() {
        let len = state_len as usize;
        unsafe {
            let layout = alloc::Layout::from_size_align(len, 8).unwrap();
            let buf = alloc::alloc(layout);
            ptr::copy_nonoverlapping(state_ptr, buf, len);
            buf
        }
    } else {
        ptr::null_mut()
    };

    let sp = allocate_process_stack();

    let id = {
        let mut guard = SCHED.lock().unwrap();
        let id = guard.next_id;
        guard.next_id += 1;
        guard.processes.push(Process {
            deadline: None,
            func: fn_ptr,
            id,
            init_state: heap_state,
            mailbox: VecDeque::new(),
            on_cpu: false,
            sp,
            state: ProcessState::Created,
        });
        id
    };

    WORK_AVAILABLE.notify_one();
    id
}
