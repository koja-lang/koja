//! Multi-threaded cooperative scheduler for Koja lightweight processes.
//!
//! N worker OS threads share the internally synchronized [`TABLE`]
//! (per-slot locks and CAS lifecycle edges; no global scheduler lock, see
//! `design/SCHEDULER-PROTOCOL.md`). Each worker runs a scheduling loop:
//! claim a runnable process, context-switch into it, and when it yields
//! (via `receive`), switch back and look for more work. Newly-runnable
//! processes route directly to the work-stealing deques ([`route_wake`]).
//! Idle workers park on a sleeper-counted condvar and are woken by `send`
//! or `spawn`.

use std::alloc;
use std::cell::{Cell, RefCell, UnsafeCell};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, Ordering};
use std::sync::{Condvar, LazyLock, Mutex, Once, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use koja_runtime_core::{
    Clock, CrashInfo, Driver, Due, Executor, ExitNotice, ExitReason, Lifecycle, MailPark, Pid,
    Priority, ProcessTable, Reclaim, SignalSource, SwitchOutcome, TimerService, Wake,
    duration_from_user_millis, slot_index,
};

use crate::ffi::{fflush, koja_context_switch, koja_process_start, koja_seed_reductions, setvbuf};
use crate::memory;
use crate::panic;
use crate::reactor;
use crate::signals;
use crate::tsan;
use crate::util::{BLOCK_HEADER_SIZE, alloc_koja_string, koja_rc_dec};
use crate::wire::{
    EXIT_SIGNAL_BACKTRACE_OFFSET, EXIT_SIGNAL_BUF_SIZE, EXIT_SIGNAL_MESSAGE_OFFSET,
    EXIT_SIGNAL_PID_OFFSET, EXIT_SIGNAL_REASON_OFFSET, Envelope, IO_READY_BUF_SIZE,
    IO_READY_FD_OFFSET, IO_READY_VARIANT_OFFSET, LIFECYCLE_BUF_SIZE, OwnedPayload, TAG_BUSINESS,
    TAG_EXIT_SIGNAL, TAG_HEADER_SIZE, TAG_IO_READY, TAG_LIFECYCLE, TAG_REPLY,
};

/// The native process table: a generational slotmap of [`NativeExecution`]
/// execution states carrying byte [`Envelope`] messages.
pub(crate) type NativeTable = ProcessTable<NativeExecution, Envelope>;

/// Checks the shared signal flags ([`crate::signals`]) and injects
/// lifecycle messages into the main process's system queue. Called
/// from the worker loop. Only takes the lock when a signal actually
/// fired.
fn poll_signals() {
    let fired = NativeSignals.drain();
    if fired.is_empty() {
        return;
    }

    // SIGTERM (`Shutdown`) starts the drain: refuse new spawns. The
    // signal is still delivered to main below so a lifecycle-aware
    // program can shut itself down before the grace deadline.
    // `enter_draining` switched exactly once, so the grace window is
    // measured from the first SIGTERM.
    if fired.contains(&Lifecycle::Shutdown) && TABLE.enter_draining() {
        TIMERS.with(|timers| timers.arm_grace(NativeClock.now() + grace_period()));
    }
    let main_pid = TABLE.main_pid();
    for lifecycle in fired {
        send_lifecycle_to(main_pid, lifecycle as i64);
    }
}

/// The SIGTERM drain grace window, from `KOJA_GRACE_MS` (default 30s, to
/// match Kubernetes' `terminationGracePeriodSeconds`). After this elapses,
/// the worker loop force-kills any straggler. Read in the adapter so the
/// core driver stays env-free.
fn grace_period() -> Duration {
    const DEFAULT_GRACE_MS: u64 = 30_000;
    let millis = std::env::var("KOJA_GRACE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_GRACE_MS);
    Duration::from_millis(millis)
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
}

const STACK_SIZE: usize = 512 * 1024;

/// A compiled process body, entered on first switch. `extern "C-unwind"`
/// because a user crash inside the body unwinds back through it to the
/// [`catch_unwind`](std::panic::catch_unwind) at [`process_trampoline`].
/// The calling convention is unchanged, only that an unwind may legally
/// propagate across the call.
pub(crate) type ProcessFn = extern "C-unwind" fn(*const u8);

//
// The offsets mirror the save/restore layout in `src/arch/*.s`.
// `RET_ADDR_OFFSET` is where the first switch's `ret` reads its target
// (always `koja_process_start`). `ENTRY_REG_OFFSET` is the slot of the
// callee-saved register (`x19` / `rbx`) the shim dispatches through.

// arm64 (macOS and Linux)
#[cfg(target_arch = "aarch64")]
const INIT_FRAME_SIZE: usize = 160;
#[cfg(target_arch = "aarch64")]
const RET_ADDR_OFFSET: usize = 88;
#[cfg(target_arch = "aarch64")]
const ENTRY_REG_OFFSET: usize = 0;

// x86_64 (SysV ABI)
#[cfg(target_arch = "x86_64")]
const INIT_FRAME_SIZE: usize = 64;
#[cfg(target_arch = "x86_64")]
const RET_ADDR_OFFSET: usize = 48;
#[cfg(target_arch = "x86_64")]
const ENTRY_REG_OFFSET: usize = 40;

/// An `mmap`-backed process stack: a `PROT_NONE` guard page at the
/// lowest address (the growth end, since stacks grow down) followed by
/// the usable region. Held on each [`NativeExecution`] so the mapping is
/// `munmap`ped on drop when the process's resources are reclaimed.
pub(crate) struct ProcessStack {
    /// Base of the whole mapping (start of the guard page).
    base: *mut u8,
    /// Total mapped bytes: guard page + usable stack.
    size: usize,
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

/// A native process's execution state: everything the native
/// [`Executor`](koja_runtime_core::Executor) needs to enter and resume one
/// process. Stored opaquely in the process's table slot, owned by the
/// slot's `on_cpu` claim holder. The scheduling policy in
/// `koja-runtime-core` never inspects it.
///
/// Dropping an execution state unmaps its `stack` and releases its `init_state`
/// (running the config's drop glue), which is how a reclaimed process frees
/// its native resources.
pub(crate) struct NativeExecution {
    /// The compiled Koja function to call when first entering this process.
    func: ProcessFn,
    /// Heap-allocated initial state passed to `func` on first entry. Owned by
    /// the process: its payload drop glue runs (releasing the config's nested
    /// heap) when the execution state is dropped.
    init_state: OwnedPayload,
    /// Saved stack pointer. Written by `koja_context_switch` when the process
    /// yields, read when a worker resumes it.
    pub(crate) sp: *mut u8,
    /// The process's `mmap`-backed stack. Never read by name, held purely so
    /// its [`Drop`] `munmap`s the mapping when the execution state is reclaimed.
    #[allow(dead_code)]
    stack: ProcessStack,
}

/// `NativeExecution` holds raw pointers that are heap-allocated and not
/// thread-affine, so cross-thread transfer is safe. This is what makes the
/// concrete `ProcessTable<NativeExecution, Envelope>` `Sync` for the
/// [`TABLE`] static (the table publishes execution state to its single
/// claim holder via the lifecycle word's release/acquire pair).
unsafe impl Send for NativeExecution {}

impl NativeExecution {
    /// Builds the execution state for a freshly spawned process.
    fn new(func: ProcessFn, init_state: OwnedPayload, stack: ProcessStack, sp: *mut u8) -> Self {
        Self {
            func,
            init_state,
            sp,
            stack,
        }
    }

    /// The entry function and its heap-allocated initial state, read by
    /// [`process_trampoline`] on first entry.
    fn entry(&self) -> (ProcessFn, *const u8) {
        (self.func, self.init_state.as_ptr())
    }
}

/// The native [`Executor`]: stackful processes switched via the
/// `koja_context_switch` assembly. A zero-sized handle: all per-process
/// state lives in the [`NativeExecution`] stored in the table, and all
/// per-worker state in this thread's [`SCHED_SP`] / [`YIELD_SP`] /
/// [`CURRENT_PID`] thread-locals.
pub(crate) struct NativeExecutor;

impl Executor for NativeExecutor {
    /// The saved stack pointer: read from the table before the switch,
    /// written back after. The whole point of [`Continuation`] being a
    /// bare `Copy` pointer is that the driver marshals it without holding
    /// a borrow into the table across the switch.
    type Continuation = *mut u8;
    type Execution = NativeExecution;
    type Message = Envelope;

    /// Switches onto `pid`'s stack at `continuation` (its saved `sp`) and
    /// runs until the process yields back via [`yield_to_scheduler`], then
    /// returns the `sp` it yielded at. This touches only thread-locals and
    /// the assembly, never the table, so no lock is held across the switch.
    fn resume(&self, pid: Pid, continuation: Self::Continuation) -> Self::Continuation {
        CURRENT_PID.with(|c| c.set(pid));
        tsan::switch_to_process(tsan::slot_fiber(slot_index(pid)));
        let sched_sp_ptr = SCHED_SP.with(|c| c.get());
        let yield_sp_ptr = YIELD_SP.with(|c| c.get());
        unsafe {
            koja_context_switch(sched_sp_ptr, continuation);
            *yield_sp_ptr
        }
    }
}

/// The native [`Clock`]: the OS monotonic clock, used by the driver for
/// deadline promotion, timer firing, and idle-park sizing.
pub(crate) struct NativeClock;

impl Clock for NativeClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// The native [`SignalSource`]: latches SIGTERM / SIGINT / SIGHUP via the
/// process-wide handlers in [`crate::signals`] and drains them into
/// [`Lifecycle`] events on the driver's schedule.
pub(crate) struct NativeSignals;

impl SignalSource for NativeSignals {
    fn install(&self) {
        signals::install();
    }

    fn drain(&self) -> Vec<Lifecycle> {
        signals::drain()
            .into_iter()
            .map(lifecycle_from_index)
            .collect()
    }
}

/// Maps a drained signal's wire index back to its [`Lifecycle`] variant.
/// The indices (`0 -> Shutdown`, `1 -> Interrupt`, `2 -> Reload`) are a
/// pinned wire ABI contract shared with the signal handler.
fn lifecycle_from_index(index: i64) -> Lifecycle {
    match index {
        0 => Lifecycle::Shutdown,
        1 => Lifecycle::Interrupt,
        _ => Lifecycle::Reload,
    }
}

/// Global scheduler state: the internally synchronized process table.
/// Every access is `&TABLE`; the table's own per-slot and registry locks
/// are always released before context-switching.
pub(crate) static TABLE: NativeTable = ProcessTable::new();

/// The timer service behind its own lock, gated by an atomic next-fire
/// instant so the worker loop's per-iteration timer check is a single
/// relaxed load instead of a lock acquisition.
///
/// Lock-order rule: this lock and the table's core locks are never held
/// together. Firing drains under this lock, releases it, then applies
/// against [`TABLE`], where [`ProcessTable::promote_expired`]
/// re-validates each wake against the live process state.
struct Timers {
    /// Reference instant for the gate encoding, fixed at first use.
    epoch: Instant,
    /// Nanoseconds from `epoch` to the next fire, `u64::MAX` when
    /// nothing is pending. Refreshed under the service lock on every
    /// mutation ([`Self::with`]), read without it.
    gate: AtomicU64,
    service: Mutex<TimerService<Envelope>>,
}

impl Timers {
    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            gate: AtomicU64::new(u64::MAX),
            service: Mutex::new(TimerService::new()),
        }
    }

    /// Runs `f` under the service lock, refreshing the gate before
    /// release so a due entry is never hidden behind a stale gate.
    fn with<T>(&self, f: impl FnOnce(&mut TimerService<Envelope>) -> T) -> T {
        let mut service = self.service.lock().unwrap();
        let out = f(&mut service);
        let gate = service
            .next_fire()
            .map_or(u64::MAX, |at| self.nanos_since_epoch(at));
        self.gate.store(gate, Ordering::Relaxed);
        out
    }

    /// Whether anything is due at `now`, the lock-free worker-loop check.
    fn due(&self, now: Instant) -> bool {
        let gate = self.gate.load(Ordering::Relaxed);
        gate != u64::MAX && self.nanos_since_epoch(now) >= gate
    }

    /// The next fire instant, for sizing an idle park. Reads the gate,
    /// no lock.
    fn next_fire(&self) -> Option<Instant> {
        match self.gate.load(Ordering::Relaxed) {
            u64::MAX => None,
            nanos => Some(self.epoch + Duration::from_nanos(nanos)),
        }
    }

    fn nanos_since_epoch(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.epoch).as_nanos() as u64
    }
}

/// Timers, deadlines, and the drain-grace backstop, off the scheduling
/// hot path.
static TIMERS: LazyLock<Timers> = LazyLock::new(Timers::new);

/// Where idle workers park: a sleeper count under a small mutex plus its
/// condvar. Woken by `koja_rt_send`, `koja_rt_spawn`, the reactor, and on
/// shutdown.
///
/// The count is what keeps the wake path cheap: a producer that woke
/// nobody-parked skips the syscall entirely, and [`notify_workers`] only
/// notifies when a sleeper is actually registered. The lost-wakeup guard
/// is the mutex: a parker re-checks the injectors under the hold before
/// waiting, and a producer pushes its work *before* taking the hold to
/// notify, so the check and the notify cannot interleave the wrong way.
struct IdlePark {
    sleepers: Mutex<usize>,
    wakeup: Condvar,
}

static IDLE_PARK: IdlePark = IdlePark {
    sleepers: Mutex::new(0),
    wakeup: Condvar::new(),
};

/// Set to `true` when the runtime should tear down. Once true, all
/// workers and the reactor thread exit their loops and join.
pub(crate) static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// OS exit code to force when the runtime joins, or `-1` for "no override"
/// (let `main` return its normal code). Set when the entry process (PID 1)
/// crashes, because a crash unwinds rather than returning a `StopReason`,
/// so the normal exit-code plumbing (`__koja_exit_code` for programs,
/// hardcoded `0` for scripts) would report success for a crashed program.
/// [`koja_rt_main_done`] honours it after the driver loop joins.
static ENTRY_EXIT_OVERRIDE: AtomicI32 = AtomicI32::new(-1);

/// The OS exit code for an entry-process crash. Non-zero so a crashed
/// program is observably a failure (mechanical check: PID 1 panic exits
/// non-zero).
const CRASH_EXIT_CODE: i32 = 1;

/// Scheduling priority levels, mirroring `koja_runtime_core::Priority`
/// (`Low`, `Normal`, `High`). One ready deque/injector per level.
const READY_LEVELS: usize = 3;

/// One worker thread's local run queues, one FIFO deque per priority. The
/// worker pushes its own continuations here (co-location) and pops from
/// here first. Siblings steal from the paired [`WorkerStealers`].
type WorkerQueues = [Worker<Pid>; READY_LEVELS];

/// The steal handles for one worker's [`WorkerQueues`], registered in
/// [`STEALERS`] so peers can steal across priority levels.
type WorkerStealers = [Stealer<Pid>; READY_LEVELS];

/// Per-priority global run queues: the overflow/cross-thread landing zone
/// for newly-runnable PIDs ([`route_wake`]). Workers drain these
/// (highest priority first) only after their local deques and stealing
/// come up empty. Lock-free, so any thread can push.
static INJECTORS: LazyLock<[Injector<Pid>; READY_LEVELS]> =
    LazyLock::new(|| [Injector::new(), Injector::new(), Injector::new()]);

/// Every worker's steal handles, indexed by worker id. Published once by
/// [`NativeDriver::run`] before any worker starts, then read-only, so the
/// steal path needs no lock.
static STEALERS: OnceLock<Vec<WorkerStealers>> = OnceLock::new();

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
//
// TLS caching hazard. A suspended process can resume on a different worker
// thread, but the compiler assumes a function's thread never changes, so it
// caches the TLS base (aarch64 `tpidr_el0`, x86-64 `%fs`) in a callee-saved
// register that koja_context_switch faithfully preserves across the
// migration. Any read of these thread-locals after a context switch in the
// same function frame then resolves to the *old* worker's cells, so the
// process jumps to a stale scheduler continuation and two threads run on one
// stack. Therefore every function that can run on a process stack and
// touches this state must be `#[inline(never)]` (a fresh call recomputes the
// base) and must not read TLS after an internal context switch.

thread_local! {
    pub(crate) static CURRENT_PID: Cell<i64> = const { Cell::new(-1) };
    static SCHED_SP: UnsafeCell<*mut u8> = const { UnsafeCell::new(ptr::null_mut()) };
    static YIELD_SP: UnsafeCell<*mut u8> = const { UnsafeCell::new(ptr::null_mut()) };
    /// This worker's local run queues, installed at the top of `worker_loop`.
    /// `Some` only on a worker thread. The reactor thread leaves it `None` and
    /// routes wakes to the global injectors instead. Lets a runtime intrinsic
    /// running in process context (`koja_rt_send`, `koja_rt_spawn`, ...)
    /// co-locate the process it wakes onto the running worker's deque, keeping
    /// communicating processes on one core.
    static LOCAL_QUEUES: RefCell<Option<WorkerQueues>> = const { RefCell::new(None) };
    /// This worker's id (its index into [`STEALERS`]), so `find_work` skips
    /// stealing from itself. Unused (`0`) off a worker thread.
    static WORKER_ID: Cell<usize> = const { Cell::new(0) };
    /// Set by [`koja_rt_yield_check`] just before switching back, consumed
    /// by the worker at switch-out: the worker replays the yield as the
    /// `Running -> Runnable` edge inside `after_switch`. Safe because the
    /// flag is written and read on the same worker thread within one
    /// resume, and the process cannot migrate until `after_switch`
    /// re-queues it.
    static YIELDED: Cell<bool> = const { Cell::new(false) };
}

/// Yields the current process back to its worker's scheduling loop,
/// returning when the worker resumes this process (possibly on a
/// different worker thread).
///
/// `#[inline(never)]` is load-bearing. Inlined into a caller whose frame
/// already crossed a context switch (the trampoline after the process
/// body, `io_block` retry loops), a cached TLS base would resolve
/// `SCHED_SP` to the worker the process *used* to run on and switch onto
/// that worker's live stack. See the TLS caching note above.
#[inline(never)]
pub(crate) fn yield_to_scheduler() {
    tsan::switch_to_scheduler();
    let yield_sp_ptr = YIELD_SP.with(|c| c.get());
    let sched_sp = unsafe { *SCHED_SP.with(|c| c.get()) };
    unsafe {
        koja_context_switch(yield_sp_ptr, sched_sp);
    }
}

/// Routes `envelope` to `pid`, enqueues the wake it may produce, then
/// drops any leftover (an undeliverable envelope, or a stale reply
/// displaced from the reply slot) after the table's slot lock is
/// released, since payload drop glue is arbitrary emitted code and
/// shouldn't run while holding a core lock.
fn deliver_or_discard(pid: i64, envelope: Envelope) {
    let delivery = TABLE.deliver(pid, envelope);
    let woken = delivery.wake.map_or(0, route_wake);
    notify_workers(woken);
    drop(delivery.leftover);
}

/// Payload drop glue for an undelivered `ExitSignal` envelope:
/// releases the two `CrashInfo` strings [`exit_signal_envelope`]
/// allocated.
unsafe extern "C" fn exit_signal_drop_glue(payload: *mut u8) {
    for offset in [EXIT_SIGNAL_MESSAGE_OFFSET, EXIT_SIGNAL_BACKTRACE_OFFSET] {
        unsafe {
            let string = *payload.add(offset - TAG_HEADER_SIZE).cast::<*mut u8>();
            if !string.is_null() {
                koja_rc_dec(string.sub(BLOCK_HEADER_SIZE));
            }
        }
    }
}

/// Synthesizes the `TAG_EXIT_SIGNAL` envelope for one staged
/// [`ExitNotice`] (see the `EXIT_SIGNAL_*` offsets in [`crate::wire`]).
/// A `Crashed` reason allocates the two `CrashInfo` strings, owned by
/// the envelope until delivery.
fn exit_signal_envelope(notice: &ExitNotice) -> Envelope {
    const PAYLOAD_SIZE: usize = EXIT_SIGNAL_BUF_SIZE - TAG_HEADER_SIZE;
    let field = |offset: usize| offset - TAG_HEADER_SIZE;

    let mut payload = [0u8; PAYLOAD_SIZE];
    let mut write_word = |offset: usize, word: u64| {
        payload[field(offset)..field(offset) + 8].copy_from_slice(&word.to_ne_bytes());
    };
    write_word(EXIT_SIGNAL_PID_OFFSET, notice.target as u64);
    if let Some(crash_info) = &notice.crash_info {
        let message = unsafe { alloc_koja_string(crash_info.message.as_bytes()) };
        let backtrace = unsafe { alloc_koja_string(crash_info.backtrace.as_bytes()) };
        write_word(EXIT_SIGNAL_MESSAGE_OFFSET, message as u64);
        write_word(EXIT_SIGNAL_BACKTRACE_OFFSET, backtrace as u64);
    }
    payload[field(EXIT_SIGNAL_REASON_OFFSET)] = notice.reason as u8;

    unsafe {
        Envelope::from_payload(
            TAG_EXIT_SIGNAL,
            payload.as_ptr(),
            PAYLOAD_SIZE,
            Some(exit_signal_drop_glue),
        )
    }
}

/// Synthesizes and delivers every staged `ExitSignal`, routing the wakes
/// of parked watchers. Returns the undeliverable envelopes for the
/// caller to drop, plus how many wakes landed in a global injector.
fn deliver_exit_signals() -> (Vec<Envelope>, usize) {
    let mut leftovers = Vec::new();
    let mut woken = 0;
    for notice in TABLE.take_exit_notices() {
        let delivery = TABLE.deliver(notice.watcher, exit_signal_envelope(&notice));
        woken += delivery.wake.map_or(0, route_wake);
        leftovers.extend(delivery.leftover);
    }
    (leftovers, woken)
}

/// Resources detached while settling a death edge, dropped by the caller
/// after any lock is released. `leftovers` and `reclaims` are never read
/// by name (they exist so their `Drop` runs at that controlled point).
/// `freed` names the reclaimed PIDs so the caller can cancel their armed
/// deadlines via [`cancel_freed_deadlines`].
#[allow(dead_code)]
struct SettledExits {
    freed: Vec<Pid>,
    leftovers: Vec<Envelope>,
    reclaims: Vec<Reclaim<NativeExecution, Envelope>>,
    /// Wakes routed to a global injector, for the caller's
    /// [`notify_workers`].
    woken: usize,
}

/// Settles every consequence of a death edge: the staged-drain cascade.
/// Drains the staged kill targets, kills each with no cross-slot lock
/// held (each kill can stage grandchildren under the registry), and
/// repeats until nothing is staged, then delivers the staged
/// `ExitSignal`s. Call at every site that can stage notices or kills
/// (death edges and `koja_rt_monitor`).
fn settle_exits() -> SettledExits {
    let mut freed = Vec::new();
    let mut reclaims = Vec::new();
    loop {
        let staged = TABLE.take_pending_kills();
        if staged.is_empty() {
            break;
        }
        for pid in staged {
            if let Some(reclaim) = TABLE.kill(pid) {
                freed.push(pid);
                reclaims.push(reclaim);
            }
        }
    }
    let (leftovers, woken) = deliver_exit_signals();
    SettledExits {
        freed,
        leftovers,
        reclaims,
        woken,
    }
}

/// Cancels freed processes' armed deadlines so they don't linger in the
/// wheel until they age out. Call with no core lock held (the timer lock
/// and the table's locks are never held together). Slot reuse is
/// harmless. A PID embeds its generation, so a stale cancel can never
/// hit a recycled slot's fresh entry.
fn cancel_freed_deadlines(freed: &[Pid]) {
    if freed.is_empty() {
        return;
    }
    TIMERS.with(|timers| {
        for &pid in freed {
            timers.cancel_deadline(pid);
        }
    });
}

/// `koja_rt_reply` status: the reply was slotted for a still-waiting caller.
/// Mirrored as `Delivery.Delivered` by the `ReplyTo.send` emitter.
const REPLY_DELIVERED: i64 = 0;
/// `koja_rt_reply` status: the caller had already given up, so the reply was
/// discarded. Mirrored as `Delivery.Expired`.
const REPLY_EXPIRED: i64 = 1;

/// Slots `envelope` for `pid` only if it is still awaiting `token`. The
/// table checks and delivers in one slot hold, so the answer is
/// linearizable against the caller's timeout. Off-lock cleanup mirrors
/// [`deliver_or_discard`].
fn reply_or_expire(pid: i64, token: i64, envelope: Envelope) -> i64 {
    let delivery = TABLE.deliver_reply(pid, token, envelope);
    let woken = delivery.wake.map_or(0, route_wake);
    notify_workers(woken);
    drop(delivery.leftover);
    if delivery.delivered {
        REPLY_DELIVERED
    } else {
        REPLY_EXPIRED
    }
}

/// Prepares a fresh process stack so the first `koja_context_switch`
/// into it "returns" to the `koja_process_start` asm shim, which
/// dispatches to `entry` via the callee-saved slot. The shim's CFI
/// marks the stack bottom so DWARF unwinders (glibc `backtrace`,
/// `_Unwind`) terminate here instead of walking off the fabricated
/// frame, which segfaulted panic backtraces on Linux arm64.
unsafe fn init_process_stack(stack_top: *mut u8, entry: unsafe extern "C" fn()) -> *mut u8 {
    unsafe {
        let sp = stack_top.sub(INIT_FRAME_SIZE);
        ptr::write_bytes(sp, 0, INIT_FRAME_SIZE);
        *(sp.add(RET_ADDR_OFFSET) as *mut usize) =
            koja_process_start as unsafe extern "C" fn() as usize;
        *(sp.add(ENTRY_REG_OFFSET) as *mut usize) = entry as usize;
        sp
    }
}

/// Entry point for every new process. Runs on the process's own stack
/// after the first context switch into it. Reads the process function
/// and initial state from the shared scheduler, calls the function,
/// marks the process dead, and yields back to the worker.
unsafe extern "C" fn process_trampoline() {
    let pid = CURRENT_PID.with(|c| c.get());

    // Safety: this trampoline runs as the freshly claimed process, so it
    // is the slot's `on_cpu` claim holder.
    let Some((func, init_state)) =
        (unsafe { TABLE.with_execution(pid, |execution| execution.entry()) })
    else {
        return;
    };

    // Contain a user crash to this one process: a panicking body unwinds
    // back to here (release-before-suspend guarantees no user code holds
    // a core lock while unwinding), carrying its `UserCrash` payload. We
    // record `Crashed` + the capture, then fall into the normal death
    // path, where the `Dead` edge runs drop glue and reclaims the stack
    // as usual.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        func(init_state);
    }));
    unsafe {
        fflush(ptr::null_mut());
    }

    if let Err(payload) = outcome {
        let crash_info = payload
            .downcast::<crate::panic::UserCrash>()
            .map_or_else(|_| CrashInfo::default(), |boxed| boxed.crash_info);
        TABLE.set_exit_reason(pid, ExitReason::Crashed);
        TABLE.set_crash_info(pid, crash_info);
        // A crashing entry process (PID 1) never returns a `StopReason`, so
        // force a non-zero OS exit. `koja_rt_main_done` applies it on join.
        if pid == TABLE.main_pid() {
            ENTRY_EXIT_OVERRIDE.store(CRASH_EXIT_CODE, Ordering::SeqCst);
        }
    }

    // The `-> Dead` edge stages one `ExitSignal` per monitor of this
    // process plus the kill-cascade for its children. Settle both
    // (waking parked watchers) before yielding, dropping bounced
    // envelopes and reclaimed resources off-lock.
    TABLE.mark_dead_if_alive(pid);
    let settled = settle_exits();
    cancel_freed_deadlines(&settled.freed);
    notify_workers(settled.woken);
    drop(settled);

    notify_all_workers();
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

/// Routes one newly-runnable process to a run queue, returning how many
/// wakes landed in a global injector (i.e. need a peer woken via
/// [`notify_workers`]). On a worker thread the wake is co-located onto
/// that worker's local deque, keeping a process and whoever woke it (a
/// `send`/`reply` partner, a parent that just spawned it) on one core, so
/// it returns 0 and the running worker drains it itself. Off a worker
/// (the reactor thread) it falls back to the per-priority injectors.
/// Lock-free either way.
///
/// `#[inline(never)]` is load-bearing. This runs on process stacks (send,
/// spawn, the trampoline's death path) where a cached TLS base from before
/// a context switch would resolve `LOCAL_QUEUES` to a *different* worker's
/// single-owner deque, racing its owner. See the TLS caching note above.
#[inline(never)]
pub(crate) fn route_wake(wake: Wake) -> usize {
    LOCAL_QUEUES.with(|cell| {
        let local = cell.borrow();
        match local.as_ref() {
            Some(local) => {
                local[wake.priority as usize].push(wake.pid);
                0
            }
            None => {
                INJECTORS[wake.priority as usize].push(wake.pid);
                1
            }
        }
    })
}

/// Wakes up to `count` parked workers: one per newly-runnable process, so
/// a burst of wakes spreads across idle workers without a thundering
/// herd. A no-op when `count` is 0 or no worker is parked (checked under
/// the sleeper mutex, so the common nobody-parked case is one short
/// uncontended lock). Call with the work already pushed, so a woken
/// worker finds it.
pub(crate) fn notify_workers(count: usize) {
    if count == 0 {
        return;
    }
    let sleepers = IDLE_PARK.sleepers.lock().unwrap();
    for _ in 0..count.min(*sleepers) {
        IDLE_PARK.wakeup.notify_one();
    }
}

/// Wakes every parked worker: the shutdown broadcast (and the death
/// path's "main may be dead" nudge).
pub(crate) fn notify_all_workers() {
    let _sleepers = IDLE_PARK.sleepers.lock().unwrap();
    IDLE_PARK.wakeup.notify_all();
}

/// Finds the next candidate PID for the running worker: its own local
/// deques first (highest priority first), then the global injectors
/// (the reactor's wake channel), then stealing from peers.
/// Injectors come before stealing so a cross-thread wake is a cheap drain
/// rather than a full sibling scan. Stealing only kicks in to spread a busy
/// worker's co-located backlog. `None` means no work is visible right now (or
/// this is not a worker thread). The returned PID may still be stale, so the
/// caller validates it with [`ProcessTable::try_claim`].
fn find_work() -> Option<Pid> {
    LOCAL_QUEUES.with(|cell| {
        let borrow = cell.borrow();
        let local = borrow.as_ref()?;
        for level in (0..READY_LEVELS).rev() {
            if let Some(pid) = local[level].pop() {
                return Some(pid);
            }
        }
        for level in (0..READY_LEVELS).rev() {
            if let Some(pid) = retry_steal(|| INJECTORS[level].steal_batch_and_pop(&local[level])) {
                return Some(pid);
            }
        }
        let me = WORKER_ID.with(Cell::get);
        let stealers = STEALERS.get()?;
        for level in (0..READY_LEVELS).rev() {
            for (peer, handles) in stealers.iter().enumerate() {
                if peer == me {
                    continue;
                }
                if let Some(pid) = retry_steal(|| handles[level].steal_batch_and_pop(&local[level]))
                {
                    return Some(pid);
                }
            }
        }
        None
    })
}

/// Runs a `steal_batch_and_pop` (from a [`Stealer`] or an [`Injector`])
/// to completion, retrying past transient contention. `None` when the
/// source is empty.
fn retry_steal(mut attempt: impl FnMut() -> Steal<Pid>) -> Option<Pid> {
    loop {
        match attempt() {
            Steal::Success(pid) => return Some(pid),
            Steal::Retry => continue,
            Steal::Empty => return None,
        }
    }
}

/// Whether any global injector holds work: the lost-wakeup guard a worker
/// re-checks under the sleeper mutex before parking, since producers push
/// to an injector before notifying.
fn injectors_nonempty() -> bool {
    INJECTORS.iter().any(|injector| !injector.is_empty())
}

/// Core scheduling loop run by every worker thread.
///
/// Each iteration: service timers/deadlines, find a runnable process via
/// [`find_work`] (local deque, then the global injectors, then stealing),
/// claim it with a lock-free CAS, context-switch into it, and on return
/// persist its saved stack pointer and re-queue or reclaim it. A process
/// that yields back (or one woken by a `send`/`reply`/`spawn` running on
/// this worker) stays on this worker's local deque (co-location). When no
/// work is visible the worker parks on [`IDLE_PARK`], and it exits when
/// [`SHUTDOWN`] is set.
fn worker_loop(local: WorkerQueues, me: usize) {
    tsan::capture_scheduler_fiber();
    // Publish this worker's deques so intrinsics running in process context
    // (e.g. `koja_rt_send`) can co-locate the processes they wake here.
    WORKER_ID.with(|id| id.set(me));
    LOCAL_QUEUES.with(|cell| *cell.borrow_mut() = Some(local));

    loop {
        if SHUTDOWN.load(Ordering::Relaxed) {
            break;
        }

        poll_signals();

        // The timer gate: a single relaxed load. Only when something is
        // actually due does this iteration take the timer lock (drain)
        // and touch the table (apply).
        let now = NativeClock.now();
        if TIMERS.due(now) {
            let (due, grace_fired) =
                TIMERS.with(|timers| (timers.advance(now), timers.grace_expired(now)));

            let (leftovers, published) = apply_due(due);

            // Drain grace elapsed: force-kill stragglers. They become `Dead`
            // (alive == 0, main included), so the runtime tears down. The
            // detached resources drop before the loop exits.
            if grace_fired && TABLE.is_draining() {
                let reclaim = TABLE.kill_all();
                // Everything is dead, so the staged cascade kills are
                // no-ops. Settling still drains the staging queues.
                let settled = settle_exits();
                SHUTDOWN.store(true, Ordering::Relaxed);
                notify_all_workers();
                drop(leftovers);
                drop(reclaim);
                drop(settled);
                break;
            }
            notify_workers(published);
            drop(leftovers);
        }

        match claim_work() {
            Some((pid, proc_sp)) => {
                let saved_sp = NativeExecutor.resume(pid, proc_sp);
                // A voluntary yield (reduction budget spent) flagged itself
                // in this worker's TLS instead of touching the table. Reading
                // it here is safe: worker_loop never migrates threads.
                let yielded = YIELDED.with(|flag| flag.replace(false));

                // Persist the saved `sp` into the executor's execution state
                // *before* the `after_switch` release publishes it to the
                // next claimer. Safety: this worker still holds the claim.
                unsafe { TABLE.with_execution(pid, |execution| execution.sp = saved_sp) };
                if yielded {
                    // Replay the yield's `Running -> Runnable` edge so
                    // `after_switch` re-queues the process.
                    TABLE.yield_running(pid);
                }
                // Release the claim and route the process by its observed
                // state. Co-location: a re-queued process stays on this
                // worker's local deque so it resumes warm here.
                let reclaim = match TABLE.after_switch(pid) {
                    SwitchOutcome::Requeue(wake) => {
                        route_wake(wake);
                        None
                    }
                    SwitchOutcome::Reclaimed(reclaim) => Some(reclaim),
                    SwitchOutcome::Parked => None,
                };

                let shutdown = TABLE.should_shutdown();
                if shutdown {
                    SHUTDOWN.store(true, Ordering::Relaxed);
                    notify_all_workers();
                }
                // A reclaimed process may have died parked with a deadline
                // armed (a deferred kill). Cancel it now.
                if reclaim.is_some() {
                    TIMERS.with(|timers| timers.cancel_deadline(pid));
                }
                drop(reclaim);
                if shutdown {
                    break;
                }
            }
            None => {
                if park_for_work(NativeClock.now()) {
                    break;
                }
            }
        }
    }
}

/// Claims the next runnable process for the running worker, returning it with
/// its saved stack pointer and this quantum's reduction budget seeded. Pops
/// candidates via [`find_work`] and validates each with the lock-free
/// [`ProcessTable::try_claim`], skipping stale entries (killed or already
/// resumed). `None` when no claimable work is visible.
fn claim_work() -> Option<(Pid, *mut u8)> {
    while let Some(pid) = find_work() {
        if !TABLE.try_claim(pid) {
            continue;
        }
        // Safety: the claim just succeeded, so this worker owns the slot's
        // execution state until `after_switch`.
        let proc_sp = unsafe { TABLE.with_execution(pid, |execution| execution.sp) }
            .expect("just-claimed process exists");
        // Seed this quantum's reduction budget (reset by `try_claim`) into the
        // C thread-local that compiled process code decrements inline at each
        // `YieldCheck`, calling `koja_rt_yield_check` only on exhaustion.
        unsafe { koja_seed_reductions(TABLE.reductions_left(pid)) };
        return Some((pid, proc_sp));
    }
    None
}

/// Parks the worker until woken or a deadline elapses. Returns `true` when
/// the worker should exit (shutdown). Registers as a sleeper and re-checks
/// injector emptiness under the sleeper mutex, so a wake that landed
/// between [`find_work`] and this registration is never missed (producers
/// push before they take the mutex to notify).
fn park_for_work(now: Instant) -> bool {
    if TABLE.should_shutdown() {
        SHUTDOWN.store(true, Ordering::Relaxed);
        notify_all_workers();
        return true;
    }
    let mut sleepers = IDLE_PARK.sleepers.lock().unwrap();
    if injectors_nonempty() || SHUTDOWN.load(Ordering::Relaxed) {
        return false;
    }
    let idle_park = Duration::from_millis(if TABLE.any_active() { 10 } else { 100 });
    let timeout = TIMERS
        .next_fire()
        .map(|deadline| deadline.saturating_duration_since(now))
        .unwrap_or(idle_park);
    *sleepers += 1;
    let (mut sleepers, _) = IDLE_PARK.wakeup.wait_timeout(sleepers, timeout).unwrap();
    *sleepers -= 1;
    false
}

/// Applies one drained timer batch, waking expired deadline waiters
/// (re-validated by the table) and delivering due envelopes, staged in
/// wire format at schedule time so firing is a plain delivery. Returns
/// the undeliverable envelopes (target gone or dead) for the caller to
/// drop off any core lock, plus the injector-routed wake count for
/// [`notify_workers`].
fn apply_due(due: Vec<Due<Envelope>>) -> (Vec<Envelope>, usize) {
    let mut leftovers = Vec::new();
    let mut published = 0;
    for entry in due {
        match entry {
            Due::Deliver {
                envelope,
                target_pid,
            } => {
                let delivery = TABLE.deliver(target_pid, envelope);
                published += delivery.wake.map_or(0, route_wake);
                leftovers.extend(delivery.leftover);
            }
            Due::Promote { pid, fire_at } => {
                if let Some(wake) = TABLE.promote_expired(pid, fire_at) {
                    published += route_wake(wake);
                }
            }
        }
    }
    (leftovers, published)
}

static RUNTIME_INIT: Once = Once::new();

/// One-time process-global runtime initialization. Installs the panic hook
/// that converts any Rust panic (on any thread, before unwinding) into a
/// clean diagnostic abort, so a panic can never unwind across the C-ABI or
/// poison a scheduler lock. Called at the head of every runtime entry
/// point (`koja_rt_spawn` is the first one a program reaches), so the hook
/// is live before any worker thread is spawned.
fn ensure_runtime_init() {
    RUNTIME_INIT.call_once(panic::install_panic_hook);
}

/// Called by the compiled Koja program after `main` returns. The C-ABI
/// entry point for the runtime: installs the panic hook, then hands off to
/// the [`NativeDriver`], which runs until the main process (PID 1) dies.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_main_done() {
    ensure_runtime_init();
    NativeDriver.run();
    // The entry process crashed: its body unwound instead of returning a
    // `StopReason`, so override the host exit status with a non-zero code
    // (flushing stdout first, since `process::exit` skips libc atexit
    // buffer flushing). Normal exits fall through and `main` returns its
    // own code.
    let override_code = ENTRY_EXIT_OVERRIDE.load(Ordering::SeqCst);
    if override_code >= 0 {
        unsafe { fflush(ptr::null_mut()) };
        std::process::exit(override_code);
    }
}

/// Forces line-buffered stdout so output is visible immediately even when
/// stdout is a pipe (e.g. when the process is spawned by a test harness).
fn line_buffer_stdout() {
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
}

/// The native [`Driver`]: a pool of `worker_count()` worker OS threads
/// plus a dedicated reactor thread. Each worker owns per-priority local
/// run queues and steals from its peers. The internally synchronized
/// [`TABLE`] owns the process slots, and idle workers park on
/// [`IDLE_PARK`].
pub(crate) struct NativeDriver;

impl Driver for NativeDriver {
    type Executor = NativeExecutor;

    /// Boots the I/O reactor and signal handlers, hands ready-queue
    /// ownership to the work-stealing deques, spawns the reactor thread plus
    /// `worker_count() - 1` workers, and runs the final worker loop on the
    /// current thread. Blocks until every thread joins (i.e. until the main
    /// process dies and [`SHUTDOWN`] is set), then reports shutdown
    /// diagnostics.
    fn run(self) {
        line_buffer_stdout();
        NativeSignals.install();
        reactor::init();

        let n = worker_count();
        // Build one deque-set per worker and register every stealer before
        // any worker starts, so a worker that races ahead can already steal.
        let queues: Vec<WorkerQueues> = (0..n)
            .map(|_| std::array::from_fn(|_| Worker::new_fifo()))
            .collect();
        let stealers: Vec<WorkerStealers> = queues
            .iter()
            .map(|local| std::array::from_fn(|level| local[level].stealer()))
            .collect();
        let _ = STEALERS.set(stealers);

        let mut handles = Vec::with_capacity(n);
        handles.push(thread::spawn(reactor::reactor_loop));
        // Worker 0 runs on this thread, and 1..n run on spawned threads. The
        // worker id indexes into `STEALERS`, so it must match `queues`.
        let mut main_queue = None;
        for (me, local) in queues.into_iter().enumerate() {
            if me == 0 {
                main_queue = Some(local);
            } else {
                handles.push(thread::spawn(move || worker_loop(local, me)));
            }
        }
        worker_loop(main_queue.expect("worker_count() >= 1"), 0);

        reactor::notify();
        for handle in handles {
            let _ = handle.join();
        }

        maybe_report_live_heap();
        maybe_dump_scheduler_trace();
    }
}

/// When `KOJA_HEAP_REPORT` is set, print the runtime's net live-block
/// count at shutdown. Informational only, it does *not* alter the exit
/// code. Runtime-internal allocations (and any orphaned live processes)
/// are still counted here, so a nonzero total is expected for real
/// programs. The robust leak guard is the steady-state delta check in
/// the `lang_ownership` fixtures (see [`crate::memory::koja_rt_live_blocks`]).
fn maybe_report_live_heap() {
    if std::env::var_os("KOJA_HEAP_REPORT").is_some() {
        eprintln!(
            "koja: live heap blocks at shutdown: {}",
            memory::koja_rt_live_blocks(),
        );
    }
}

/// When `KOJA_SCHED_TRACE` is set, dump the scheduler's lifecycle event
/// ring (oldest first) and counter totals at shutdown. The debugging
/// companion to `koja_rt_sched_violations`: when a race fixture fails,
/// re-run with this set and read the offending interleaving directly.
fn maybe_dump_scheduler_trace() {
    if std::env::var_os("KOJA_SCHED_TRACE").is_none() {
        return;
    }
    for entry in TABLE.trace_entries() {
        eprintln!(
            "koja sched: {:>8} pid {:#012x} {}",
            entry.seq, entry.pid, entry.event,
        );
    }
    let counters = TABLE.counters();
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
/// wire tag. Context-switches back to the scheduler (marking the
/// process `Blocked`) until a message arrives. System traffic is
/// drained before business traffic (see
/// [`Mailbox::pop_received`](koja_runtime_core::mailbox::Mailbox::pop_received)).
/// Returns `-1` only if woken with empty queues, which no live wake
/// path produces.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_receive(out: *mut u8, out_cap: i64) -> i64 {
    let pid = CURRENT_PID.with(|c| c.get());

    // Check and park in one slot hold, so a delivery can't slip between
    // the empty check and the park. A refused park (killed mid-run) still
    // yields: the owner reclaims at switch-out and the frame never resumes.
    match TABLE.receive_or_park(pid, None) {
        MailPark::Ready(envelope) => return deliver_envelope(envelope, out, out_cap),
        MailPark::Parked | MailPark::Refused | MailPark::Stale(_) => {}
    }

    yield_to_scheduler();

    TABLE
        .pop_received(pid)
        .map_or(-1, |envelope| deliver_envelope(envelope, out, out_cap))
}

/// Receive with a timeout. Like [`koja_rt_receive`] but sets a
/// deadline `timeout_ms` milliseconds in the future. If no message
/// arrives before the deadline, the worker loop promotes the process
/// back to `Runnable` and this returns `-1`.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_receive_timeout(out: *mut u8, out_cap: i64, timeout_ms: i64) -> i64 {
    let pid = CURRENT_PID.with(|c| c.get());
    let now = Instant::now();
    let deadline = now + duration_from_user_millis(timeout_ms);
    if deadline <= now {
        return TABLE
            .pop_received(pid)
            .map_or(-1, |envelope| deliver_envelope(envelope, out, out_cap));
    }

    match TABLE.receive_or_park(pid, Some(deadline)) {
        MailPark::Ready(envelope) => return deliver_envelope(envelope, out, out_cap),
        // Arm outside the slot hold. The order is safe: this process
        // cannot be resumed (and so cannot re-arm or cancel) until it
        // yields below, and a fire before the arm just misses an entry
        // that lands on the next drain.
        MailPark::Parked => TIMERS.with(|timers| timers.arm_deadline(pid, deadline)),
        // A refused park (killed mid-run) arms nothing but still yields.
        MailPark::Refused | MailPark::Stale(_) => {}
    }

    yield_to_scheduler();

    TABLE.clear_deadline(pid);
    let envelope = TABLE.pop_received(pid);
    TIMERS.with(|timers| timers.cancel_deadline(pid));
    envelope.map_or(-1, |envelope| deliver_envelope(envelope, out, out_cap))
}

/// Monotonic source of call-correlation tokens. Global rather than
/// per-process so tokens never repeat for the lifetime of the runtime,
/// meaning a stale reply can never collide with a later call's token.
static CALL_TOKEN: AtomicI64 = AtomicI64::new(0);

/// Mints a fresh correlation token for a `Ref.call` and registers the caller
/// as awaiting that reply *before* the request is sent, so a fast reply can't
/// beat the caller to `ReplyTo.send`'s awaited-token check.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_call_token() -> i64 {
    let token = CALL_TOKEN.fetch_add(1, Ordering::Relaxed) + 1;
    let pid = CURRENT_PID.with(|c| c.get());
    TABLE.set_awaiting_reply(pid, token);
    token
}

/// Blocks until the reply correlated with `token` lands in this
/// process's reply slot, copies its payload into `out` (at most
/// `out_cap` bytes), and returns `0`. Returns `-1` if `timeout_ms`
/// elapses first. A slotted reply with a different token is a stale
/// leftover from an earlier call that timed out, so it is dropped
/// (running its payload drop glue) and the wait continues. Queue traffic is left
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
    let deadline = Instant::now() + duration_from_user_millis(timeout_ms);

    loop {
        // The timeout check comes before the reply check, so a woken
        // waiter that finds its deadline passed gives up even if a reply
        // squeaked in meanwhile; `clear_awaiting_reply` makes the sender's
        // `Expired` answer authoritative from here on.
        if Instant::now() >= deadline {
            TABLE.clear_deadline(pid);
            TABLE.clear_awaiting_reply(pid);
            TIMERS.with(|timers| timers.cancel_deadline(pid));
            return -1;
        }
        // Check the reply slot and park in one hold, so a reply can't
        // slip between the empty check and the park. A matching take
        // also clears the awaited token and recorded deadline.
        match TABLE.take_reply_or_park(pid, token, Some(deadline)) {
            MailPark::Ready(envelope) => {
                TIMERS.with(|timers| timers.cancel_deadline(pid));
                deliver_envelope(envelope, out, out_cap);
                return 0;
            }
            // A stale leftover from an earlier timed-out call: drop it
            // (running its payload glue) and re-check immediately.
            MailPark::Stale(stale) => drop(stale),
            MailPark::Parked => {
                // Arm outside the slot hold (see `koja_rt_receive_timeout`
                // for the ordering argument).
                TIMERS.with(|timers| timers.arm_deadline(pid, deadline));
                yield_to_scheduler();
            }
            // A refused park (killed mid-run) arms nothing but still
            // yields; the owner reclaims at switch-out.
            MailPark::Refused => yield_to_scheduler(),
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
/// only the transport buffer, never running the glue (see
/// [`crate::wire::Envelope`]).
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
}

/// Sends a reply for the in-flight call `token` to the caller `pid`, tagged
/// `TAG_REPLY` and routed to its one-shot reply slot (never the receive
/// queues). Returns [`REPLY_DELIVERED`] if the caller was still awaiting the
/// token, or [`REPLY_EXPIRED`] if it gave up first (the reply is dropped).
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
) -> i64 {
    let mut envelope =
        unsafe { Envelope::from_payload(TAG_REPLY, msg_ptr, msg_len as usize, drop_glue) };
    envelope.reply_token = token;
    reply_or_expire(pid, token, envelope)
}

/// Sends a lifecycle event to the given process, routed to its system
/// queue so `receive` sees it before any queued business traffic.
///
/// Variant indices: 0=Shutdown, 1=Interrupt, 2=Reload.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_send_lifecycle(pid: i64, variant: i64) {
    send_lifecycle_to(pid, variant);
}

/// Records the calling process's exit reason from a wire code (0=Normal,
/// 1=Shutdown, ...). Emitted in the process-body tail from the process's
/// own `StopReason`, so the reason is set before the trampoline marks it
/// dead and `notify_exit` fires.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_process_exit(reason: i64) {
    let pid = CURRENT_PID.with(|c| c.get());
    TABLE.set_exit_reason(pid, ExitReason::from_index(reason));
}

/// Sets the calling process's scheduling priority from a `Priority`
/// variant index (0=Low, 1=Normal, 2=High). Called once per process
/// body right after `start` succeeds, so the process runs at its
/// declared priority for the rest of its life.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_set_priority(level: i64) {
    let pid = CURRENT_PID.with(|c| c.get());
    TABLE.set_priority(pid, Priority::from_index(level));
}

/// Slow path of the cooperative preemption point. Compiled process code
/// decrements the per-worker `koja_reductions_left` budget inline at each
/// `YieldCheck` and calls this only once it hits zero. It takes no lock:
/// it flags the voluntary yield in [`YIELDED`] and switches back, and the
/// worker replays the `Running -> Runnable` edge inside `after_switch`
/// at switch-out. Taking a lock here instead serialized every
/// compute-bound worker's quantum expiry through one mutex (the
/// dominant contended stack in the process_storm profile). The next
/// resume re-seeds the budget through `koja_seed_reductions`.
///
/// The [`YIELDED`] write happens before the switch on the running worker's
/// own TLS, so the TLS-caching hazard above does not apply.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_yield_check() {
    let pid = CURRENT_PID.with(|c| c.get());
    if pid < 0 {
        return;
    }
    YIELDED.with(|flag| flag.set(true));
    yield_to_scheduler();
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
}

/// Schedules a message to be delivered to `pid` after `delay_ms`
/// milliseconds. The message is staged as a finished envelope
/// immediately, and the worker loop delivers it when the timer fires.
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
    let fire_at = Instant::now() + duration_from_user_millis(delay_ms);

    TIMERS.with(|timers| timers.schedule_deliver(fire_at, pid, envelope));
    // Wake a parked worker so it re-sizes its idle park to the new timer.
    notify_workers(1);
}

/// Returns 1 if the process with the given PID is still alive (not `Dead`),
/// 0 otherwise. An out-of-range PID returns 0.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_is_process_alive(pid: i64) -> i64 {
    TABLE.is_alive(pid) as i64
}

/// Immediately marks a process as `Dead`, reclaiming its stack and
/// initial state. No signal is sent, so the process gets no chance to
/// run cleanup. This is the "last resort" termination primitive.
///
/// If the target is currently executing on another worker (`on_cpu`),
/// all of its resources (stack, initial state, and any undelivered
/// mailbox envelopes) are reclaimed by that worker when it switches
/// out. Otherwise they are freed here. Either way the [`Reclaim`] drop
/// runs every discarded envelope's payload glue and the init-state
/// config glue, so nested heap is released too.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_kill(pid: i64) {
    let reclaim = TABLE.kill(pid);
    let settled = settle_exits();
    notify_workers(settled.woken);
    // A parked target may have died with a deadline armed. A deferred
    // (on_cpu) kill reclaims at the owner's switch-out, which cancels
    // there instead.
    if reclaim.is_some() {
        TIMERS.with(|timers| timers.cancel_deadline(pid));
    }
    cancel_freed_deadlines(&settled.freed);
    drop(reclaim);
    drop(settled);
}

/// Registers the calling process as a monitor of `target`, returning
/// the token backing `Process.MonitorRef`. Monitoring an already-dead
/// PID delivers the `ExitSignal` immediately.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_monitor(target: i64) -> i64 {
    let watcher = CURRENT_PID.with(|c| c.get());
    let token = TABLE.monitor(watcher, target);
    let settled = settle_exits();
    notify_workers(settled.woken);
    cancel_freed_deadlines(&settled.freed);
    drop(settled);
    token
}

/// Removes the monitor identified by `token`, suppressing its
/// `ExitSignal`. A no-op for an unknown or already-fired token.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_demonitor(token: i64) {
    TABLE.demonitor(token);
}

/// The calling process's parent PID, or 0 for the entry process
/// (backing `Option.None` at the emitter).
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_parent() -> i64 {
    let pid = CURRENT_PID.with(|c| c.get());
    TABLE.parent(pid).unwrap_or(0)
}

/// Count of illegal lifecycle edges the scheduler has applied: zero in
/// a correct runtime, in release builds too. The machine oracle for the
/// race-regression lang fixtures (see `tests/lang/memory/`).
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_sched_violations() -> i64 {
    TABLE.counters().violations as i64
}

/// Count of parks refused over a kill tombstone. Positive evidence that
/// the kill-vs-park window was actually hit during a storm fixture.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_parks_refused() -> i64 {
    TABLE.counters().parks_refused as i64
}

/// Spawns a new lightweight process that will call `fn_ptr(state)`.
///
/// Allocates a stack, copies the initial state onto the heap, and
/// registers the process as `Created`. Wakes an idle worker via
/// [`notify_workers`]. Returns the new process's PID.
///
/// The process owns its config copy: `drop_glue` (null when the config
/// owns no nested heap) runs over it when the process's resources are
/// reclaimed, whether it exited normally, was killed, or never ran.
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

    // Refuse new processes once draining (SIGTERM seen): the program is
    // shutting down. Return the invalid pid 0 (a `Ref` over it behaves like
    // a ref to an already-dead process). Dropping `init_state` runs its glue.
    if TABLE.is_draining() {
        drop(init_state);
        return 0;
    }

    let (stack, sp) = allocate_process_stack();
    let execution = NativeExecution::new(fn_ptr, init_state, stack, sp);

    // The spawner becomes the parent. On the host thread (the entry
    // process's spawn) `CURRENT_PID` is -1, so PID 1 gets no parent.
    let parent = match CURRENT_PID.with(|c| c.get()) {
        pid if pid > 0 => Some(pid),
        _ => None,
    };

    let id = match TABLE.spawn(execution, parent) {
        Ok(id) => id,
        // A kill landed on the (still running) spawner: refuse, so the
        // child cannot outlive the already-settled kill cascade. The
        // execution drops here, running the config glue and releasing
        // the stack.
        Err(refused) => {
            drop(refused);
            return 0;
        }
    };
    let woken = route_wake(Wake {
        pid: id,
        priority: Priority::default(),
    });

    // Materialize the slot's TSan fiber here, at spawn, rather than lazily on
    // first claim. Creation is a no-op off `koja_tsan`. Under it, deferring to
    // claim would move every `__tsan_create_fiber` into the high-concurrency
    // scheduling window, where it trips TSan's own cooperative-fiber
    // bookkeeping (see `crate::tsan` and the `tsan` justfile recipe).
    tsan::slot_fiber(slot_index(id));

    notify_workers(woken);
    id
}
