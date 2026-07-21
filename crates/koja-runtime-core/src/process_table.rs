//! Generational slotmap of live processes: the agnostic scheduling
//! *policy*, internally sharded so every method takes `&self` and hot
//! paths never meet a global lock. Timers and deadlines live in the
//! separately-locked [`TimerService`](crate::timer_service::TimerService).
//! Ready queues are driver-owned (work-stealing deques in native, a
//! [`ReadyQueue`](crate::ready_queue::ReadyQueue) in cooperative
//! backends), fed by the [`Wake`] facts this table returns.
//!
//! A PID packs a slot index and a generation: `pid = (generation << 32) |
//! index`. Slots are reused after a process dies (so memory is bounded), and
//! the generation is bumped on free so a stale `Ref` to a recycled slot fails
//! every lookup and CAS instead of aliasing the new occupant.
//!
//! Synchronization is per the sharded-table protocol in
//! `design/SCHEDULER-PROTOCOL.md`:
//!
//! - Each slot's lifecycle (generation, state, `on_cpu`) is one packed
//!   atomic word ([`LifecycleWord`]), and every state change is a CAS edge.
//! - Each slot's messaging and death-path state sits behind its own
//!   mutex. Coupled edges (park, wake, kill) CAS the word while holding
//!   it. The claim family (`try_claim`, the `after_switch` release) is
//!   lock-free.
//! - The registry mutex owns the cold global state: freelist and arena
//!   growth, monitors, staged exit notices and cascade kills, and the
//!   drain mode. Monitor aliveness and the spawn tombstone check are
//!   decided under its hold.
//! - Counters are relaxed atomics. `execution` lives in a cell owned by
//!   the `on_cpu` claim holder, and slots live in an append-only chunked
//!   arena so they never move under lock-free readers.
//!
//! The hierarchy is flat: a thread holds at most one core lock (slot or
//! registry) at a time, asserted in debug builds. Cross-shard effects
//! (the kill cascade) stage under one lock, release, and re-validate
//! under the next.

use std::cell::UnsafeCell;
use std::collections::{BTreeMap, BTreeSet, btree_map};
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{
    AtomicI64, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};
use std::time::Instant;

use crate::lifecycle::LifecycleWord;
use crate::mailbox::{Mailbox, WaitTarget};
use crate::protocol::{Message, Pid, Tag};
use crate::scheduler_trace::{SchedulerTrace, TraceEntry, TraceEvent};

/// Splits a packed PID into `(slot_index, generation)`.
fn decode(pid: Pid) -> (u32, u32) {
    ((pid & 0xFFFF_FFFF) as u32, (pid >> 32) as u32)
}

/// Packs a slot index and generation into a PID. Generation starts at 1, so a
/// live PID is always `>= 2^32` and `0` is never a valid handle.
fn encode(index: u32, generation: u32) -> Pid {
    ((generation as i64) << 32) | (index as i64)
}

/// The slot index a PID resolves to. Adapters use it to key per-slot
/// platform state (e.g. a reused TSan fiber) outside the table.
pub const fn slot_index(pid: Pid) -> usize {
    (pid & 0xFFFF_FFFF) as usize
}

/// Debug-build enforcement of the flat lock hierarchy: a thread may hold
/// at most one core lock (slot or registry) at a time. Acquire the token
/// *before* blocking on the mutex, so waiting while holding another core
/// lock is caught too.
struct Held;

#[cfg(debug_assertions)]
thread_local! {
    static CORE_LOCKS_HELD: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

impl Held {
    fn acquire() -> Self {
        #[cfg(debug_assertions)]
        CORE_LOCKS_HELD.with(|held| {
            assert_eq!(
                held.get(),
                0,
                "flat lock hierarchy violated: a core lock is already held on this thread",
            );
            held.set(1);
        });
        Held
    }
}

#[cfg(debug_assertions)]
impl Drop for Held {
    fn drop(&mut self) {
        CORE_LOCKS_HELD.with(|held| held.set(held.get() - 1));
    }
}

/// The executor's per-process execution state, owned by the `on_cpu`
/// claim holder (see [`ProcessTable::with_execution`]).
///
/// Safety of the `Sync` impl: access is licensed exclusively by the
/// lifecycle word. A spawner writes before the `occupy` release-store, a
/// claimer's acquire-CAS licenses its reads and writes until its
/// release-CAS clears `on_cpu`, and a reclaimer takes the value only
/// after proving no claim exists (a `Dead` CAS observing `on_cpu`
/// clear, under the slot mutex). At most one such owner exists at a
/// time, with acquire/release pairs ordering every handoff.
struct ExecutionCell<X>(UnsafeCell<Option<X>>);

unsafe impl<X: Send> Sync for ExecutionCell<X> {}

/// The per-slot state behind the slot mutex: the mailbox and its wait
/// bookkeeping (the messaging hot path) plus the death-path capture.
struct HotState<M> {
    /// Correlation token of the `Ref.call` this process is currently waiting
    /// on a reply for, set when the token is minted and cleared when the call
    /// completes. Reply delivery consults it to report whether the caller is
    /// still listening.
    awaiting_reply: Option<i64>,
    /// Crash capture recorded at the unwind site. `None` unless the process
    /// died with `ExitReason::Crashed`.
    crash_info: Option<CrashInfo>,
    /// Optional wake deadline. Set when parking with a timeout, cleared on
    /// resume. [`ProcessTable::promote_expired`] re-validates a fired timer
    /// entry against it before waking.
    deadline: Option<Instant>,
    /// Why the process terminated, recorded at its death site and read
    /// when the death edge stages exit notices. `Normal` until set.
    exit_reason: ExitReason,
    /// Routed message queues plus the one-shot reply slot.
    mailbox: Mailbox<M>,
    /// What a `Blocked` process is waiting on, so delivery only wakes it for
    /// traffic that can satisfy the wait. Meaningful only while `Blocked`.
    waiting: WaitTarget,
}

impl<M> Default for HotState<M> {
    fn default() -> Self {
        Self {
            awaiting_reply: None,
            crash_info: None,
            deadline: None,
            exit_reason: ExitReason::default(),
            mailbox: Mailbox::default(),
            waiting: WaitTarget::Receive,
        }
    }
}

/// One slot: the lifecycle word, the mutex-guarded hot state, the
/// claim-holder-owned execution cell, and lock-free per-process scalars.
struct Slot<X, M> {
    execution: ExecutionCell<X>,
    hot: Mutex<HotState<M>>,
    lifecycle: LifecycleWord,
    /// The spawning process (0 = none). Written before the slot's
    /// `occupy` release-store, read lock-free by the kill-cascade scan
    /// and `parent`.
    parent: AtomicI64,
    /// Scheduling priority as its wire index. Relaxed: a torn moment
    /// between store and enqueue only misroutes one wake's level.
    priority: AtomicU8,
    /// This quantum's reduction budget, granted on each claim.
    reductions: AtomicU32,
}

impl<X, M> Slot<X, M> {
    fn new() -> Self {
        Self {
            execution: ExecutionCell(UnsafeCell::new(None)),
            hot: Mutex::new(HotState::default()),
            lifecycle: LifecycleWord::new(),
            parent: AtomicI64::new(0),
            priority: AtomicU8::new(Priority::default() as u8),
            reductions: AtomicU32::new(0),
        }
    }

    fn priority(&self) -> Priority {
        Priority::from_index(self.priority.load(Ordering::Relaxed) as i64)
    }
}

/// Slots per arena chunk 0. Chunk `k` holds `BASE_CHUNK << k` slots, so
/// the chunk pointer array stays tiny while capacity grows geometrically.
const BASE_CHUNK: usize = 64;
/// Enough chunks to cover every 32-bit slot index.
const MAX_CHUNKS: usize = 32;

/// Append-only chunked slot storage. Slots never move once published, so
/// lock-free readers can hold `&Slot` across arena growth. Growth happens
/// under the registry mutex, and `published` is the release-published slot
/// count readers bound their indexing by.
struct SlotArena<X, M> {
    chunks: [AtomicPtr<Slot<X, M>>; MAX_CHUNKS],
    published: AtomicUsize,
}

/// Locates `index`: chunk `k` spans `[BASE_CHUNK * (2^k - 1),
/// BASE_CHUNK * (2^(k+1) - 1))`.
fn chunk_position(index: usize) -> (usize, usize) {
    let granule = index / BASE_CHUNK + 1;
    let chunk = (usize::BITS - 1 - granule.leading_zeros()) as usize;
    let offset = index - BASE_CHUNK * ((1 << chunk) - 1);
    (chunk, offset)
}

/// Slot count of chunk `k`.
fn chunk_len(chunk: usize) -> usize {
    BASE_CHUNK << chunk
}

impl<X, M> SlotArena<X, M> {
    const fn new() -> Self {
        Self {
            chunks: [const { AtomicPtr::new(ptr::null_mut()) }; MAX_CHUNKS],
            published: AtomicUsize::new(0),
        }
    }

    /// The published slot count. Indices below it are safely readable.
    fn len(&self) -> usize {
        self.published.load(Ordering::Acquire)
    }

    /// The slot at `index`, or `None` beyond the published length.
    fn get(&self, index: u32) -> Option<&Slot<X, M>> {
        let index = index as usize;
        if index >= self.len() {
            return None;
        }
        let (chunk, offset) = chunk_position(index);
        let base = self.chunks[chunk].load(Ordering::Acquire);
        // Publication order (chunk store before the `published` bump, both
        // release) guarantees a non-null chunk for any index below `len`.
        Some(unsafe { &*base.add(offset) })
    }

    /// Publishes one more slot, allocating its chunk if this index is the
    /// first to land there. Caller holds the registry mutex (growth is
    /// single-writer) while readers stay lock-free.
    fn grow(&self) -> u32 {
        let index = self.published.load(Ordering::Relaxed);
        let (chunk, _) = chunk_position(index);
        if self.chunks[chunk].load(Ordering::Relaxed).is_null() {
            let slots: Box<[Slot<X, M>]> = (0..chunk_len(chunk)).map(|_| Slot::new()).collect();
            let base = Box::leak(slots).as_mut_ptr();
            self.chunks[chunk].store(base, Ordering::Release);
        }
        self.published.store(index + 1, Ordering::Release);
        index as u32
    }
}

impl<X, M> Drop for SlotArena<X, M> {
    fn drop(&mut self) {
        for (chunk, pointer) in self.chunks.iter().enumerate() {
            let base = pointer.load(Ordering::Relaxed);
            if !base.is_null() {
                drop(unsafe {
                    Box::from_raw(ptr::slice_from_raw_parts_mut(base, chunk_len(chunk)))
                });
            }
        }
    }
}

/// Resources moved out of a dead process, freed when this value is dropped,
/// which the reclaim sites do only after any lock is released. Each field
/// is an RAII owner, so dropping a `Reclaim` runs the execution state's
/// drop glue (native: unmaps the stack, releases the spawn config) and drains
/// the mailbox (running each message's drop glue).
///
/// The fields are never read by name (they exist purely so their own `Drop`
/// runs at this controlled point), hence `allow(dead_code)`.
#[allow(dead_code)]
pub struct Reclaim<X, M> {
    execution: Option<X>,
    mailbox: Mailbox<M>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessState {
    /// Newly spawned, not yet entered by any worker.
    Created,
    /// Ready to run, waiting for a worker to pick it up.
    Runnable,
    /// Currently executing on a worker.
    Running,
    /// Waiting for a message (via `receive`). Becomes `Runnable` when a
    /// message arrives or its deadline expires.
    Blocked,
    /// Waiting for I/O readiness on a file descriptor. The reactor promotes
    /// this to `Runnable` when the fd is ready.
    WaitingIO,
    /// Function returned. Process will not be scheduled again.
    Dead,
}

/// A process's scheduling priority. The wire weight is an explicit ABI
/// contract (0 = `Low`, 1 = `Normal`, 2 = `High`) decoded by
/// `from_index`. The Rust enum's order fixes the ready-queue index via
/// `as usize`. Default is `Normal`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
}

impl Priority {
    /// The priority for a wire/FFI tag index, clamping unknown values to
    /// `Normal` so a malformed boundary value can't panic the scheduler.
    pub fn from_index(index: i64) -> Self {
        match index {
            0 => Self::Low,
            2 => Self::High,
            _ => Self::Normal,
        }
    }

    /// Reductions granted per scheduling quantum at this priority: one is
    /// spent per `YieldCheck`, bounding how long a process runs before
    /// yielding. `Normal` mirrors BEAM's 2000-reduction quantum.
    pub fn budget(&self) -> u32 {
        match self {
            Self::Low => 1_000,
            Self::Normal => 2_000,
            Self::High => 4_000,
        }
    }
}

/// Why a process terminated, recorded on its slot and read when the death
/// edge stages exit notices. The wire code is an ABI contract (0 =
/// `Normal`, 1 = `Shutdown`, 2 = `Killed`, 3 = `Crashed`) decoded by
/// `from_index`: `Normal`/`Shutdown` mirror the stop reason a process
/// returns, `Killed` marks a forced kill, and `Crashed` is fault capture.
/// Default is `Normal`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExitReason {
    #[default]
    Normal,
    Shutdown,
    Killed,
    Crashed,
}

impl ExitReason {
    /// The reason for a wire/FFI code, clamping unknown values to `Normal`
    /// so a malformed boundary value can't panic the scheduler.
    pub fn from_index(index: i64) -> Self {
        match index {
            1 => Self::Shutdown,
            2 => Self::Killed,
            3 => Self::Crashed,
            _ => Self::Normal,
        }
    }
}

/// The panic message and pre-rendered backtrace for a [`ExitReason::Crashed`]
/// death. Carried alongside the `Copy` `ExitReason` on the slot rather than
/// inside the discriminant, so the heavy strings travel only on an actual crash.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CrashInfo {
    pub message: String,
    pub backtrace: String,
}

/// A staged `ExitSignal` delivery: everything an adapter needs to
/// synthesize the watcher's message outside any lock. Staged by the death
/// edge and [`ProcessTable::monitor`], drained via
/// [`ProcessTable::take_exit_notices`].
pub struct ExitNotice {
    /// The dying process's crash capture, present only for `Crashed`.
    pub crash_info: Option<CrashInfo>,
    pub reason: ExitReason,
    /// The process that exited.
    pub target: Pid,
    /// The monitoring process the signal is addressed to.
    pub watcher: Pid,
}

/// One registered monitor: `watcher` observes `target`'s exit. A flat
/// list (not a per-target map) so `new` stays `const`. Monitor counts
/// are small, so the linear scans are cheap.
struct MonitorEntry {
    target: Pid,
    token: i64,
    watcher: Pid,
}

/// Whether `from -> to` is a legal process lifecycle edge.
///
/// Built from the audited transition sites. A worker claims a fresh or woken
/// process (`Created`/`Runnable -> Running`). A running process blocks on a
/// message or I/O (`Running -> Blocked`/`WaitingIO`). A wake re-arms a parked
/// process (`Blocked`/`WaitingIO -> Runnable`). A running process voluntarily
/// yields back to the ready queue (`Running -> Runnable`, cooperative
/// preemption). Any live process can die via return (`Running -> Dead`) or
/// a kill from another worker (`* -> Dead`). No legal edge is a self-edge,
/// because every edge is a guarded CAS from its expected source state.
pub(crate) fn is_legal_transition(from: ProcessState, to: ProcessState) -> bool {
    use ProcessState::*;
    matches!(
        (from, to),
        (Created | Runnable, Running)
            | (Running, Blocked | WaitingIO | Dead)
            | (Running | Blocked | WaitingIO, Runnable)
            | (Created | Runnable | Blocked | WaitingIO, Dead)
    )
}

/// Monotonic invariant counters bumped at the [`ProcessTable`] chokepoints.
/// `violations` is the machine oracle (must stay zero). The rest are coverage
/// evidence: a kill-storm fixture observing `parks_refused > 0` knows
/// the kill-vs-park window was actually hit, not merely survived. Exposed to
/// lang fixtures via the adapter's `koja_rt_sched_violations` /
/// `koja_rt_parks_refused`. Read via [`ProcessTable::counters`], a snapshot.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScheduleCounters {
    /// Kills that found the target `on_cpu` and deferred reclaim.
    pub kills_deferred: u64,
    /// Parks refused because the target was already `Dead` (or stale).
    pub parks_refused: u64,
    /// Ready-queue entries rejected by `try_claim` (killed, already
    /// resumed, or still `on_cpu`).
    pub stale_claims_skipped: u64,
    /// Fired deadline entries rejected by `promote_expired`.
    pub stale_deadlines_skipped: u64,
    /// Envelopes bounced off a dead or stale target.
    pub undeliverable_envelopes: u64,
    /// Edges requested from an unexpected source state. Always zero in a
    /// correct runtime. Counted (not just debug-asserted) so release builds
    /// can detect ordering bugs too.
    pub violations: u64,
}

/// The counters' live (relaxed-atomic) form.
struct AtomicCounters {
    kills_deferred: AtomicU64,
    parks_refused: AtomicU64,
    stale_claims_skipped: AtomicU64,
    stale_deadlines_skipped: AtomicU64,
    undeliverable_envelopes: AtomicU64,
    violations: AtomicU64,
}

impl AtomicCounters {
    const fn new() -> Self {
        Self {
            kills_deferred: AtomicU64::new(0),
            parks_refused: AtomicU64::new(0),
            stale_claims_skipped: AtomicU64::new(0),
            stale_deadlines_skipped: AtomicU64::new(0),
            undeliverable_envelopes: AtomicU64::new(0),
            violations: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> ScheduleCounters {
        ScheduleCounters {
            kills_deferred: self.kills_deferred.load(Ordering::Relaxed),
            parks_refused: self.parks_refused.load(Ordering::Relaxed),
            stale_claims_skipped: self.stale_claims_skipped.load(Ordering::Relaxed),
            stale_deadlines_skipped: self.stale_deadlines_skipped.load(Ordering::Relaxed),
            undeliverable_envelopes: self.undeliverable_envelopes.load(Ordering::Relaxed),
            violations: self.violations.load(Ordering::Relaxed),
        }
    }
}

/// Bumps one counter field by one, relaxed.
macro_rules! count {
    ($table:expr, $field:ident) => {
        $table.counters.$field.fetch_add(1, Ordering::Relaxed)
    };
}

/// The runtime's lifecycle mode. `Draining` is entered on `SIGTERM`: new
/// spawns are refused and the adapter arms a grace deadline in its
/// `TimerService`, after which any straggler is force-killed. Resets to
/// `Running` by construction (a fresh table per program / per run).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Running,
    Draining,
}

/// The cold global state behind the registry mutex. Spawn/teardown
/// decisions and monitor aliveness linearize on this lock (invariants 5
/// and 6 in the design doc).
struct Registry {
    /// Live children by parent: the kill-cascade's reverse index.
    /// Maintained at spawn and death (both under this hold), so a death
    /// edge stages its children without scanning the arena.
    children: BTreeMap<Pid, BTreeSet<Pid>>,
    /// Indices of free slots available for reuse.
    free: Vec<u32>,
    /// Runtime lifecycle mode, `Draining` once a `SIGTERM` has been seen.
    mode: Mode,
    /// Registered monitors, evicted when either endpoint dies.
    monitors: Vec<MonitorEntry>,
    /// Next monitor token to mint. Starts at 1 so 0 is never valid.
    next_monitor_token: i64,
    /// `ExitSignal` deliveries staged by death edges and `monitor`,
    /// drained by [`ProcessTable::take_exit_notices`].
    pending_exit_notices: Vec<ExitNotice>,
    /// Kill-cascade targets staged by death edges (a dead process's live
    /// children), drained by [`ProcessTable::take_pending_kills`].
    pending_kills: Vec<Pid>,
}

/// A newly-runnable process for the caller to enqueue: the wake fact
/// every waking method returns instead of pushing into a table-owned
/// queue. `priority` selects the ready-queue level.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Wake {
    pub pid: Pid,
    pub priority: Priority,
}

/// The outcome of [`ProcessTable::deliver`]: the wake fact (when the
/// delivery promoted a parked receiver) plus any message the caller must
/// drop after routing the wake: the original when the target is gone or
/// dead, or a stale reply displaced from the reply slot.
pub struct Delivery<M> {
    pub leftover: Option<M>,
    pub wake: Option<Wake>,
}

/// The outcome of [`ProcessTable::deliver_reply`]: `delivered` mirrors
/// the `Delivered`/`Expired` wire status, decided in the same hold as
/// the delivery so it is linearizable against the caller's timeout.
pub struct ReplyDelivery<M> {
    pub delivered: bool,
    pub leftover: Option<M>,
    pub wake: Option<Wake>,
}

/// The outcome of a check-then-park ([`ProcessTable::receive_or_park`] /
/// [`ProcessTable::take_reply_or_park`]), decided in one slot hold so a
/// delivery can never slip between the check and the park.
pub enum MailPark<M> {
    /// A message was already waiting, so nothing was parked.
    Ready(M),
    /// The mailbox part was empty and the process is parked.
    Parked,
    /// The park was refused over a kill tombstone. The caller should
    /// still yield, and the owner reclaims at switch-out.
    Refused,
    /// A reply was waiting but its token doesn't match the in-flight
    /// call: a leftover from an earlier timed-out call. The caller drops
    /// it and retries.
    Stale(M),
}

/// The outcome of [`ProcessTable::try_park_io`], decided in one slot hold
/// so a system message can never slip in between the check and the park.
pub enum IoPark {
    /// Parked as `WaitingIO`. The caller registers the fd.
    Parked,
    /// Refused over a kill tombstone: no waiter to wake, don't register.
    Refused,
    /// A system/lifecycle message is queued: the wait must be
    /// interrupted, not started.
    SystemMail,
}

/// What became of a process at its owner's switch-out
/// ([`ProcessTable::after_switch`]).
pub enum SwitchOutcome<X, M> {
    /// Parked (`Blocked`/`WaitingIO`). A wake will re-enqueue it.
    Parked,
    /// Dead: the slot was reclaimed. Drop the resources off-lock.
    Reclaimed(Reclaim<X, M>),
    /// Runnable (yielded, or woken during the on-cpu window): the caller
    /// enqueues it.
    Requeue(Wake),
}

/// The scheduler's process store. Internally synchronized (see the module
/// doc), so every method takes `&self` and adapters share it directly: the
/// native driver as a plain `static`, cooperative drivers via `Rc`.
pub struct ProcessTable<X, M> {
    /// Count of `Running` + `WaitingIO` processes (park-timeout heuristic).
    active: AtomicUsize,
    /// Count of processes not yet `Dead` (shutdown when this hits zero).
    alive: AtomicUsize,
    arena: SlotArena<X, M>,
    /// Invariant counters, exposed to fixtures via the adapter.
    counters: AtomicCounters,
    /// First spawned process (the program entry). Drives signal delivery
    /// and the shutdown decision. `0` until the first spawn.
    main_pid: AtomicI64,
    registry: Mutex<Registry>,
    /// Lifecycle event rings, dumped at shutdown under `KOJA_SCHED_TRACE`.
    trace: SchedulerTrace,
}

impl<X, M: Message> ProcessTable<X, M> {
    pub const fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            alive: AtomicUsize::new(0),
            arena: SlotArena::new(),
            counters: AtomicCounters::new(),
            main_pid: AtomicI64::new(0),
            registry: Mutex::new(Registry {
                children: BTreeMap::new(),
                free: Vec::new(),
                mode: Mode::Running,
                monitors: Vec::new(),
                next_monitor_token: 1,
                pending_exit_notices: Vec::new(),
                pending_kills: Vec::new(),
            }),
            trace: SchedulerTrace::new(),
        }
    }

    /// A snapshot of the invariant counters.
    pub fn counters(&self) -> ScheduleCounters {
        self.counters.snapshot()
    }

    /// Recorded lifecycle events, merged across threads, oldest first.
    pub fn trace_entries(&self) -> Vec<TraceEntry> {
        self.trace.entries()
    }

    /// The program entry process, or `0` before the first spawn.
    pub fn main_pid(&self) -> Pid {
        self.main_pid.load(Ordering::Acquire)
    }

    /// The slot `pid` points into, with no generation check (each use
    /// validates against the lifecycle word itself). `None` only for an
    /// out-of-range index.
    fn slot(&self, pid: Pid) -> Option<(&Slot<X, M>, u32)> {
        let (index, generation) = decode(pid);
        Some((self.arena.get(index)?, generation))
    }

    /// Whether `pid` resolves to a live (non-`Dead`) process. Stale and freed
    /// PIDs are not alive.
    pub fn is_alive(&self, pid: Pid) -> bool {
        self.slot(pid)
            .is_some_and(|(slot, generation)| slot.lifecycle.load().is_alive(generation))
    }

    /// Runs `f` over `pid`'s hot state while holding its slot mutex, after
    /// validating the generation. `None` for a stale or vacated PID.
    fn with_hot<R>(
        &self,
        pid: Pid,
        f: impl FnOnce(&Slot<X, M>, &mut HotState<M>) -> R,
    ) -> Option<R> {
        let (slot, generation) = self.slot(pid)?;
        let _held = Held::acquire();
        let mut hot = slot.hot.lock().unwrap();
        if slot.lifecycle.load().generation != generation {
            return None;
        }
        Some(f(slot, &mut hot))
    }

    /// Runs `f` over the registry while holding its mutex.
    fn with_registry<R>(&self, f: impl FnOnce(&mut Registry) -> R) -> R {
        let _held = Held::acquire();
        let mut registry = self.registry.lock().unwrap();
        f(&mut registry)
    }

    /// Whether `pid` has a pending system/lifecycle message: the signal a
    /// blocking I/O wait checks to decide whether it was interrupted.
    pub fn has_system_mail(&self, pid: Pid) -> bool {
        self.with_hot(pid, |_, hot| hot.mailbox.has_system())
            .unwrap_or(false)
    }

    /// Sets `pid`'s scheduling priority. Takes effect at the next enqueue,
    /// so an entry already queued at the old level is not moved.
    pub fn set_priority(&self, pid: Pid, priority: Priority) {
        if let Some((slot, generation)) = self.slot(pid)
            && slot.lifecycle.load().generation == generation
        {
            slot.priority.store(priority as u8, Ordering::Relaxed);
        }
    }

    /// Runs `f` over `pid`'s execution state. `None` for a stale PID.
    ///
    /// # Safety
    /// The caller must be the slot's current unique owner: the `on_cpu`
    /// claim holder (between a successful [`try_claim`](Self::try_claim)
    /// and the matching [`after_switch`](Self::after_switch)). The
    /// claim's acquire/release pair on the lifecycle word is what makes
    /// this access data-race free.
    pub unsafe fn with_execution<R>(&self, pid: Pid, f: impl FnOnce(&mut X) -> R) -> Option<R> {
        let (slot, generation) = self.slot(pid)?;
        if slot.lifecycle.load().generation != generation {
            return None;
        }
        let execution = unsafe { &mut *slot.execution.0.get() };
        execution.as_mut().map(f)
    }

    /// Registers a new process (with its executor `execution` state) in a
    /// free or freshly grown slot and returns its packed PID. The caller
    /// enqueues it (a fresh spawn is `Created` at `Normal` priority).
    /// `parent` is the spawning process, `None` only for the entry process.
    ///
    /// Refuses over the spawner's tombstone, handing `execution` back for
    /// the caller to drop off-lock: a kill can land while the spawner is
    /// mid-run (`* -> Dead` is a legal cross-worker edge), and its kill
    /// cascade only covers children that exist at the death edge, so a
    /// child registered after it would escape the cascade as an orphan.
    /// The check happens under the registry mutex, where the cascade's
    /// child staging also runs, so the two cannot interleave (invariant 6).
    pub fn spawn(&self, execution: X, parent: Option<Pid>) -> Result<Pid, X> {
        self.with_registry(|registry| {
            if parent.is_some_and(|parent| !self.is_alive(parent)) {
                return Err(execution);
            }
            let index = registry.free.pop().unwrap_or_else(|| self.arena.grow());
            let slot = self.arena.get(index).expect("just-published slot");
            // A fresh slot's word is vacant at generation 0, and live
            // generations start at 1.
            let generation = slot.lifecycle.load().generation.max(1);
            let pid = encode(index, generation);

            slot.parent.store(parent.unwrap_or(0), Ordering::Relaxed);
            if let Some(parent) = parent {
                registry.children.entry(parent).or_default().insert(pid);
            }
            slot.priority
                .store(Priority::default() as u8, Ordering::Relaxed);
            // The spawner owns the vacant slot under the registry hold
            // (the index is off the freelist and unpublished), so the
            // execution write is exclusive. `occupy`'s release-store
            // publishes it to the eventual claimer.
            unsafe { *slot.execution.0.get() = Some(execution) };
            slot.lifecycle.occupy(generation);

            self.alive.fetch_add(1, Ordering::Relaxed);
            if self.main_pid.load(Ordering::Relaxed) == 0 {
                self.main_pid.store(pid, Ordering::Release);
            }
            Ok(pid)
        })
    }

    /// Marks `pid` `Running` and `on_cpu` when it is a fresh claim (alive,
    /// not already `on_cpu`, `Created`/`Runnable`), granting its quantum's
    /// reduction budget. Lock-free. Returns `false` for a stale ready
    /// entry (killed, already resumed, or still `on_cpu`), counting the
    /// skip so the caller pops the next candidate.
    pub fn try_claim(&self, pid: Pid) -> bool {
        let Some((slot, generation)) = self.slot(pid) else {
            count!(self, stale_claims_skipped);
            return false;
        };
        let from = slot.lifecycle.load().state;
        if !slot.lifecycle.try_claim(generation) {
            count!(self, stale_claims_skipped);
            return false;
        }
        slot.reductions
            .store(slot.priority().budget(), Ordering::Relaxed);
        self.active.fetch_add(1, Ordering::Relaxed);
        self.trace.record(
            pid,
            TraceEvent::Transition {
                from: from.unwrap_or(ProcessState::Runnable),
                to: ProcessState::Running,
            },
        );
        true
    }

    /// The reductions `pid` was granted this quantum, or 0 for a stale PID.
    /// Each adapter reads this once per resume to seed its own lock-free
    /// thread-local decrement counter (`YieldCheck` spends from that, not
    /// from the slot).
    pub fn reductions_left(&self, pid: Pid) -> u32 {
        match self.slot(pid) {
            Some((slot, generation)) if slot.lifecycle.load().generation == generation => {
                slot.reductions.load(Ordering::Relaxed)
            }
            _ => 0,
        }
    }

    /// Parks `pid` as `Blocked`, recording which part of its mailbox it waits
    /// on and an optional wake deadline. Refuses (returning `false` without
    /// touching the state) when the process is dead or stale, because a kill
    /// can land while the process is mid-run on another worker (`* -> Dead`
    /// is a legal cross-worker edge), and parking over the tombstone would
    /// resurrect it. A refused caller should still yield: the owner sees
    /// `Dead` at switch-out and reclaims the slot, so the frame never resumes.
    ///
    /// The table only records `deadline`. After a successful park the
    /// caller arms the actual timer entry in its `TimerService`, outside
    /// any table lock.
    pub fn try_park(&self, pid: Pid, target: WaitTarget, deadline: Option<Instant>) -> bool {
        self.with_hot(pid, |slot, hot| {
            if !self.park_edge(pid, slot, ProcessState::Blocked) {
                return false;
            }
            hot.deadline = deadline;
            hot.waiting = target;
            self.active.fetch_sub(1, Ordering::Relaxed);
            true
        })
        .unwrap_or_else(|| {
            self.note_refused_park(pid);
            false
        })
    }

    /// Parks `pid` as `WaitingIO`, with the same kill-tombstone refusal as
    /// [`try_park`](Self::try_park), unless a system message is already
    /// queued (checked in the same hold, so a signal can't slip between
    /// the check and the park).
    pub fn try_park_io(&self, pid: Pid) -> IoPark {
        self.with_hot(pid, |slot, hot| {
            if hot.mailbox.has_system() {
                return IoPark::SystemMail;
            }
            if self.park_edge(pid, slot, ProcessState::WaitingIO) {
                IoPark::Parked
            } else {
                IoPark::Refused
            }
        })
        .unwrap_or_else(|| {
            self.note_refused_park(pid);
            IoPark::Refused
        })
    }

    /// The `Running -> Blocked`/`WaitingIO` park edge, under the caller's
    /// slot hold. `false` (counted) over a kill tombstone.
    fn park_edge(&self, pid: Pid, slot: &Slot<X, M>, to: ProcessState) -> bool {
        let (_, generation) = decode(pid);
        let word = slot.lifecycle.load();
        if !word.is_alive(generation) {
            self.note_refused_park(pid);
            return false;
        }
        if !slot
            .lifecycle
            .try_edge(generation, ProcessState::Running, to)
        {
            // Alive but not Running: no live park site produces this.
            count!(self, violations);
            debug_assert!(false, "park of pid {pid} from {:?}", word.state);
            return false;
        }
        self.trace.record(
            pid,
            TraceEvent::Transition {
                from: ProcessState::Running,
                to,
            },
        );
        true
    }

    fn note_refused_park(&self, pid: Pid) {
        count!(self, parks_refused);
        self.trace.record(pid, TraceEvent::ParkRefused);
    }

    /// Pops the next received message (system before business) for `pid`,
    /// or `None` when its queues are empty or the PID is stale.
    pub fn pop_received(&self, pid: Pid) -> Option<M> {
        self.with_hot(pid, |_, hot| hot.mailbox.pop_received())?
    }

    /// Takes the pending reply from `pid`'s one-shot reply slot, if one
    /// has landed.
    pub fn take_reply(&self, pid: Pid) -> Option<M> {
        self.with_hot(pid, |_, hot| hot.mailbox.take_reply())?
    }

    /// Pops the next received message, or parks `pid` on its receive
    /// queues, in one slot hold (so a delivery can't slip between the
    /// empty check and the park and leave the process parked forever).
    pub fn receive_or_park(&self, pid: Pid, deadline: Option<Instant>) -> MailPark<M> {
        self.with_hot(pid, |slot, hot| {
            if let Some(message) = hot.mailbox.pop_received() {
                return MailPark::Ready(message);
            }
            if !self.park_edge(pid, slot, ProcessState::Blocked) {
                return MailPark::Refused;
            }
            hot.deadline = deadline;
            hot.waiting = WaitTarget::Receive;
            self.active.fetch_sub(1, Ordering::Relaxed);
            MailPark::Parked
        })
        .unwrap_or_else(|| {
            self.note_refused_park(pid);
            MailPark::Refused
        })
    }

    /// Takes the reply for the in-flight call `token`, or parks `pid` on
    /// its reply slot, in one hold. A matching reply also clears the
    /// awaited token and deadline (the call completed). A mismatched one
    /// is handed back as [`MailPark::Stale`] for the caller to drop and
    /// retry.
    pub fn take_reply_or_park(
        &self,
        pid: Pid,
        token: i64,
        deadline: Option<Instant>,
    ) -> MailPark<M> {
        self.with_hot(pid, |slot, hot| {
            if let Some(reply) = hot.mailbox.take_reply() {
                if reply.reply_token() == token {
                    hot.awaiting_reply = None;
                    hot.deadline = None;
                    return MailPark::Ready(reply);
                }
                return MailPark::Stale(reply);
            }
            if !self.park_edge(pid, slot, ProcessState::Blocked) {
                return MailPark::Refused;
            }
            hot.deadline = deadline;
            hot.waiting = WaitTarget::Reply;
            self.active.fetch_sub(1, Ordering::Relaxed);
            MailPark::Parked
        })
        .unwrap_or_else(|| {
            self.note_refused_park(pid);
            MailPark::Refused
        })
    }

    /// Clears `pid`'s recorded wake deadline, so a later fired timer
    /// entry fails the [`promote_expired`](Self::promote_expired)
    /// re-validation. Wake paths pair this with the `TimerService`
    /// cancel, made outside any table lock.
    pub fn clear_deadline(&self, pid: Pid) {
        self.with_hot(pid, |_, hot| hot.deadline = None);
    }

    /// Records that `pid` is now waiting on the reply for call `token`. Set
    /// when the token is minted (before the request is sent) so a reply can
    /// never race ahead of the caller registering its interest.
    pub fn set_awaiting_reply(&self, pid: Pid, token: i64) {
        self.with_hot(pid, |_, hot| hot.awaiting_reply = Some(token));
    }

    /// Clears `pid`'s awaited-reply token once its call completes (reply
    /// received or timed out). A no-op for a stale PID.
    pub fn clear_awaiting_reply(&self, pid: Pid) {
        self.with_hot(pid, |_, hot| hot.awaiting_reply = None);
    }

    /// Whether `pid` is alive and still waiting on the reply for `token`.
    pub fn is_awaiting_reply(&self, pid: Pid, token: i64) -> bool {
        self.with_hot(pid, |slot, hot| {
            let (_, generation) = decode(pid);
            slot.lifecycle.load().is_alive(generation) && hot.awaiting_reply == Some(token)
        })
        .unwrap_or(false)
    }

    /// Records why `pid` is terminating, to be read when the death edge
    /// stages exit notices. Set before marking the process dead. A no-op
    /// for a stale PID. A process that sets no reason dies `Normal`.
    pub fn set_exit_reason(&self, pid: Pid, reason: ExitReason) {
        self.with_hot(pid, |_, hot| hot.exit_reason = reason);
    }

    /// Records a crashing process's capture, paired with
    /// `set_exit_reason(pid, ExitReason::Crashed)` at the unwind site. A
    /// no-op for a stale PID.
    pub fn set_crash_info(&self, pid: Pid, crash_info: CrashInfo) {
        self.with_hot(pid, |_, hot| hot.crash_info = Some(crash_info));
    }

    /// The crash capture recorded for `pid`, if it died `Crashed`.
    pub fn crash_info(&self, pid: Pid) -> Option<CrashInfo> {
        self.with_hot(pid, |_, hot| hot.crash_info.clone())?
    }

    /// The process that spawned `pid`. `None` for the entry process
    /// and for stale PIDs.
    pub fn parent(&self, pid: Pid) -> Option<Pid> {
        let (slot, generation) = self.slot(pid)?;
        if slot.lifecycle.load().generation != generation {
            return None;
        }
        match slot.parent.load(Ordering::Relaxed) {
            0 => None,
            parent => Some(parent),
        }
    }

    /// Routes `message` into a process's mailbox (see [`Mailbox::push`]),
    /// waking the process if it is parked waiting on the part of the mailbox
    /// this message satisfies. The caller enqueues the returned wake fact
    /// and drops the leftover, both after this returns (off the slot lock).
    pub fn deliver(&self, pid: Pid, message: M) -> Delivery<M> {
        let outcome = match self.slot(pid) {
            Some((slot, generation)) => {
                let _held = Held::acquire();
                let mut hot = slot.hot.lock().unwrap();
                if slot.lifecycle.load().is_alive(generation) {
                    Ok(self.deliver_locked(pid, slot, &mut hot, message))
                } else {
                    Err(message)
                }
            }
            None => Err(message),
        };
        outcome.unwrap_or_else(|message| {
            // Stale or dead target: bounce the message back to the caller.
            count!(self, undeliverable_envelopes);
            self.trace.record(pid, TraceEvent::Undeliverable);
            Delivery {
                leftover: Some(message),
                wake: None,
            }
        })
    }

    /// Slots `message` as the reply for `pid`'s in-flight call `token`,
    /// checking and delivering in one hold so the `Delivered`/`Expired`
    /// answer is linearizable against the caller's timeout.
    pub fn deliver_reply(&self, pid: Pid, token: i64, message: M) -> ReplyDelivery<M> {
        let outcome = match self.slot(pid) {
            Some((slot, generation)) => {
                let _held = Held::acquire();
                let mut hot = slot.hot.lock().unwrap();
                if slot.lifecycle.load().is_alive(generation) && hot.awaiting_reply == Some(token) {
                    Ok(self.deliver_locked(pid, slot, &mut hot, message))
                } else {
                    Err(message)
                }
            }
            None => Err(message),
        };
        match outcome {
            Ok(delivery) => ReplyDelivery {
                delivered: true,
                leftover: delivery.leftover,
                wake: delivery.wake,
            },
            // The caller already gave up (or died): the reply is handed
            // back for the sender to drop.
            Err(message) => ReplyDelivery {
                delivered: false,
                leftover: Some(message),
                wake: None,
            },
        }
    }

    /// The delivery body, under the caller's slot hold with liveness
    /// already validated: route by tag, wake a matching parked waiter.
    fn deliver_locked(
        &self,
        pid: Pid,
        slot: &Slot<X, M>,
        hot: &mut HotState<M>,
        message: M,
    ) -> Delivery<M> {
        let target = Mailbox::target_of(&message);
        let is_system = matches!(message.tag(), Tag::Lifecycle);
        // Wake a `Blocked` receiver for the matching queue, or a
        // `WaitingIO` process for a system/lifecycle signal so blocking
        // I/O can be interrupted.
        let wake_from = match slot.lifecycle.load().state {
            Some(ProcessState::Blocked) if hot.waiting == target => Some(ProcessState::Blocked),
            Some(ProcessState::WaitingIO) if is_system => Some(ProcessState::WaitingIO),
            _ => None,
        };
        let leftover = hot.mailbox.push(message);
        self.trace.record(pid, TraceEvent::Delivered);
        let wake = wake_from.and_then(|from| self.wake_edge(pid, slot, from));
        Delivery { leftover, wake }
    }

    /// Wakes `pid` for a fired deadline entry, re-validating it against
    /// the live state first. The entry is stale (counted, skipped) when
    /// the process was woken by a message, resumed, re-blocked with a
    /// different deadline, or died between the fire and this apply. The
    /// re-validation is what lets the `TimerService` fire under its own
    /// lock without ever holding a table lock.
    pub fn promote_expired(&self, pid: Pid, fire_at: Instant) -> Option<Wake> {
        let wake = self.with_hot(pid, |slot, hot| {
            let (_, generation) = decode(pid);
            let word = slot.lifecycle.load();
            let expired = word.generation == generation
                && word.state == Some(ProcessState::Blocked)
                && hot.deadline == Some(fire_at);
            if !expired {
                return None;
            }
            hot.deadline = None;
            self.wake_edge(pid, slot, ProcessState::Blocked)
        });
        let wake = wake.flatten();
        if wake.is_none() {
            count!(self, stale_deadlines_skipped);
        }
        wake
    }

    /// Promotes a process from `WaitingIO` to `Runnable` if (and only if)
    /// it is still parked there: the reactor's `io_block` wake. The state
    /// guard is essential, because a concurrent system wake or kill may
    /// have already moved it.
    pub fn promote_io_waiter(&self, pid: Pid) -> Option<Wake> {
        self.with_hot(pid, |slot, _hot| {
            let (_, generation) = decode(pid);
            let word = slot.lifecycle.load();
            if word.generation != generation || word.state != Some(ProcessState::WaitingIO) {
                return None;
            }
            self.wake_edge(pid, slot, ProcessState::WaitingIO)
        })?
    }

    /// The `Blocked`/`WaitingIO -> Runnable` wake edge, under the caller's
    /// slot hold (which pins the state, so the CAS only ever retries over
    /// concurrent `on_cpu` traffic).
    fn wake_edge(&self, pid: Pid, slot: &Slot<X, M>, from: ProcessState) -> Option<Wake> {
        let (_, generation) = decode(pid);
        if !slot
            .lifecycle
            .try_edge(generation, from, ProcessState::Runnable)
        {
            debug_assert!(false, "wake edge lost its pinned source state");
            return None;
        }
        if from == ProcessState::WaitingIO {
            self.active.fetch_sub(1, Ordering::Relaxed);
        }
        self.trace.record(
            pid,
            TraceEvent::Transition {
                from,
                to: ProcessState::Runnable,
            },
        );
        Some(Wake {
            pid,
            priority: slot.priority(),
        })
    }

    /// Voluntarily returns a running process toward the ready queue
    /// (cooperative preemption). No-op unless `pid` is currently `Running`.
    /// Only marks the process `Runnable`: the actual re-enqueue happens
    /// via [`after_switch`](Self::after_switch)'s `Requeue` once the
    /// owner releases `on_cpu`.
    pub fn yield_running(&self, pid: Pid) {
        let Some((slot, generation)) = self.slot(pid) else {
            return;
        };
        if slot
            .lifecycle
            .try_edge(generation, ProcessState::Running, ProcessState::Runnable)
        {
            self.active.fetch_sub(1, Ordering::Relaxed);
            self.trace.record(
                pid,
                TraceEvent::Transition {
                    from: ProcessState::Running,
                    to: ProcessState::Runnable,
                },
            );
        }
    }

    /// After a process yields back to its driver: releases the `on_cpu`
    /// claim and routes the process by the state observed at the release.
    /// The caller persists any executor execution state (native: the saved
    /// `sp`) via [`with_execution`](Self::with_execution) *before* calling
    /// this, so the release publishes it to the next claimer.
    pub fn after_switch(&self, pid: Pid) -> SwitchOutcome<X, M> {
        let Some((slot, generation)) = self.slot(pid) else {
            return SwitchOutcome::Parked;
        };
        match slot.lifecycle.release(generation) {
            Some(ProcessState::Created | ProcessState::Runnable) => SwitchOutcome::Requeue(Wake {
                pid,
                priority: slot.priority(),
            }),
            // The release left `Dead` off-cpu: this owner is the unique
            // reclaimer (a racing kill saw `on_cpu` and deferred, and a
            // second kill sees `Dead` and no-ops).
            Some(ProcessState::Dead) => SwitchOutcome::Reclaimed(self.reclaim(pid, slot)),
            _ => SwitchOutcome::Parked,
        }
    }

    /// The "last resort" termination primitive: marks `pid` `Dead`
    /// (recording [`ExitReason::Killed`]), runs the death edge's registry
    /// pass, and reclaims its slot, returning the detached resources for
    /// the caller to drop off-lock. `None` when the target was already
    /// dead/stale, or when it is still `on_cpu`: reclaiming a stack a
    /// worker is running on would be a use-after-free, so the owning
    /// worker reclaims at switch-out instead (it sees `Dead` in
    /// [`after_switch`](Self::after_switch)). The mailbox rides along
    /// either way, so messages are freed exactly once.
    pub fn kill(&self, pid: Pid) -> Option<Reclaim<X, M>> {
        self.death_edge(pid, Some(ExitReason::Killed))?
    }

    /// Marks `pid` `Dead` with its recorded exit reason unless it already
    /// is (a racing kill may have won) or the slot is stale. The
    /// self-death edge: the caller is the running owner, so reclaim always
    /// defers to its own switch-out. Returns whether this call performed
    /// the transition.
    pub fn mark_dead_if_alive(&self, pid: Pid) -> bool {
        self.death_edge(pid, None).is_some()
    }

    /// The shared death edge. Under the slot hold: record `forced` (if
    /// any), CAS to `Dead`, snapshot what the registry pass needs, and
    /// reclaim an off-cpu target. Then, under the registry: evict
    /// monitors, stage notices and the child cascade, and return the
    /// freed index to the freelist.
    ///
    /// The outer `None` means the edge did not apply (already dead or
    /// stale), and the inner option is the off-cpu reclaim.
    fn death_edge(&self, pid: Pid, forced: Option<ExitReason>) -> Option<Option<Reclaim<X, M>>> {
        let (_, generation) = decode(pid);
        let staged = self.with_hot(pid, |slot, hot| {
            let prior = slot.lifecycle.try_kill(generation)?;
            if let Some(reason) = forced {
                hot.exit_reason = reason;
            }
            self.alive.fetch_sub(1, Ordering::Relaxed);
            if matches!(
                prior.state,
                Some(ProcessState::Running | ProcessState::WaitingIO)
            ) {
                self.active.fetch_sub(1, Ordering::Relaxed);
            }
            self.trace.record(
                pid,
                TraceEvent::Transition {
                    from: prior.state.unwrap_or(ProcessState::Runnable),
                    to: ProcessState::Dead,
                },
            );
            // Snapshot the registry pass's inputs before a reclaim resets
            // the hot state.
            let reason = hot.exit_reason;
            let crash_info = hot.crash_info.clone();
            let parent = slot.parent.load(Ordering::Relaxed);
            let reclaim = if prior.on_cpu {
                count!(self, kills_deferred);
                self.trace.record(pid, TraceEvent::KillDeferred);
                None
            } else {
                Some(self.reclaim_locked(pid, slot, hot))
            };
            Some((reason, crash_info, parent, reclaim))
        })??;
        let (reason, crash_info, parent, reclaim) = staged;

        self.with_registry(|registry| {
            self.notify_exit(registry, pid, parent, reason, crash_info);
            if reclaim.is_some() {
                registry.free.push(decode(pid).0);
            }
        });
        Some(reclaim)
    }

    /// Detaches a dead slot's resources and vacates it, under the caller's
    /// slot hold. The caller returns the index to the freelist afterwards,
    /// under the registry.
    fn reclaim_locked(&self, pid: Pid, slot: &Slot<X, M>, hot: &mut HotState<M>) -> Reclaim<X, M> {
        // The word is Dead with on_cpu clear and the slot mutex is held:
        // no claim exists and none can start, so this take is exclusive.
        let execution = unsafe { (*slot.execution.0.get()).take() };
        debug_assert!(execution.is_some(), "reclaiming a slot with no execution");
        let mailbox = std::mem::take(&mut hot.mailbox);
        // Reset the death-path fields now, so spawn's occupy needs no slot
        // hold ("free resets, spawn occupies").
        *hot = HotState::default();
        slot.lifecycle.vacate();
        self.trace.record(pid, TraceEvent::Freed);
        Reclaim { execution, mailbox }
    }

    /// [`reclaim_locked`](Self::reclaim_locked) plus its own slot hold and
    /// freelist return: the owner's switch-out reclaim.
    fn reclaim(&self, pid: Pid, slot: &Slot<X, M>) -> Reclaim<X, M> {
        let reclaim = {
            let _held = Held::acquire();
            let mut hot = slot.hot.lock().unwrap();
            self.reclaim_locked(pid, slot, &mut hot)
        };
        self.with_registry(|registry| registry.free.push(decode(pid).0));
        reclaim
    }

    /// The death edge's registry pass: stages one [`ExitNotice`] per
    /// monitor watching `pid`, evicts the dead process's monitor entries
    /// in both directions (as target and as watcher), stages the
    /// kill-cascade for its live children, and prunes the children
    /// index. Runs under the registry hold, which is what linearizes it
    /// against `monitor` (invariant 5) and `spawn` (invariant 6).
    fn notify_exit(
        &self,
        registry: &mut Registry,
        pid: Pid,
        parent: Pid,
        reason: ExitReason,
        crash_info: Option<CrashInfo>,
    ) {
        let mut watchers = Vec::new();
        registry.monitors.retain(|entry| {
            if entry.target == pid {
                watchers.push(entry.watcher);
            }
            entry.target != pid && entry.watcher != pid
        });
        for watcher in watchers {
            registry.pending_exit_notices.push(ExitNotice {
                crash_info: crash_info.clone(),
                reason,
                target: pid,
                watcher,
            });
        }
        // Stage the children for the cascade and drop out of the parent's
        // entry. Spawn also runs under this hold, so no child can register
        // concurrently and escape the staging. A staged child may still
        // lose a racing death of its own, which the cascade's `kill` skips.
        if let Some(children) = registry.children.remove(&pid) {
            registry.pending_kills.extend(children);
        }
        if let btree_map::Entry::Occupied(mut siblings) = registry.children.entry(parent) {
            siblings.get_mut().remove(&pid);
            if siblings.get().is_empty() {
                siblings.remove();
            }
        }
    }

    /// Registers `watcher`'s monitor on `target`, returning its token.
    /// Monitoring an already-dead PID stages an immediate [`ExitNotice`]
    /// instead, so the watcher receives an `ExitSignal` either way. The
    /// aliveness decision happens under the registry hold, the same hold
    /// the death edge's eviction pass takes, so exactly one notice is
    /// staged no matter how the two race (invariant 5). The caller drains
    /// [`take_exit_notices`](Self::take_exit_notices) afterwards.
    pub fn monitor(&self, watcher: Pid, target: Pid) -> i64 {
        let (token, registered) = self.with_registry(|registry| {
            let token = registry.next_monitor_token;
            registry.next_monitor_token += 1;
            if self.is_alive(target) {
                registry.monitors.push(MonitorEntry {
                    target,
                    token,
                    watcher,
                });
                (token, true)
            } else {
                (token, false)
            }
        });
        if registered {
            return token;
        }
        // Already dead: snapshot the recorded reason from the slot (a
        // reclaimed or recycled slot has lost it and reports `Normal`),
        // then stage the immediate notice.
        let (reason, crash_info) = self
            .with_hot(target, |slot, hot| {
                let (_, generation) = decode(target);
                let word = slot.lifecycle.load();
                (word.generation == generation && word.state == Some(ProcessState::Dead))
                    .then(|| (hot.exit_reason, hot.crash_info.clone()))
            })
            .flatten()
            .unwrap_or((ExitReason::Normal, None));
        self.with_registry(|registry| {
            registry.pending_exit_notices.push(ExitNotice {
                crash_info,
                reason,
                target,
                watcher,
            });
        });
        token
    }

    /// Removes the monitor identified by `token`, suppressing its
    /// `ExitSignal`. A no-op for an unknown or already-evicted token.
    pub fn demonitor(&self, token: i64) {
        self.with_registry(|registry| registry.monitors.retain(|entry| entry.token != token));
    }

    /// Drains every staged [`ExitNotice`] for the driver to synthesize
    /// and deliver.
    pub fn take_exit_notices(&self) -> Vec<ExitNotice> {
        self.with_registry(|registry| std::mem::take(&mut registry.pending_exit_notices))
    }

    /// Drains the staged kill-cascade targets. The driver kills each
    /// (staging any grandchildren in turn) and loops until empty: the
    /// staged-drain that keeps the hierarchy flat.
    pub fn take_pending_kills(&self) -> Vec<Pid> {
        self.with_registry(|registry| std::mem::take(&mut registry.pending_kills))
    }

    /// Whether any process is `Running` or `WaitingIO`.
    pub fn any_active(&self) -> bool {
        self.active.load(Ordering::Relaxed) != 0
    }

    /// Whether the runtime should tear down: no live processes remain, or the
    /// program entry process has died (or its slot is already reclaimed).
    pub fn should_shutdown(&self) -> bool {
        if self.alive.load(Ordering::Relaxed) == 0 {
            return true;
        }
        let main = self.main_pid();
        main != 0 && !self.is_alive(main)
    }

    /// Enters `Draining`, returning whether this call performed the
    /// switch so the adapter arms the grace deadline in its
    /// `TimerService` exactly once (a second `SIGTERM` neither re-arms
    /// nor extends the window).
    pub fn enter_draining(&self) -> bool {
        self.with_registry(|registry| {
            if registry.mode == Mode::Draining {
                return false;
            }
            registry.mode = Mode::Draining;
            true
        })
    }

    /// Whether the runtime is draining (a `SIGTERM` has been seen). New
    /// spawns are refused while this holds.
    pub fn is_draining(&self) -> bool {
        self.with_registry(|registry| registry.mode == Mode::Draining)
    }

    /// Force-kills every live process (recording [`ExitReason::Killed`]),
    /// returning their detached resources for the caller to drop after
    /// releasing any lock. The drain grace backstop: once the deadline
    /// passes, stragglers are killed so
    /// [`should_shutdown`](Self::should_shutdown) fires. A process still
    /// `on_cpu` is marked `Dead` and reclaimed by its worker on switch-out.
    pub fn kill_all(&self) -> Vec<Reclaim<X, M>> {
        self.live_pids()
            .into_iter()
            .filter_map(|pid| self.kill(pid))
            .collect()
    }

    /// The PIDs of all currently-live (non-`Dead`) processes.
    fn live_pids(&self) -> Vec<Pid> {
        (0..self.arena.len() as u32)
            .filter_map(|index| {
                let word = self.arena.get(index)?.lifecycle.load();
                word.state
                    .is_some_and(|state| state != ProcessState::Dead)
                    .then(|| encode(index, word.generation))
            })
            .collect()
    }
}

impl<X, M: Message> Default for ProcessTable<X, M> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Envelope, TAG_BUSINESS, TAG_LIFECYCLE, TAG_REPLY};
    use std::ptr;
    use std::time::Duration;

    /// A unit execution state: the table stores it opaquely, so tests need
    /// no real stack or entry function.
    type TestTable = ProcessTable<(), Envelope>;

    fn fake_spawn(table: &TestTable) -> Pid {
        table.spawn((), None).expect("no parent to refuse over")
    }

    fn fake_spawn_child(table: &TestTable, parent: Pid) -> Pid {
        table.spawn((), Some(parent)).expect("live parent")
    }

    /// Spawns a process and claims it, as a worker picking up the fresh
    /// spawn would: the process is `Running` and `on_cpu`.
    fn spawn_running(table: &TestTable) -> Pid {
        let pid = fake_spawn(table);
        assert!(table.try_claim(pid), "fresh spawn claims");
        pid
    }

    /// A minimal business envelope (empty payload, no glue).
    fn fake_envelope() -> Envelope {
        unsafe { Envelope::from_payload(TAG_BUSINESS, ptr::null(), 0, None) }
    }

    /// A minimal lifecycle/system envelope (empty payload, no glue).
    fn fake_lifecycle() -> Envelope {
        unsafe { Envelope::from_payload(TAG_LIFECYCLE, ptr::null(), 0, None) }
    }

    /// A minimal reply envelope carrying `token`, waking a
    /// `WaitTarget::Reply` waiter.
    fn fake_reply(token: i64) -> Envelope {
        let mut envelope = unsafe { Envelope::from_payload(TAG_REPLY, ptr::null(), 0, None) };
        envelope.reply_token = token;
        envelope
    }

    #[test]
    fn encode_decode_roundtrip() {
        for (index, generation) in [(0u32, 1u32), (5, 2), (0xFFFF, 0x1234)] {
            let pid = encode(index, generation);
            assert_eq!(decode(pid), (index, generation));
        }
    }

    #[test]
    fn chunk_position_partitions_every_index() {
        let mut expected = 0usize;
        for chunk in 0..6 {
            for offset in 0..chunk_len(chunk) {
                assert_eq!(chunk_position(expected), (chunk, offset));
                expected += 1;
            }
        }
    }

    #[test]
    fn first_pid_is_index_zero_generation_one() {
        let table = TestTable::new();
        let pid = fake_spawn(&table);
        assert_eq!(decode(pid), (0, 1));
        assert!(table.is_alive(pid));
        assert_eq!(table.main_pid(), pid);
    }

    #[test]
    fn spawn_refuses_over_the_spawners_tombstone() {
        let table = TestTable::new();
        let parent = spawn_running(&table);
        let sibling = fake_spawn_child(&table, parent);

        // A cross-worker kill lands while the parent is mid-run.
        assert!(table.kill(parent).is_none(), "on_cpu kill defers reclaim");
        // Its next spawn must refuse: the cascade already scanned for
        // children, so a later registration would escape as an orphan.
        assert_eq!(table.spawn((), Some(parent)), Err(()));
        assert!(table.is_alive(sibling), "the earlier child was staged");
        assert_eq!(table.take_pending_kills(), vec![sibling]);
    }

    #[test]
    fn free_then_spawn_reuses_slot_with_bumped_generation() {
        let table = TestTable::new();
        let first = spawn_running(&table);
        assert!(table.mark_dead_if_alive(first));
        assert!(matches!(
            table.after_switch(first),
            SwitchOutcome::Reclaimed(_)
        ));

        let second = fake_spawn(&table);
        assert_eq!(decode(second).0, decode(first).0, "slot index reused");
        assert_eq!(decode(second).1, decode(first).1 + 1, "generation bumped");
        assert!(!table.is_alive(first), "stale PID rejected");
        assert!(table.is_alive(second), "new PID resolves");
    }

    #[test]
    fn stale_generation_is_rejected() {
        let table = TestTable::new();
        let pid = fake_spawn(&table);
        let (index, generation) = decode(pid);
        let stale = encode(index, generation + 7);
        assert!(!table.is_alive(stale));
        assert!(!table.is_alive(0), "pid 0 never valid");
    }

    #[test]
    fn arena_growth_keeps_every_slot_live() {
        let table = TestTable::new();
        let pids: Vec<Pid> = (0..BASE_CHUNK * 3).map(|_| fake_spawn(&table)).collect();
        for (position, &pid) in pids.iter().enumerate() {
            assert!(table.is_alive(pid), "pid #{position} survived growth");
        }
        assert_eq!(pids.len(), table.arena.len());
    }

    #[test]
    fn try_claim_marks_fresh_and_rejects_stale() {
        let table = TestTable::new();
        let pid = fake_spawn(&table);
        assert!(table.try_claim(pid), "fresh runnable pid claims");
        assert_eq!(table.reductions_left(pid), Priority::Normal.budget());
        // Already on_cpu: a second claim is refused.
        assert!(!table.try_claim(pid), "on_cpu pid is not re-claimed");
        // Dead pid: refused.
        table.kill(pid);
        assert!(!table.try_claim(pid), "dead pid is not claimed");
    }

    #[test]
    fn stale_claim_skip_is_counted() {
        let table = TestTable::new();
        let pid = fake_spawn(&table);
        assert!(table.kill(pid).is_some(), "off-cpu kill reclaims inline");

        assert!(!table.try_claim(pid), "killed entry never claimed");
        assert_eq!(table.counters().stale_claims_skipped, 1);
    }

    #[test]
    fn park_refuses_killed_process() {
        // A cross-worker kill can land while the victim is mid-run. Its next
        // park must not resurrect it over the `Dead` tombstone.
        let table = TestTable::new();
        let pid = spawn_running(&table);
        assert!(table.mark_dead_if_alive(pid), "first kill wins");

        assert!(!table.try_park(pid, WaitTarget::Receive, None));
        assert!(matches!(table.try_park_io(pid), IoPark::Refused));
        assert!(matches!(
            table.receive_or_park(pid, None),
            MailPark::Refused
        ));
        assert_eq!(table.counters().parks_refused, 3);
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "park of pid")]
    fn park_from_an_unclaimed_state_asserts_in_debug() {
        let table = TestTable::new();
        let pid = fake_spawn(&table);
        // Created, never claimed: no live park site produces this edge.
        table.try_park(pid, WaitTarget::Receive, None);
    }

    #[test]
    fn kill_defers_reclaim_while_on_cpu() {
        let table = TestTable::new();
        let pid = spawn_running(&table);

        // The worker is mid-run on this stack: kill must not reclaim it.
        assert!(table.kill(pid).is_none(), "reclaim deferred to the worker");
        assert_eq!(table.counters().kills_deferred, 1);

        // The owning worker switches out, sees `Dead`, and reclaims.
        assert!(matches!(
            table.after_switch(pid),
            SwitchOutcome::Reclaimed(_)
        ));
        assert!(table.kill(pid).is_none(), "second kill is a no-op");
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn kill_reclaims_parked_process_directly() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        assert!(table.try_park(pid, WaitTarget::Receive, None));
        assert!(matches!(table.after_switch(pid), SwitchOutcome::Parked));

        assert!(table.kill(pid).is_some(), "off-cpu target frees here");
        assert_eq!(table.counters().kills_deferred, 0);
        assert!(!table.is_alive(pid), "slot reclaimed");
    }

    #[test]
    fn mark_dead_if_alive_is_idempotent() {
        let table = TestTable::new();
        let pid = spawn_running(&table);

        assert!(table.mark_dead_if_alive(pid));
        assert!(!table.mark_dead_if_alive(pid), "second kill is a no-op");
        assert!(!table.mark_dead_if_alive(0), "stale PID is a no-op");
        assert!(table.should_shutdown(), "alive count decremented once");
    }

    #[test]
    fn deliver_wakes_a_matching_receive_waiter() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        assert!(table.try_park(pid, WaitTarget::Receive, None));
        assert!(matches!(table.after_switch(pid), SwitchOutcome::Parked));

        let delivery = table.deliver(pid, fake_envelope());
        assert!(delivery.leftover.is_none());
        assert_eq!(
            delivery.wake,
            Some(Wake {
                pid,
                priority: Priority::Normal,
            }),
        );
        assert!(table.try_claim(pid), "the woken process claims");
    }

    #[test]
    fn deliver_does_not_wake_a_reply_waiter_with_business() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        assert!(table.try_park(pid, WaitTarget::Reply, None));

        let business = table.deliver(pid, fake_envelope());
        assert!(business.wake.is_none(), "business can't satisfy the wait");

        let reply = table.deliver(pid, fake_reply(1));
        assert!(reply.wake.is_some(), "a reply satisfies it");
    }

    #[test]
    fn undeliverable_envelope_bounces_and_counts() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        table.mark_dead_if_alive(pid);

        let delivery = table.deliver(pid, fake_envelope());
        assert!(
            delivery.leftover.is_some(),
            "caller must reclaim the envelope"
        );
        assert!(delivery.wake.is_none());
        assert_eq!(table.counters().undeliverable_envelopes, 1);
    }

    #[test]
    fn receive_or_park_pops_or_parks_in_one_hold() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        assert!(table.deliver(pid, fake_envelope()).wake.is_none());

        // A queued message short-circuits the park.
        assert!(matches!(
            table.receive_or_park(pid, None),
            MailPark::Ready(_)
        ));
        // Empty queues park.
        assert!(matches!(table.receive_or_park(pid, None), MailPark::Parked));
        assert!(table.deliver(pid, fake_envelope()).wake.is_some());
    }

    #[test]
    fn take_reply_or_park_matches_the_token() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        table.set_awaiting_reply(pid, 7);

        // No reply yet: park on the reply slot.
        assert!(matches!(
            table.take_reply_or_park(pid, 7, None),
            MailPark::Parked
        ));
        assert!(matches!(table.after_switch(pid), SwitchOutcome::Parked));
        // A stale reply (an earlier timed-out call's) is handed back.
        assert!(table.deliver(pid, fake_reply(6)).wake.is_some());
        assert!(table.try_claim(pid));
        assert!(matches!(
            table.take_reply_or_park(pid, 7, None),
            MailPark::Stale(_)
        ));
        // The matching reply completes the call and clears the token.
        let delivery = table.deliver_reply(pid, 7, fake_reply(7));
        assert!(delivery.delivered);
        assert!(matches!(
            table.take_reply_or_park(pid, 7, None),
            MailPark::Ready(_)
        ));
        assert!(!table.is_awaiting_reply(pid, 7), "match cleared the token");
    }

    #[test]
    fn deliver_reply_expires_when_the_caller_gave_up() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        table.set_awaiting_reply(pid, 3);
        table.clear_awaiting_reply(pid);

        let delivery = table.deliver_reply(pid, 3, fake_reply(3));
        assert!(!delivery.delivered);
        assert!(
            delivery.leftover.is_some(),
            "the expired reply is handed back to drop"
        );
    }

    #[test]
    fn promote_expired_wakes_a_deadline_waiter() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        let deadline = Instant::now() + Duration::from_millis(5);
        assert!(table.try_park(pid, WaitTarget::Receive, Some(deadline)));
        assert!(matches!(table.after_switch(pid), SwitchOutcome::Parked));

        let wake = table.promote_expired(pid, deadline);
        assert_eq!(wake.map(|wake| wake.pid), Some(pid));
        assert_eq!(table.counters().stale_deadlines_skipped, 0);
        // The wake cleared the recorded deadline: a duplicate fire is stale.
        assert!(table.try_claim(pid));
        assert!(table.try_park(pid, WaitTarget::Receive, None));
        assert!(table.promote_expired(pid, deadline).is_none());
        assert_eq!(table.counters().stale_deadlines_skipped, 1);
    }

    #[test]
    fn promote_expired_skips_a_message_woken_waiter() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        let deadline = Instant::now() + Duration::from_millis(5);
        assert!(table.try_park(pid, WaitTarget::Receive, Some(deadline)));

        // A message wins the race: the waiter is promoted, leaving the
        // fired entry stale.
        assert!(table.deliver(pid, fake_envelope()).wake.is_some());

        assert!(table.promote_expired(pid, deadline).is_none());
        assert_eq!(table.counters().stale_deadlines_skipped, 1);
    }

    #[test]
    fn promote_expired_skips_a_reparked_waiter() {
        let table = TestTable::new();
        let pid = spawn_running(&table);

        // Park, stale-reply wake, re-park with a later deadline, as the
        // call_receive loop does. The first deadline's fire must not wake
        // the re-parked waiter early.
        let first = Instant::now() + Duration::from_millis(10);
        let second = first + Duration::from_millis(10);
        assert!(table.try_park(pid, WaitTarget::Reply, Some(first)));
        assert!(matches!(table.after_switch(pid), SwitchOutcome::Parked));
        assert!(table.deliver(pid, fake_reply(1)).wake.is_some());
        assert!(table.try_claim(pid));
        assert!(table.try_park(pid, WaitTarget::Reply, Some(second)));

        assert!(table.promote_expired(pid, first).is_none());
        assert_eq!(table.counters().stale_deadlines_skipped, 1);

        assert!(table.promote_expired(pid, second).is_some());
    }

    #[test]
    fn promote_expired_skips_a_cleared_deadline() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        let deadline = Instant::now() + Duration::from_millis(10);
        assert!(table.try_park(pid, WaitTarget::Receive, Some(deadline)));
        assert!(table.deliver(pid, fake_envelope()).wake.is_some());

        // The resume path cleared the recorded deadline, so a late fire
        // for it is stale.
        table.clear_deadline(pid);
        assert!(table.promote_expired(pid, deadline).is_none());
        assert_eq!(table.counters().stale_deadlines_skipped, 1);
    }

    #[test]
    fn promote_expired_skips_a_dead_waiter() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        let deadline = Instant::now() + Duration::from_millis(10);
        assert!(table.try_park(pid, WaitTarget::Receive, Some(deadline)));
        assert!(matches!(table.after_switch(pid), SwitchOutcome::Parked));
        assert!(table.kill(pid).is_some(), "off-cpu target frees inline");

        assert!(table.promote_expired(pid, deadline).is_none());
        assert_eq!(table.counters().stale_deadlines_skipped, 1);
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn lifecycle_wakes_waiting_io_but_business_does_not() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        assert!(matches!(table.try_park_io(pid), IoPark::Parked));
        assert!(!table.has_system_mail(pid));

        // Business traffic must not wake an I/O waiter.
        assert!(table.deliver(pid, fake_envelope()).wake.is_none());

        // A lifecycle/system message wakes it and is observable as pending.
        assert!(table.deliver(pid, fake_lifecycle()).wake.is_some());
        assert!(table.has_system_mail(pid));
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn park_io_is_interrupted_by_queued_system_mail() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        assert!(table.deliver(pid, fake_lifecycle()).wake.is_none());

        // The signal is already queued: the wait must not start.
        assert!(matches!(table.try_park_io(pid), IoPark::SystemMail));
        assert!(matches!(table.after_switch(pid), SwitchOutcome::Parked));
    }

    #[test]
    fn promote_io_waiter_guards_the_source_state() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        assert!(matches!(table.try_park_io(pid), IoPark::Parked));

        assert_eq!(table.promote_io_waiter(pid).map(|wake| wake.pid), Some(pid),);
        // Already promoted: a duplicate reactor wake is a no-op.
        assert!(table.promote_io_waiter(pid).is_none());
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn yield_running_requeues_at_switch_out() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        table.set_priority(pid, Priority::High);

        table.yield_running(pid);
        match table.after_switch(pid) {
            SwitchOutcome::Requeue(wake) => {
                assert_eq!(wake.pid, pid);
                assert_eq!(wake.priority, Priority::High, "requeued at priority");
            }
            _ => panic!("a yielded process re-queues"),
        }
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn wake_during_the_on_cpu_window_requeues_at_switch_out() {
        let table = TestTable::new();
        let pid = spawn_running(&table);
        // The process parks, then a delivery wakes it before its owner
        // switches out. The delivery's wake fact enqueues a candidate that
        // cannot claim yet (`on_cpu` is still set), so the release's
        // `Requeue` is what actually gets the process resumed.
        assert!(table.try_park(pid, WaitTarget::Receive, None));
        assert!(table.deliver(pid, fake_envelope()).wake.is_some());
        assert!(!table.try_claim(pid), "still on_cpu until the release");
        assert!(matches!(table.after_switch(pid), SwitchOutcome::Requeue(_)));
        assert!(table.try_claim(pid), "claimable once released");
    }

    #[test]
    fn alive_and_active_counts_track_the_lifecycle() {
        let table = TestTable::new();
        let pid = fake_spawn(&table);
        assert!(!table.any_active(), "Created is not active");
        assert!(!table.should_shutdown(), "one live process");

        assert!(table.try_claim(pid));
        assert!(table.any_active());

        assert!(table.try_park(pid, WaitTarget::Receive, None));
        assert!(!table.any_active());

        assert!(table.deliver(pid, fake_envelope()).wake.is_some());
        table.kill(pid);
        assert!(table.should_shutdown(), "main dead");
    }

    #[test]
    fn enter_draining_switches_once() {
        let table = TestTable::new();
        assert!(!table.is_draining());
        assert!(table.enter_draining(), "first SIGTERM performs the switch");
        assert!(table.is_draining());
        // A second SIGTERM reports false so the adapter arms the grace
        // deadline exactly once.
        assert!(!table.enter_draining());
        assert!(table.is_draining());
    }

    #[test]
    fn kill_all_kills_every_live_process() {
        let table = TestTable::new();
        let parked = spawn_running(&table);
        assert!(table.try_park(parked, WaitTarget::Receive, None));
        assert!(matches!(table.after_switch(parked), SwitchOutcome::Parked));
        // The second process stays on_cpu, as a straggler mid-run would.
        let running = spawn_running(&table);

        let reclaimed = table.kill_all();
        assert_eq!(
            reclaimed.len(),
            1,
            "only the off-cpu process reclaims inline"
        );
        assert!(table.should_shutdown(), "every process is now dead");
        assert!(!table.is_alive(running));
        assert!(!table.is_alive(parked));
    }

    /// Drives a claimed process to `Dead` with `reason` recorded first, as
    /// a worker's death site would. The claim defers reclaim, so the slot
    /// (and its recorded reason) stays readable.
    fn run_to_death(table: &TestTable, pid: Pid, reason: ExitReason) {
        assert!(table.try_claim(pid));
        table.set_exit_reason(pid, reason);
        assert!(table.mark_dead_if_alive(pid));
    }

    #[test]
    fn monitor_stages_notice_on_target_death() {
        let table = TestTable::new();
        let watcher = fake_spawn(&table);
        let target = fake_spawn(&table);
        let token = table.monitor(watcher, target);
        assert!(token > 0);
        assert!(table.take_exit_notices().is_empty(), "nothing staged yet");

        run_to_death(&table, watcher, ExitReason::Normal);
        // The watcher died first: its own monitor entry is evicted, so
        // the target's later death stages nothing.
        run_to_death(&table, target, ExitReason::Normal);
        assert!(
            table.take_exit_notices().is_empty(),
            "dead watcher's monitors are evicted"
        );
    }

    #[test]
    fn target_death_stages_one_notice_per_monitor() {
        let table = TestTable::new();
        let watcher_a = fake_spawn(&table);
        let watcher_b = fake_spawn(&table);
        let target = fake_spawn(&table);
        table.monitor(watcher_a, target);
        table.monitor(watcher_a, target);
        table.monitor(watcher_b, target);

        table.set_crash_info(
            target,
            CrashInfo {
                message: "boom".into(),
                backtrace: "trace".into(),
            },
        );
        run_to_death(&table, target, ExitReason::Crashed);

        let notices = table.take_exit_notices();
        assert_eq!(notices.len(), 3, "one notice per monitor");
        for notice in &notices {
            assert_eq!(notice.target, target);
            assert_eq!(notice.reason, ExitReason::Crashed);
            assert_eq!(notice.crash_info.as_ref().unwrap().message, "boom");
        }
        let watchers: Vec<Pid> = notices.iter().map(|notice| notice.watcher).collect();
        assert_eq!(watchers, vec![watcher_a, watcher_a, watcher_b]);
        assert!(table.take_exit_notices().is_empty(), "drain is exhaustive");
        // Entries were evicted: a hypothetical second death can't fire.
        assert!(table.registry.lock().unwrap().monitors.is_empty());
    }

    #[test]
    fn demonitor_suppresses_delivery() {
        let table = TestTable::new();
        let watcher = fake_spawn(&table);
        let target = fake_spawn(&table);
        let token = table.monitor(watcher, target);
        table.demonitor(token);

        run_to_death(&table, target, ExitReason::Killed);
        assert!(table.take_exit_notices().is_empty());
        // A second demonitor of the same token is a harmless no-op.
        table.demonitor(token);
    }

    #[test]
    fn monitor_on_dead_pid_stages_immediately() {
        let table = TestTable::new();
        let watcher = fake_spawn(&table);
        let target = fake_spawn(&table);
        // Off-cpu kill reclaims the slot inline.
        assert!(table.kill(target).is_some());
        let _ = table.take_exit_notices();

        // The slot is already reclaimed, so the recorded reason is
        // lost: a freed PID reports `Normal`.
        let token = table.monitor(watcher, target);
        assert!(token > 0, "a token is minted even for a dead target");
        let notices = table.take_exit_notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].watcher, watcher);
        assert_eq!(notices[0].target, target);
        assert_eq!(notices[0].reason, ExitReason::Normal);
    }

    #[test]
    fn monitor_on_dead_unreclaimed_pid_reports_recorded_reason() {
        let table = TestTable::new();
        let watcher = fake_spawn(&table);
        let target = fake_spawn(&table);
        // Claim + kill: on_cpu defers reclaim, so the slot (and its
        // recorded reason) is still readable.
        assert!(table.try_claim(target));
        assert!(table.kill(target).is_none(), "on_cpu kill defers");
        let _ = table.take_exit_notices();

        table.monitor(watcher, target);
        let notices = table.take_exit_notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].reason, ExitReason::Killed);
    }

    #[test]
    fn exit_reason_defaults_normal_until_set() {
        let table = TestTable::new();
        let watcher = fake_spawn(&table);
        let target = fake_spawn(&table);
        table.monitor(watcher, target);

        // No reason recorded before death: the notice reports `Normal`.
        assert!(table.try_claim(target));
        assert!(table.mark_dead_if_alive(target));
        let notices = table.take_exit_notices();
        assert_eq!(notices[0].reason, ExitReason::Normal);
    }

    #[test]
    fn parent_recorded_at_spawn() {
        let table = TestTable::new();
        let root = fake_spawn(&table);
        let child = fake_spawn_child(&table, root);
        assert_eq!(table.parent(root), None, "entry process has no parent");
        assert_eq!(table.parent(child), Some(root));
        assert_eq!(table.parent(child + 1), None, "stale pid has no parent");
    }

    /// Drains staged kills as a driver would: kill each, which stages
    /// the next generation, until the cascade settles.
    fn run_kill_cascade(table: &TestTable) -> Vec<Pid> {
        let mut killed = Vec::new();
        loop {
            let staged = table.take_pending_kills();
            if staged.is_empty() {
                return killed;
            }
            for pid in staged {
                table.kill(pid);
                killed.push(pid);
            }
        }
    }

    #[test]
    fn death_cascades_to_descendants_transitively() {
        let table = TestTable::new();
        let root = fake_spawn(&table);
        let child = fake_spawn_child(&table, root);
        let grandchild = fake_spawn_child(&table, child);
        let bystander = fake_spawn(&table);

        run_to_death(&table, root, ExitReason::Normal);
        let killed = run_kill_cascade(&table);

        assert_eq!(killed, vec![child, grandchild], "subtree died in order");
        assert!(!table.is_alive(child));
        assert!(!table.is_alive(grandchild));
        assert!(table.is_alive(bystander), "unrelated process survives");
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn cascade_kills_fire_monitors_with_killed_reason() {
        let table = TestTable::new();
        let watcher = fake_spawn(&table);
        let root = fake_spawn(&table);
        let child = fake_spawn_child(&table, root);
        table.monitor(watcher, child);

        run_to_death(&table, root, ExitReason::Killed);
        run_kill_cascade(&table);

        let notices = table.take_exit_notices();
        assert_eq!(notices.len(), 1, "the child's monitor fired");
        assert_eq!(notices[0].target, child);
        assert_eq!(notices[0].reason, ExitReason::Killed);
    }

    #[test]
    fn dead_children_are_not_staged() {
        let table = TestTable::new();
        let root = fake_spawn(&table);
        let child = fake_spawn_child(&table, root);
        assert!(table.kill(child).is_some());
        assert_eq!(
            table.take_pending_kills(),
            Vec::<Pid>::new(),
            "a childless kill stages nothing"
        );

        run_to_death(&table, root, ExitReason::Normal);
        assert!(
            table.take_pending_kills().is_empty(),
            "already-dead child is not staged again"
        );
    }

    #[test]
    fn kill_stages_killed_notice() {
        let table = TestTable::new();
        let watcher = fake_spawn(&table);
        let target = fake_spawn(&table);
        table.monitor(watcher, target);

        assert!(table.kill(target).is_some());
        let notices = table.take_exit_notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].reason, ExitReason::Killed);
        assert!(notices[0].crash_info.is_none());
    }
}
