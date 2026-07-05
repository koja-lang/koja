//! Generational slotmap of live processes plus the scheduler's ready queue
//! and timing wheel: the agnostic scheduling *policy*.
//!
//! A PID packs a slot index and a generation: `pid = (generation << 32) |
//! index`. Slots are reused after a process dies (so memory is bounded), and
//! the generation is bumped on free so a stale `Ref` to a recycled slot fails
//! the lookup ([`ProcessTable::get`] returns `None` -> `ProcessDown`) instead
//! of aliasing the new occupant.
//!
//! All state changes funnel through [`ProcessTable::transition`], which keeps
//! the ready queue and the live / active counts in sync. The ready queue makes
//! process pickup O(1), and the [`TimerWheel`] makes deadline promotion and
//! timer firing amortized O(1). Both delayed deliveries and receive/call
//! deadlines share one wheel keyed by fire instant.
//!
//! The table is generic over two platform types and contains **no locking,
//! no threads, and no I/O**: the per-process execution state `X` (opaque,
//! the executor's private state, e.g. a native stack + saved `sp`) and the
//! mailbox message representation `M` (byte [`Envelope`](crate::wire::Envelope)
//! natively, a typed value cooperatively). An adapter wraps the table in
//! whatever synchronization its driver needs: a `Mutex` for the
//! multi-threaded native driver, a bare `&mut` borrow for a single-threaded
//! cooperative one.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::mailbox::{Mailbox, WaitTarget};
use crate::protocol::{Message, Pid, Tag};
use crate::scheduler_trace::{SchedulerTrace, TraceEntry, TraceEvent};
use crate::timer_wheel::{Due, TimerEntry, TimerWheel};

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

/// A single lightweight process: the agnostic lifecycle record plus the
/// opaque executor `execution` state and the mailbox.
///
/// The control-block fields (`state`, `waiting`, `deadline`, `on_cpu`) are
/// the scheduling policy's. `execution` is the executor's private state,
/// which the table stores and hands back but never inspects.
pub struct ProcessControlBlock<X, M> {
    /// The executor's per-process execution state (native: entry fn, config,
    /// saved `sp`, stack mapping). Opaque to the table.
    pub execution: X,
    /// Optional wake deadline. Set when parking with a timeout, cleared on
    /// resume. The driver promotes `Blocked -> Runnable` when it passes.
    pub deadline: Option<Instant>,
    /// Routed message queues plus the one-shot reply slot.
    pub mailbox: Mailbox<M>,
    /// Correlation token of the `Ref.call` this process is currently waiting
    /// on a reply for, set when the token is minted and cleared when the call
    /// completes. `ReplyTo.send` consults it to report whether the caller is
    /// still listening (`Some(token)`) or has moved on (`None` / mismatch).
    awaiting_reply: Option<i64>,
    /// Crash capture recorded at the unwind site. `None` unless the process
    /// died with `ExitReason::Crashed`.
    crash_info: Option<CrashInfo>,
    /// Claim flag: `true` from the moment a driver activates this process
    /// until it has persisted the post-yield execution state. Gates pickup so no
    /// other worker resumes a stale frame in the publish-before-save window.
    on_cpu: bool,
    /// Scheduling priority, selecting which ready queue an enqueue lands in.
    priority: Priority,
    /// This quantum's reduction budget, granted (= `priority.budget()`) on
    /// each `-> Running` claim. An adapter seeds its thread-local decrement
    /// counter from this on resume via [`ProcessTable::reductions_left`].
    /// The per-`YieldCheck` spend happens there, not on this field.
    reductions_left: u32,
    /// Why the process terminated, recorded at its death site and read by
    /// [`ProcessTable::notify_exit`] on the `-> Dead` edge. `Normal` until a
    /// reason is set.
    exit_reason: ExitReason,
    /// Current lifecycle state, driven by [`ProcessTable::transition`].
    pub state: ProcessState,
    /// What a `Blocked` process is waiting on, so delivery only wakes it for
    /// traffic that can satisfy the wait. Meaningful only while `Blocked`.
    waiting: WaitTarget,
}

impl<X, M> ProcessControlBlock<X, M> {
    /// A freshly spawned process in the `Created` state with an empty mailbox.
    fn new(execution: X) -> Self {
        Self {
            execution,
            deadline: None,
            mailbox: Mailbox::default(),
            awaiting_reply: None,
            crash_info: None,
            on_cpu: false,
            priority: Priority::default(),
            reductions_left: Priority::default().budget(),
            exit_reason: ExitReason::default(),
            state: ProcessState::Created,
            waiting: WaitTarget::Receive,
        }
    }
}

/// Resources moved out of a dead process, freed when this value is dropped,
/// which the reclaim sites do only after any driver lock is released. Each
/// field is an RAII owner, so dropping a `Reclaim` runs the execution state's
/// drop glue (native: unmaps the stack, releases the spawn config) and drains the
/// mailbox (running each message's drop glue).
///
/// The fields are never read by name (they exist purely so their own `Drop`
/// runs at this controlled point), hence `allow(dead_code)`.
#[allow(dead_code)]
pub struct Reclaim<X, M> {
    execution: X,
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

/// Number of scheduling priority levels.
const LEVELS: usize = 3;

/// Selections a non-empty ready queue may be passed over before it ages
/// enough to preempt in `next_ready_level`, bounding every level's wait.
const STARVATION_THRESHOLD: u32 = 8;

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

/// Why a process terminated, recorded on its PCB and read by the
/// [`ProcessTable::notify_exit`] seam when it goes `Dead`. The wire code is
/// an ABI contract (0 = `Normal`, 1 = `Shutdown`, 2 = `Killed`, 3 =
/// `Crashed`) decoded by `from_index`: `Normal`/`Shutdown` mirror the stop
/// reason a process returns, `Killed` marks a forced kill, and `Crashed` is
/// reserved for fault capture (nothing produces it yet). Default is `Normal`.
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
/// death. Carried alongside the `Copy` `ExitReason` on the PCB rather than
/// inside the discriminant, so the heavy strings travel only on an actual crash.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CrashInfo {
    pub message: String,
    pub backtrace: String,
}

/// One slot in the table. `process` is `None` when the slot is free;
/// `generation` is bumped on free so recycled slots reject stale PIDs.
struct Slot<X, M> {
    generation: u32,
    process: Option<ProcessControlBlock<X, M>>,
}

/// Whether a state counts toward the `active` tally (work the reactor or
/// another worker will make progress on without a timer wakeup).
fn is_active(state: ProcessState) -> bool {
    matches!(state, ProcessState::Running | ProcessState::WaitingIO)
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
/// because every call site gates its precondition first.
fn is_legal_transition(from: ProcessState, to: ProcessState) -> bool {
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
/// `koja_rt_parks_refused`.
pub struct ScheduleCounters {
    /// Kills that found the target `on_cpu` and deferred reclaim.
    pub kills_deferred: u64,
    /// Parks refused because the target was already `Dead` (or stale).
    pub parks_refused: u64,
    /// Ready-queue entries skipped by `claim_next` (killed, already resumed,
    /// or still `on_cpu`).
    pub stale_claims_skipped: u64,
    /// Deadline-heap entries rejected by `promote_due_deadlines`.
    pub stale_deadlines_skipped: u64,
    /// Envelopes bounced off a dead or stale target.
    pub undeliverable_envelopes: u64,
    /// Illegal lifecycle edges applied by `transition`. Always zero in a
    /// correct runtime. Counted (not just debug-asserted) so release builds
    /// can detect ordering bugs too.
    pub violations: u64,
}

impl ScheduleCounters {
    const fn new() -> Self {
        Self {
            kills_deferred: 0,
            parks_refused: 0,
            stale_claims_skipped: 0,
            stale_deadlines_skipped: 0,
            undeliverable_envelopes: 0,
            violations: 0,
        }
    }
}

/// The runtime's lifecycle mode. `Draining` is entered on `SIGTERM`: new
/// spawns are refused and a grace deadline is armed, after which any
/// straggler is force-killed. Resets to `Running` by construction (a fresh
/// table per program / per run).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Running,
    Draining,
}

/// The scheduler's process store: a generational slotmap with a ready queue
/// and a timing wheel. Contains no synchronization. The adapter's driver
/// supplies it.
pub struct ProcessTable<X, M> {
    /// Count of `Running` + `WaitingIO` processes (park-timeout heuristic).
    active: usize,
    /// Count of processes not yet `Dead` (shutdown when this hits zero).
    alive: usize,
    /// Invariant counters, exposed to fixtures via the adapter.
    counters: ScheduleCounters,
    /// Due deliveries staged by the most recent [`Self::advance_timers`],
    /// drained by [`Self::take_due_timers`]. Promotions are applied inline
    /// during the advance. Only the deliveries the driver routes wait here.
    due_delivers: Vec<TimerEntry<M>>,
    /// When set, [`Self::enqueue_ready`] stages PIDs in [`Self::pending_ready`]
    /// for an adapter that owns the ready queue (native work-stealing) instead
    /// of pushing into [`Self::ready`]. Off by default, so the cooperative and
    /// WASM drivers keep using the in-core queues unchanged.
    external_ready: bool,
    /// Indices of free slots available for reuse.
    free: Vec<u32>,
    /// When draining, the instant after which stragglers are force-killed
    /// (armed once by [`Self::enter_draining`]). `None` while `Running`.
    grace_deadline: Option<Instant>,
    /// The instant the wheel was last advanced to, so the paired
    /// `promote_due_deadlines` / `take_due_timers` calls for one `now` drain
    /// the wheel exactly once.
    last_advance: Option<Instant>,
    /// First spawned process (the program entry). Drives signal delivery and
    /// the shutdown decision. `0` until the first spawn.
    main_pid: Pid,
    /// Runtime lifecycle mode, `Draining` once a `SIGTERM` has been seen.
    mode: Mode,
    /// Newly-runnable PIDs staged for an external queue owner, drained by
    /// [`Self::drain_pending_ready`]. Only populated while [`Self::external_ready`].
    pending_ready: Vec<Pid>,
    /// Packed PIDs ready to run, one FIFO queue per priority level
    /// (index = `priority as usize`), highest level served first.
    ready: [VecDeque<Pid>; LEVELS],
    /// Per-level count of consecutive selections each ready queue has
    /// been passed over while non-empty. A level that reaches
    /// `STARVATION_THRESHOLD` preempts so no priority is starved.
    ready_ages: [u32; LEVELS],
    /// All slots, indexed by a PID's low 32 bits.
    slots: Vec<Slot<X, M>>,
    /// Lifecycle event ring, dumped at shutdown under `KOJA_SCHED_TRACE`.
    trace: SchedulerTrace,
    /// Delayed deliveries and receive/call deadlines, soonest first.
    wheel: TimerWheel<M>,
}

impl<X, M: Message> ProcessTable<X, M> {
    pub const fn new() -> Self {
        Self {
            active: 0,
            alive: 0,
            counters: ScheduleCounters::new(),
            due_delivers: Vec::new(),
            external_ready: false,
            free: Vec::new(),
            grace_deadline: None,
            last_advance: None,
            main_pid: 0,
            mode: Mode::Running,
            pending_ready: Vec::new(),
            ready: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
            ready_ages: [0; LEVELS],
            slots: Vec::new(),
            trace: SchedulerTrace::new(),
            wheel: TimerWheel::new(),
        }
    }

    /// The invariant counters.
    pub fn counters(&self) -> &ScheduleCounters {
        &self.counters
    }

    /// Recorded lifecycle events, oldest first.
    pub fn trace_entries(&self) -> impl Iterator<Item = &TraceEntry> {
        self.trace.iter()
    }

    /// The program entry process, or `0` before the first spawn.
    pub fn main_pid(&self) -> Pid {
        self.main_pid
    }

    /// Looks up a process by packed PID, validating the generation. Returns
    /// `None` for an out-of-range, freed, or recycled (stale) PID.
    pub fn get(&self, pid: Pid) -> Option<&ProcessControlBlock<X, M>> {
        let (index, generation) = decode(pid);
        let slot = self.slots.get(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        slot.process.as_ref()
    }

    /// Mutable [`get`](Self::get).
    pub fn get_mut(&mut self, pid: Pid) -> Option<&mut ProcessControlBlock<X, M>> {
        let (index, generation) = decode(pid);
        let slot = self.slots.get_mut(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        slot.process.as_mut()
    }

    /// Whether `pid` resolves to a live (non-`Dead`) process. Stale and freed
    /// PIDs are not alive.
    pub fn is_alive(&self, pid: Pid) -> bool {
        self.get(pid)
            .is_some_and(|process| process.state != ProcessState::Dead)
    }

    /// Whether `pid` has a pending system/lifecycle message: the signal a
    /// blocking I/O wait checks to decide whether it was interrupted.
    pub fn has_system_mail(&self, pid: Pid) -> bool {
        self.get(pid)
            .is_some_and(|process| process.mailbox.has_system())
    }

    /// Sets `pid`'s scheduling priority. Takes effect at the next enqueue,
    /// so an entry already queued at the old level is not moved.
    pub fn set_priority(&mut self, pid: Pid, priority: Priority) {
        if let Some(process) = self.get_mut(pid) {
            process.priority = priority;
        }
    }

    /// Registers a new process (with its executor `execution` state) in a free
    /// or freshly grown slot, queues it as runnable, and returns its packed PID.
    pub fn spawn(&mut self, execution: X) -> Pid {
        let (index, generation) = match self.free.pop() {
            Some(index) => (index, self.slots[index as usize].generation),
            None => {
                let index = self.slots.len() as u32;
                self.slots.push(Slot {
                    generation: 1,
                    process: None,
                });
                (index, 1)
            }
        };

        let pid = encode(index, generation);
        self.slots[index as usize].process = Some(ProcessControlBlock::new(execution));
        self.alive += 1;
        self.enqueue_ready(pid);
        if self.main_pid == 0 {
            self.main_pid = pid;
        }
        pid
    }

    /// Reclaims a dead process's slot: detaches its resources (to drop
    /// off-lock), bumps the generation, and returns the slot to the freelist.
    /// Idempotent: a second call on the same PID returns `None`.
    fn free(&mut self, pid: Pid) -> Option<Reclaim<X, M>> {
        let (index, generation) = decode(pid);
        let slot = self.slots.get_mut(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        let process = slot.process.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(index);
        self.trace.record(pid, TraceEvent::Freed);
        Some(Reclaim {
            execution: process.execution,
            mailbox: process.mailbox,
        })
    }

    /// Single chokepoint for lifecycle state changes. Counts (and, in debug
    /// builds, asserts) illegal edges, keeps the `alive` / `active` counts
    /// current, and enqueues the PID when it becomes `Runnable`. A `None`
    /// lookup (stale PID) is a no-op so racing wakeups against a freed slot
    /// are harmless.
    pub fn transition(&mut self, pid: Pid, to: ProcessState) {
        let from = match self.get_mut(pid) {
            Some(process) => {
                let from = process.state;
                process.state = to;
                from
            }
            None => return,
        };
        if !is_legal_transition(from, to) {
            self.counters.violations += 1;
            debug_assert!(
                false,
                "illegal process state transition for pid {pid}: {from:?} -> {to:?}",
            );
        }
        self.trace.record(pid, TraceEvent::Transition { from, to });

        match (is_active(from), is_active(to)) {
            (true, false) => self.active -= 1,
            (false, true) => self.active += 1,
            _ => {}
        }
        if from != ProcessState::Dead && to == ProcessState::Dead {
            self.alive -= 1;
            let reason = self.get(pid).map_or(ExitReason::Normal, |p| p.exit_reason);
            self.notify_exit(pid, reason);
        }
        // A `Running -> Runnable` edge is a cooperative yield: the worker's
        // following `after_switch` re-enqueues the process once `on_cpu`
        // clears, so enqueuing here too would leave a duplicate that piles
        // up under a tight yield loop. Every other `-> Runnable` edge wakes a
        // parked process with no such follow-up and must enqueue.
        if to == ProcessState::Runnable && from != ProcessState::Running {
            self.enqueue_ready(pid);
        }
    }

    /// Enqueues `pid` onto the ready queue for its current priority, or, in
    /// [`external_ready`](Self::external_ready) mode, stages it in
    /// [`pending_ready`](Self::pending_ready) for the adapter to route.
    fn enqueue_ready(&mut self, pid: Pid) {
        if self.external_ready {
            self.pending_ready.push(pid);
            return;
        }
        let level = self.get(pid).map_or(Priority::Normal, |p| p.priority) as usize;
        self.ready[level].push_back(pid);
    }

    /// Hands ready-queue ownership to the adapter: from now on
    /// [`enqueue_ready`](Self::enqueue_ready) stages newly-runnable PIDs in
    /// [`pending_ready`](Self::pending_ready) (drained via
    /// [`drain_pending_ready`](Self::drain_pending_ready)) rather than the
    /// in-core [`ready`](Self::ready) queues. One-way, set once at native boot.
    pub fn use_external_ready(&mut self) {
        self.external_ready = true;
    }

    /// Drains every PID staged since the last call, each paired with its
    /// current [`Priority`] so the adapter can route to a priority queue
    /// without re-locking per PID. Empty unless [`external_ready`](Self::external_ready).
    pub fn drain_pending_ready(&mut self) -> Vec<(Pid, Priority)> {
        std::mem::take(&mut self.pending_ready)
            .into_iter()
            .map(|pid| {
                let priority = self.get(pid).map_or(Priority::Normal, |p| p.priority);
                (pid, priority)
            })
            .collect()
    }

    /// Parks `pid` as `Blocked`, recording which part of its mailbox it waits
    /// on and an optional wake deadline. Refuses (returning `false` without
    /// touching the state) when the process is dead or stale, because a kill
    /// can land while the process is mid-run on another worker (`* -> Dead`
    /// is a legal cross-worker edge), and parking over the tombstone would
    /// resurrect it.
    /// A refused caller should still yield. The worker sees `Dead` on
    /// switch-out and reclaims the slot, so the frame never resumes.
    pub fn try_park(&mut self, pid: Pid, target: WaitTarget, deadline: Option<Instant>) -> bool {
        if !self.is_alive(pid) {
            self.note_refused_park(pid);
            return false;
        }
        self.transition(pid, ProcessState::Blocked);
        if let Some(process) = self.get_mut(pid) {
            process.deadline = deadline;
            process.waiting = target;
        }
        if let Some(deadline) = deadline {
            self.push_deadline(pid, deadline);
        }
        true
    }

    /// Parks `pid` as `WaitingIO`, with the same kill-tombstone refusal as
    /// [`try_park`](Self::try_park). A refused caller must not register the fd
    /// with the reactor, since there is no waiter to wake.
    pub fn try_park_io(&mut self, pid: Pid) -> bool {
        if !self.is_alive(pid) {
            self.note_refused_park(pid);
            return false;
        }
        self.transition(pid, ProcessState::WaitingIO);
        true
    }

    fn note_refused_park(&mut self, pid: Pid) {
        self.counters.parks_refused += 1;
        self.trace.record(pid, TraceEvent::ParkRefused);
    }

    /// Marks `pid` `Dead` unless it already is (a racing kill may have won)
    /// or the slot is stale, since re-marking would be an illegal self-edge
    /// and would double-decrement the `alive` count. Returns whether this
    /// call performed the transition.
    pub fn mark_dead_if_alive(&mut self, pid: Pid) -> bool {
        if !self.is_alive(pid) {
            return false;
        }
        self.transition(pid, ProcessState::Dead);
        true
    }

    /// The "last resort" termination primitive: marks `pid` `Dead` and
    /// reclaims its slot, returning the detached resources for the caller to
    /// drop off-lock. `None` when the target was already dead/stale, or when
    /// it is still `on_cpu`. Reclaiming a stack a worker is running on would
    /// be a use-after-free, so the owning worker reclaims on switch-out
    /// instead (it sees `Dead` in [`after_switch`](Self::after_switch)). The
    /// mailbox rides along either way, so envelopes are freed exactly once.
    pub fn kill(&mut self, pid: Pid) -> Option<Reclaim<X, M>> {
        if !self.is_alive(pid) {
            return None;
        }
        // Record the reason before the `-> Dead` edge so `notify_exit` sees
        // `Killed`, not the `Normal` default.
        self.set_exit_reason(pid, ExitReason::Killed);
        self.mark_dead_if_alive(pid);
        if self.get(pid).is_some_and(|process| process.on_cpu) {
            self.counters.kills_deferred += 1;
            self.trace.record(pid, TraceEvent::KillDeferred);
            None
        } else {
            self.free(pid)
        }
    }

    /// Records why `pid` is terminating, to be read by [`notify_exit`] on the
    /// `-> Dead` edge. Set before marking the process dead. A no-op for a
    /// stale PID, and a process that sets no reason dies `Normal`.
    ///
    /// [`notify_exit`]: Self::notify_exit
    pub fn set_exit_reason(&mut self, pid: Pid, reason: ExitReason) {
        if let Some(process) = self.get_mut(pid) {
            process.exit_reason = reason;
        }
    }

    /// Records a crashing process's capture, paired with
    /// `set_exit_reason(pid, ExitReason::Crashed)` at the unwind site. A no-op
    /// for a stale PID.
    pub fn set_crash_info(&mut self, pid: Pid, crash_info: CrashInfo) {
        if let Some(process) = self.get_mut(pid) {
            process.crash_info = Some(crash_info);
        }
    }

    /// The crash capture recorded for `pid`, if it died `Crashed`.
    pub fn crash_info(&self, pid: Pid) -> Option<&CrashInfo> {
        self.get(pid)
            .and_then(|process| process.crash_info.as_ref())
    }

    /// Records that `pid` is now waiting on the reply for call `token`. Set
    /// when the token is minted (before the request is sent) so a reply can
    /// never race ahead of the caller registering its interest.
    pub fn set_awaiting_reply(&mut self, pid: Pid, token: i64) {
        if let Some(process) = self.get_mut(pid) {
            process.awaiting_reply = Some(token);
        }
    }

    /// Clears `pid`'s awaited-reply token once its call completes (reply
    /// received or timed out). A no-op for a stale PID.
    pub fn clear_awaiting_reply(&mut self, pid: Pid) {
        if let Some(process) = self.get_mut(pid) {
            process.awaiting_reply = None;
        }
    }

    /// Whether `pid` is alive and still waiting on the reply for `token`:
    /// the linearizable check `ReplyTo.send` makes under the lock to decide
    /// `Delivery.Delivered` versus `Delivery.Expired`.
    pub fn is_awaiting_reply(&self, pid: Pid, token: i64) -> bool {
        self.is_alive(pid)
            && self.get(pid).and_then(|process| process.awaiting_reply) == Some(token)
    }

    /// Fired from [`transition`](Self::transition) when a process goes `Dead`,
    /// carrying its recorded [`ExitReason`]. A no-op seam today. Process
    /// monitoring delivers exit signals to linked/monitoring processes here.
    fn notify_exit(&mut self, _pid: Pid, _reason: ExitReason) {}

    /// Pops the next claimable process, marking it `Running` and `on_cpu`.
    /// Serves the highest-priority non-empty queue, but ages the ready
    /// queues so any non-empty level passed over `STARVATION_THRESHOLD`
    /// times preempts, bounding every level's wait so no priority is
    /// starved. Skips stale ready-queue entries (killed, already resumed,
    /// or still `on_cpu`).
    pub fn claim_next(&mut self) -> Option<Pid> {
        loop {
            let level = self.next_ready_level()?;
            let pid = self.ready[level].pop_front()?;
            if self.try_claim(pid) {
                return Some(pid);
            }
        }
    }

    /// Marks `pid` `Running` and `on_cpu`, granting its quantum's reduction
    /// budget, when it is a fresh claim (alive, not already `on_cpu`,
    /// `Created`/`Runnable`). Returns `false` for a stale ready entry
    /// (killed, already resumed, or still `on_cpu`), counting the skip so
    /// the caller pops the next candidate. The per-PID half of
    /// [`claim_next`](Self::claim_next), used by an external queue owner
    /// that pops candidates itself.
    pub fn try_claim(&mut self, pid: Pid) -> bool {
        match self.get_mut(pid) {
            Some(process)
                if !process.on_cpu
                    && matches!(
                        process.state,
                        ProcessState::Created | ProcessState::Runnable
                    ) =>
            {
                process.on_cpu = true;
                process.reductions_left = process.priority.budget();
            }
            _ => {
                self.counters.stale_claims_skipped += 1;
                return false;
            }
        }
        self.transition(pid, ProcessState::Running);
        true
    }

    /// Chooses which priority level `claim_next` serves, aging the ready
    /// queues so no level starves. Normally the highest-priority non-empty
    /// level wins, but any non-empty level that has been passed over
    /// `STARVATION_THRESHOLD` times preempts (most-aged first, ties broken
    /// toward higher priority), bounding every level's wait. Returns `None`
    /// only when all queues are empty.
    fn next_ready_level(&mut self) -> Option<usize> {
        let highest = (0..LEVELS)
            .rev()
            .find(|&level| !self.ready[level].is_empty())?;
        let chosen = (0..LEVELS)
            .filter(|&level| {
                !self.ready[level].is_empty() && self.ready_ages[level] >= STARVATION_THRESHOLD
            })
            .max_by_key(|&level| (self.ready_ages[level], level))
            .unwrap_or(highest);
        for level in 0..LEVELS {
            if level == chosen || self.ready[level].is_empty() {
                self.ready_ages[level] = 0;
            } else {
                self.ready_ages[level] += 1;
            }
        }
        Some(chosen)
    }

    /// After a process yields back to its driver, releases the `on_cpu` claim
    /// and then either re-queues it (woken during the `on_cpu` window) or
    /// reclaims its slot (dead). The caller persists any executor execution
    /// state (native: the saved `sp`) into the PCB *before* calling this. Returns
    /// detached resources for the caller to drop after releasing the lock.
    pub fn after_switch(&mut self, pid: Pid) -> Option<Reclaim<X, M>> {
        let state = {
            let process = self.get_mut(pid)?;
            process.on_cpu = false;
            process.state
        };
        match state {
            ProcessState::Dead => self.free(pid),
            ProcessState::Created | ProcessState::Runnable => {
                self.enqueue_ready(pid);
                None
            }
            _ => None,
        }
    }

    /// The reductions `pid` has left this quantum, or 0 for a stale PID. Each
    /// adapter reads this once per resume to seed its own lock-free
    /// thread-local decrement counter (`YieldCheck` spends from that, not from
    /// the PCB), so the value here is just the freshly granted budget.
    pub fn reductions_left(&self, pid: Pid) -> u32 {
        self.get(pid).map_or(0, |process| process.reductions_left)
    }

    /// Voluntarily returns a running process to the ready queue (cooperative
    /// preemption). No-op unless `pid` is currently `Running`. Only marks
    /// the process `Runnable`. The actual re-enqueue happens in
    /// [`after_switch`](Self::after_switch) once `on_cpu` is released.
    pub fn yield_running(&mut self, pid: Pid) {
        if self
            .get(pid)
            .is_some_and(|process| process.state == ProcessState::Running)
        {
            self.transition(pid, ProcessState::Runnable);
        }
    }

    /// Routes `envelope` into a process's mailbox (see [`Mailbox::push`]),
    /// waking the process if it is parked waiting on the part of the mailbox
    /// this envelope satisfies. Returns a message the caller must drop after
    /// releasing the lock: the original when the target is gone or dead, or a
    /// stale reply displaced from the reply slot.
    pub fn deliver(&mut self, pid: Pid, envelope: M) -> Option<M> {
        if !self.is_alive(pid) {
            self.counters.undeliverable_envelopes += 1;
            self.trace.record(pid, TraceEvent::Undeliverable);
            return Some(envelope);
        }
        let target = Mailbox::target_of(&envelope);
        let is_system = matches!(envelope.tag(), Tag::Lifecycle);
        let process = self
            .get_mut(pid)
            .expect("is_alive implies the process exists");
        // Wake a `Blocked` receiver for the matching queue, or a `WaitingIO`
        // process for a system/lifecycle signal so blocking I/O can be
        // interrupted.
        let wake = match process.state {
            ProcessState::Blocked => process.waiting == target,
            ProcessState::WaitingIO => is_system,
            _ => false,
        };
        let displaced = process.mailbox.push(envelope);
        self.trace.record(pid, TraceEvent::Delivered);
        if wake {
            self.transition(pid, ProcessState::Runnable);
        }
        displaced
    }

    /// Schedules a delayed message. Cancellation is lazy: a timer aimed at a
    /// process that later dies is simply dropped (undeliverable) when it fires,
    /// reclaiming its envelope then.
    pub fn push_timer(&mut self, fire_at: Instant, target_pid: Pid, envelope: M) {
        self.wheel.insert_deliver(fire_at, target_pid, envelope);
    }

    /// Drains the wheel up to `now` exactly once: applies expired deadlines
    /// inline (promoting validated waiters, counting stale entries) and stages
    /// due deliveries in [`Self::due_delivers`] for the driver to route. The
    /// paired `promote_due_deadlines` / `take_due_timers` calls a driver makes
    /// for one `now` both route here, and the second is a no-op.
    fn advance_timers(&mut self, now: Instant) {
        if self.last_advance.is_some_and(|last| now <= last) {
            return;
        }
        self.last_advance = Some(now);
        for due in self.wheel.drain_due(now) {
            match due {
                Due::Deliver {
                    envelope,
                    target_pid,
                } => self.due_delivers.push(TimerEntry {
                    envelope,
                    target_pid,
                }),
                Due::Promote { pid, fire_at } => {
                    let expired = matches!(
                        self.get(pid),
                        Some(process)
                            if process.state == ProcessState::Blocked
                                && process.deadline == Some(fire_at)
                    );
                    if expired {
                        self.transition(pid, ProcessState::Runnable);
                    } else {
                        self.counters.stale_deadlines_skipped += 1;
                    }
                }
            }
        }
    }

    /// Removes and returns every timer whose `fire_at` is at or before
    /// `now`, soonest first. The caller delivers each staged envelope.
    /// Pairs with [`Self::promote_due_deadlines`] for the same `now`: the
    /// wheel is advanced at most once per `now`, so whichever of the two
    /// runs first does the draining and the second only reads what that
    /// advance staged.
    pub fn take_due_timers(&mut self, now: Instant) -> Vec<TimerEntry<M>> {
        self.advance_timers(now);
        std::mem::take(&mut self.due_delivers)
    }

    /// Records a receive deadline so the driver can promote the waiter back to
    /// `Runnable` when it expires.
    fn push_deadline(&mut self, pid: Pid, deadline: Instant) {
        self.wheel.insert_deadline(deadline, pid);
    }

    /// Promotes every process whose receive deadline has passed. Stale entries
    /// (the process was woken by a message, resumed, or died, or re-blocked
    /// with a different deadline) are validated against the live state and
    /// skipped.
    ///
    /// Drives the shared wheel via [`Self::advance_timers`]. A driver calls
    /// this *before* [`Self::take_due_timers`] for the same `now`: this call
    /// advances the wheel and applies expired deadlines inline, and the
    /// paired `take` then collects the deliveries staged by the same advance.
    pub fn promote_due_deadlines(&mut self, now: Instant) {
        self.advance_timers(now);
    }

    /// Whether any process is `Running` or `WaitingIO`.
    pub fn any_active(&self) -> bool {
        self.active != 0
    }

    /// Whether the runtime should tear down: no live processes remain, or the
    /// program entry process has died (or its slot is already reclaimed).
    pub fn should_shutdown(&self) -> bool {
        self.alive == 0 || (self.main_pid != 0 && !self.is_alive(self.main_pid))
    }

    /// The soonest pending timer or deadline (folding in the drain grace
    /// deadline so the idle park wakes to enforce it), for sizing the idle
    /// park.
    pub fn nearest_wakeup(&self) -> Option<Instant> {
        match (self.wheel.nearest(), self.grace_deadline) {
            (Some(timer), Some(grace)) => Some(timer.min(grace)),
            (timer, grace) => timer.or(grace),
        }
    }

    /// Enters `Draining` and arms the grace deadline at `now + grace`.
    /// Idempotent: a second `SIGTERM` neither re-arms the deadline nor
    /// extends it, so the grace window is measured from the first signal.
    pub fn enter_draining(&mut self, now: Instant, grace: Duration) {
        if self.mode == Mode::Draining {
            return;
        }
        self.mode = Mode::Draining;
        self.grace_deadline = Some(now + grace);
    }

    /// Whether the runtime is draining (a `SIGTERM` has been seen). New
    /// spawns are refused while this holds.
    pub fn is_draining(&self) -> bool {
        self.mode == Mode::Draining
    }

    /// Whether the drain grace deadline has passed at `now`. Always `false`
    /// while `Running` (no deadline armed).
    pub fn grace_expired(&self, now: Instant) -> bool {
        self.grace_deadline.is_some_and(|deadline| now >= deadline)
    }

    /// Force-kills every live process (recording [`ExitReason::Killed`]),
    /// returning their detached resources for the caller to drop after
    /// releasing the lock. The drain grace backstop: once the deadline
    /// passes, stragglers are killed so [`should_shutdown`](Self::should_shutdown)
    /// fires. A process still `on_cpu` is marked `Dead` (decrementing
    /// `alive`) and reclaimed by its worker on switch-out.
    pub fn kill_all(&mut self) -> Vec<Reclaim<X, M>> {
        self.live_pids()
            .into_iter()
            .filter_map(|pid| self.kill(pid))
            .collect()
    }

    /// The PIDs of all currently-live (non-`Dead`) processes.
    fn live_pids(&self) -> Vec<Pid> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let process = slot.process.as_ref()?;
                (process.state != ProcessState::Dead).then(|| encode(index as u32, slot.generation))
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
    use crate::wire::{Envelope, TAG_BUSINESS, TAG_LIFECYCLE};
    use std::ptr;
    use std::time::Duration;

    /// A unit execution state: the table stores it opaquely, so tests need
    /// no real stack or entry function.
    type TestTable = ProcessTable<(), Envelope>;

    fn fake_spawn(table: &mut TestTable) -> Pid {
        table.spawn(())
    }

    /// Spawns a process, promotes it to `priority`, and parks it off the ready
    /// queue (so a later `transition(.., Runnable)` enqueues it at the new
    /// level). Requires the ready queues to be empty at call time so the fresh
    /// PID is the one claimed.
    fn spawn_parked(table: &mut TestTable, priority: Priority) -> Pid {
        let pid = table.spawn(());
        assert_eq!(table.claim_next(), Some(pid), "freshly spawned pid claimed");
        table.set_priority(pid, priority);
        assert!(table.try_park(pid, WaitTarget::Receive, None));
        assert!(table.after_switch(pid).is_none(), "parked, not reclaimed");
        pid
    }

    /// Cooperatively yields a just-claimed (Running) process back to its ready
    /// queue, as a driver would around `after_switch`.
    fn yield_back(table: &mut TestTable, pid: Pid) {
        table.yield_running(pid);
        assert!(table.after_switch(pid).is_none(), "yielded, re-queued");
    }

    /// A minimal business envelope (empty payload, no glue).
    fn fake_envelope() -> Envelope {
        unsafe { Envelope::from_payload(TAG_BUSINESS, ptr::null(), 0, None) }
    }

    /// A minimal lifecycle/system envelope (empty payload, no glue).
    fn fake_lifecycle() -> Envelope {
        unsafe { Envelope::from_payload(TAG_LIFECYCLE, ptr::null(), 0, None) }
    }

    #[test]
    fn encode_decode_roundtrip() {
        for (index, generation) in [(0u32, 1u32), (5, 2), (0xFFFF, 0x1234)] {
            let pid = encode(index, generation);
            assert_eq!(decode(pid), (index, generation));
        }
    }

    #[test]
    fn exit_reason_defaults_normal_until_set() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        assert_eq!(table.get(pid).unwrap().exit_reason, ExitReason::Normal);
        table.set_exit_reason(pid, ExitReason::Shutdown);
        assert_eq!(table.get(pid).unwrap().exit_reason, ExitReason::Shutdown);
    }

    #[test]
    fn kill_records_killed_reason() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        // Claim it so the kill is deferred (on_cpu) and the slot survives for
        // inspection rather than being reclaimed inline.
        assert_eq!(table.claim_next(), Some(pid));
        assert!(table.kill(pid).is_none(), "on_cpu kill defers reclaim");
        let process = table.get(pid).expect("deferred kill keeps the slot");
        assert_eq!(process.state, ProcessState::Dead);
        assert_eq!(process.exit_reason, ExitReason::Killed);
    }

    #[test]
    fn enter_draining_arms_grace_once() {
        let mut table = TestTable::new();
        assert!(!table.is_draining());
        assert!(!table.grace_expired(Instant::now()));

        let start = Instant::now();
        table.enter_draining(start, Duration::from_secs(5));
        assert!(table.is_draining());
        assert!(!table.grace_expired(start));
        assert!(table.grace_expired(start + Duration::from_secs(5)));

        // A second SIGTERM neither re-arms nor extends the window.
        table.enter_draining(start + Duration::from_secs(1), Duration::from_secs(100));
        assert!(table.grace_expired(start + Duration::from_secs(5)));
    }

    #[test]
    fn nearest_wakeup_folds_in_grace_deadline() {
        let mut table = TestTable::new();
        assert_eq!(table.nearest_wakeup(), None);
        let start = Instant::now();
        table.enter_draining(start, Duration::from_secs(5));
        assert_eq!(table.nearest_wakeup(), Some(start + Duration::from_secs(5)));
    }

    #[test]
    fn kill_all_kills_every_live_process() {
        let mut table = TestTable::new();
        // Park one process off the ready queue (so `spawn_parked`'s claim
        // sees an empty queue), then spawn + claim main so it is on_cpu.
        let parked = spawn_parked(&mut table, Priority::Normal);
        let main = fake_spawn(&mut table);
        // A claimed (on_cpu) process is marked dead but reclaimed by its
        // worker on switch-out, so it isn't in the returned batch.
        assert_eq!(table.claim_next(), Some(main));

        let reclaimed = table.kill_all();
        assert_eq!(
            reclaimed.len(),
            1,
            "only the off-cpu process reclaims inline"
        );
        assert!(table.should_shutdown(), "every process is now dead");
        assert_eq!(table.get(main).unwrap().exit_reason, ExitReason::Killed);
        assert!(table.get(parked).is_none(), "parked process was freed");
    }

    #[test]
    fn external_ready_stages_instead_of_enqueuing() {
        let mut table = TestTable::new();
        table.use_external_ready();
        let pid = fake_spawn(&mut table);
        // The spawn's enqueue went to the pending buffer, not the in-core
        // queues, so the cooperative claim path sees nothing.
        assert_eq!(table.claim_next(), None, "ready queues stay empty");
        assert_eq!(table.drain_pending_ready(), vec![(pid, Priority::Normal)]);
        // Draining is exhaustive: a second drain is empty.
        assert!(table.drain_pending_ready().is_empty());
    }

    #[test]
    fn drain_pending_ready_reports_current_priority() {
        let mut table = TestTable::new();
        table.use_external_ready();
        let pid = fake_spawn(&mut table);
        table.set_priority(pid, Priority::High);
        let _ = table.drain_pending_ready();
        // A later wake reports the updated priority.
        assert!(table.try_claim(pid));
        table.yield_running(pid);
        assert!(table.after_switch(pid).is_none());
        assert_eq!(table.drain_pending_ready(), vec![(pid, Priority::High)]);
    }

    #[test]
    fn try_claim_marks_fresh_and_rejects_stale() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        assert!(table.try_claim(pid), "fresh runnable pid claims");
        let process = table.get(pid).expect("claimed pid exists");
        assert_eq!(process.state, ProcessState::Running);
        assert_eq!(process.reductions_left, Priority::Normal.budget());
        // Already on_cpu: a second claim is refused.
        assert!(!table.try_claim(pid), "on_cpu pid is not re-claimed");
        // Dead pid: refused.
        table.kill(pid);
        assert!(!table.try_claim(pid), "dead pid is not claimed");
    }

    #[test]
    fn first_pid_is_index_zero_generation_one() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        assert_eq!(decode(pid), (0, 1));
        assert!(table.get(pid).is_some());
        assert_eq!(table.main_pid(), pid);
    }

    #[test]
    fn free_then_spawn_reuses_slot_with_bumped_generation() {
        // Drive to Dead through a legal path: Created -> Running -> Dead.
        let mut table = TestTable::new();
        let first = fake_spawn(&mut table);
        table.transition(first, ProcessState::Running);
        table.transition(first, ProcessState::Dead);
        let reclaim = table.free(first);
        assert!(reclaim.is_some());

        let second = fake_spawn(&mut table);
        assert_eq!(decode(second).0, decode(first).0, "slot index reused");
        assert_eq!(decode(second).1, decode(first).1 + 1, "generation bumped");
        assert!(table.get(first).is_none(), "stale PID rejected");
        assert!(table.get(second).is_some(), "new PID resolves");
    }

    #[test]
    fn stale_generation_is_rejected() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        let (index, generation) = decode(pid);
        let stale = encode(index, generation + 7);
        assert!(table.get(stale).is_none());
        assert!(table.get(0).is_none(), "pid 0 never valid");
    }

    #[test]
    fn ready_queue_is_fifo() {
        let mut table = TestTable::new();
        let a = fake_spawn(&mut table);
        let b = fake_spawn(&mut table);
        let c = fake_spawn(&mut table);
        let order: Vec<Pid> = std::iter::from_fn(|| table.claim_next()).take(3).collect();
        assert_eq!(order, vec![a, b, c]);
    }

    #[test]
    fn timer_wheel_pops_in_fire_order() {
        let mut table = TestTable::new();
        let base = Instant::now();
        table.push_timer(base + Duration::from_millis(30), 1, fake_envelope());
        table.push_timer(base + Duration::from_millis(10), 2, fake_envelope());
        table.push_timer(base + Duration::from_millis(20), 3, fake_envelope());

        let due = table.take_due_timers(base + Duration::from_millis(25));
        let pids: Vec<Pid> = due.iter().map(|entry| entry.target_pid).collect();
        assert_eq!(pids, vec![2, 3], "soonest-first, only due timers");
        assert_eq!(
            table.nearest_wakeup(),
            Some(base + Duration::from_millis(30)),
            "remaining timer still pending"
        );
    }

    #[test]
    fn park_refuses_killed_process() {
        // A cross-worker kill can land while the victim is mid-run. Its next
        // park must not resurrect it over the `Dead` tombstone.
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        table.transition(pid, ProcessState::Running);
        assert!(table.mark_dead_if_alive(pid), "first kill wins");

        assert!(!table.try_park(pid, WaitTarget::Receive, None));
        assert!(!table.try_park_io(pid));
        assert_eq!(table.get(pid).unwrap().state, ProcessState::Dead);
        assert_eq!(table.counters().parks_refused, 2);
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn kill_defers_reclaim_while_on_cpu() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        let claimed = table.claim_next().unwrap();
        assert_eq!(claimed, pid);

        // The worker is mid-run on this stack: kill must not reclaim it.
        assert!(table.kill(pid).is_none(), "reclaim deferred to the worker");
        assert_eq!(table.counters().kills_deferred, 1);

        // The owning worker switches out, sees `Dead`, and reclaims.
        assert!(table.after_switch(pid).is_some());
        assert!(table.kill(pid).is_none(), "second kill is a no-op");
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn kill_reclaims_parked_process_directly() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        table.claim_next().unwrap();
        assert!(table.try_park(pid, WaitTarget::Receive, None));
        assert!(table.after_switch(pid).is_none(), "parked, not dead");

        assert!(table.kill(pid).is_some(), "off-cpu target frees here");
        assert_eq!(table.counters().kills_deferred, 0);
        assert!(table.get(pid).is_none(), "slot reclaimed");
    }

    #[test]
    fn undeliverable_envelope_bounces_and_counts() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        table.transition(pid, ProcessState::Running);
        table.mark_dead_if_alive(pid);

        let bounced = table.deliver(pid, fake_envelope());
        assert!(bounced.is_some(), "caller must reclaim the envelope");
        assert_eq!(table.counters().undeliverable_envelopes, 1);
    }

    #[test]
    fn stale_ready_entry_skip_is_counted() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        table.kill(pid);

        assert!(table.claim_next().is_none(), "killed entry never claimed");
        assert_eq!(table.counters().stale_claims_skipped, 1);
    }

    #[test]
    fn stale_deadline_skip_is_counted() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        table.transition(pid, ProcessState::Running);
        let deadline = Instant::now() + Duration::from_millis(5);
        assert!(table.try_park(pid, WaitTarget::Receive, Some(deadline)));

        // A message wins the race: the waiter is promoted, leaving the
        // deadline entry stale.
        assert!(table.deliver(pid, fake_envelope()).is_none());
        assert_eq!(table.get(pid).unwrap().state, ProcessState::Runnable);

        table.promote_due_deadlines(deadline + Duration::from_millis(1));
        assert_eq!(table.counters().stale_deadlines_skipped, 1);
        assert_eq!(table.get(pid).unwrap().state, ProcessState::Runnable);
    }

    #[test]
    fn lifecycle_wakes_waiting_io_but_business_does_not() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        table.transition(pid, ProcessState::Running);
        assert!(table.try_park_io(pid));
        assert_eq!(table.get(pid).unwrap().state, ProcessState::WaitingIO);
        assert!(!table.has_system_mail(pid));

        // Business traffic must not wake an I/O waiter.
        assert!(table.deliver(pid, fake_envelope()).is_none());
        assert_eq!(table.get(pid).unwrap().state, ProcessState::WaitingIO);

        // A lifecycle/system message wakes it and is observable as pending.
        assert!(table.deliver(pid, fake_lifecycle()).is_none());
        assert_eq!(table.get(pid).unwrap().state, ProcessState::Runnable);
        assert!(table.has_system_mail(pid));
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "illegal process state transition")]
    fn illegal_transition_asserts_in_debug() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        // `Created -> Blocked` is not a legal edge.
        table.transition(pid, ProcessState::Blocked);
    }

    #[test]
    fn mark_dead_if_alive_is_idempotent() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        table.transition(pid, ProcessState::Running);

        assert!(table.mark_dead_if_alive(pid));
        assert!(!table.mark_dead_if_alive(pid), "second kill is a no-op");
        assert!(!table.mark_dead_if_alive(0), "stale PID is a no-op");
        assert!(table.should_shutdown(), "alive count decremented once");
    }

    #[test]
    fn try_park_records_wait_target_and_deadline() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        table.transition(pid, ProcessState::Running);

        let deadline = Instant::now() + Duration::from_millis(10);
        assert!(table.try_park(pid, WaitTarget::Reply, Some(deadline)));
        let process = table.get(pid).unwrap();
        assert_eq!(process.state, ProcessState::Blocked);
        assert_eq!(process.waiting, WaitTarget::Reply);
        assert_eq!(process.deadline, Some(deadline));
        assert_eq!(table.nearest_wakeup(), Some(deadline));
    }

    #[test]
    fn alive_and_active_counts_track_transitions() {
        let mut table = TestTable::new();
        let pid = fake_spawn(&mut table);
        assert!(!table.any_active(), "Created is not active");
        assert!(!table.should_shutdown(), "one live process");

        table.transition(pid, ProcessState::Running);
        assert!(table.any_active());

        table.transition(pid, ProcessState::Blocked);
        assert!(!table.any_active());

        table.transition(pid, ProcessState::Dead);
        assert!(table.should_shutdown(), "main dead");
    }

    #[test]
    fn higher_priority_served_first_fifo_within_level() {
        let mut table = TestTable::new();
        let a = spawn_parked(&mut table, Priority::High);
        let b = spawn_parked(&mut table, Priority::High);
        let c = spawn_parked(&mut table, Priority::Normal);

        // Wake all three; High queue gets a then b (FIFO), Normal gets c.
        table.transition(a, ProcessState::Runnable);
        table.transition(b, ProcessState::Runnable);
        table.transition(c, ProcessState::Runnable);

        let order: Vec<Pid> = std::iter::from_fn(|| table.claim_next()).take(3).collect();
        assert_eq!(order, vec![a, b, c], "High before Normal, FIFO within High");
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn low_priority_served_within_starvation_bound() {
        let mut table = TestTable::new();
        let low = spawn_parked(&mut table, Priority::Low);
        let high = spawn_parked(&mut table, Priority::High);
        table.transition(low, ProcessState::Runnable);
        table.transition(high, ProcessState::Runnable);

        // Keep the High queue continuously non-empty by re-queuing it each
        // claim. The Low process must still be served within the bound.
        let mut claims = 0;
        loop {
            let pid = table.claim_next().expect("a process is always ready");
            claims += 1;
            if pid == low {
                break;
            }
            assert_eq!(pid, high);
            yield_back(&mut table, high);
            assert!(
                claims <= STARVATION_THRESHOLD + 1,
                "low priority starved past the bound"
            );
        }
        assert!(claims <= STARVATION_THRESHOLD + 1);
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn all_tiers_served_when_high_and_low_stay_busy() {
        // Regression: with both High and Low continuously non-empty, the old
        // "highest or forced-lowest" scheme never selected the middle Normal
        // tier, starving default-priority work. Per-level aging must serve it.
        let mut table = TestTable::new();
        let low = spawn_parked(&mut table, Priority::Low);
        let normal = spawn_parked(&mut table, Priority::Normal);
        let high = spawn_parked(&mut table, Priority::High);
        table.transition(low, ProcessState::Runnable);
        table.transition(normal, ProcessState::Runnable);
        table.transition(high, ProcessState::Runnable);

        // Keep High and Low queues continuously non-empty. The Normal process
        // must still be claimed within the aging bound.
        let mut claims = 0;
        let mut normal_seen = false;
        let mut low_seen = false;
        loop {
            let pid = table.claim_next().expect("a process is always ready");
            claims += 1;
            if pid == normal {
                normal_seen = true;
            } else if pid == low {
                low_seen = true;
                yield_back(&mut table, low);
            } else {
                assert_eq!(pid, high);
                yield_back(&mut table, high);
            }
            if normal_seen && low_seen {
                break;
            }
            assert!(
                claims <= STARVATION_THRESHOLD + LEVELS as u32,
                "a tier was starved past the aging bound"
            );
        }
        assert!(normal_seen, "middle Normal tier must not be starved");
        assert!(low_seen, "Low tier must still be served while High is busy");
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn yield_running_requeues_at_priority() {
        let mut table = TestTable::new();
        let pid = spawn_parked(&mut table, Priority::High);
        table.transition(pid, ProcessState::Runnable);
        assert_eq!(table.claim_next(), Some(pid));
        assert_eq!(table.get(pid).unwrap().state, ProcessState::Running);

        table.yield_running(pid);
        assert_eq!(table.get(pid).unwrap().state, ProcessState::Runnable);
        assert!(table.after_switch(pid).is_none(), "yielded, not reclaimed");

        // Re-queued at High: it is served ahead of a freshly spawned Normal.
        let normal = fake_spawn(&mut table);
        let order: Vec<Pid> = std::iter::from_fn(|| table.claim_next()).take(2).collect();
        assert_eq!(order, vec![pid, normal], "yielded High beats Normal");
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn set_priority_promotes_next_enqueue() {
        let mut table = TestTable::new();
        let promoted = spawn_parked(&mut table, Priority::High);
        let normal = fake_spawn(&mut table);
        table.transition(promoted, ProcessState::Runnable);

        let order: Vec<Pid> = std::iter::from_fn(|| table.claim_next()).take(2).collect();
        assert_eq!(
            order,
            vec![promoted, normal],
            "set_priority lands the next enqueue at High"
        );
        assert_eq!(table.counters().violations, 0);
    }
}
