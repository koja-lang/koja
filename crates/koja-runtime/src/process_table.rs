//! Generational slotmap of live processes plus the scheduler's ready queue
//! and timer / deadline min-heaps.
//!
//! Replaces the old `Vec<Process>` indexed by `pid - 1`. A PID packs a slot
//! index and a generation: `pid = (generation << 32) | index`. Slots are
//! reused after a process dies (so memory is bounded), and the generation is
//! bumped on free so a stale `Ref` to a recycled slot fails the lookup
//! ([`ProcessTable::get`] returns `None` -> `ProcessDown`) instead of
//! aliasing the new occupant.
//!
//! All state changes funnel through [`ProcessTable::transition`], which keeps
//! the ready queue and the live / active counts in sync. The ready queue
//! makes process pickup O(1), and the two min-heaps make deadline promotion
//! and timer firing O(log n) instead of full O(n) scans every worker turn.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::time::Instant;

use crate::mailbox::{Mailbox, WaitTarget};
use crate::sched_trace::{SchedTrace, TraceEntry, TraceEvent};
use crate::scheduler::{Process, ProcessFn, ProcessStack, ProcessState, Reclaim};
use crate::tsan::{self, Fiber};
use crate::wire::{Envelope, OwnedPayload};

/// Splits a packed PID into `(slot_index, generation)`.
fn decode(pid: i64) -> (u32, u32) {
    ((pid & 0xFFFF_FFFF) as u32, (pid >> 32) as u32)
}

/// Packs a slot index and generation into a PID. Generation starts at 1, so a
/// live PID is always `>= 2^32` and `0` is never a valid handle.
fn encode(index: u32, generation: u32) -> i64 {
    ((generation as i64) << 32) | (index as i64)
}

/// One slot in the table. `process` is `None` when the slot is free;
/// `generation` is bumped on free so recycled slots reject stale PIDs. The
/// TSan fiber is bound to the slot (not the process) and reused across the
/// slot's successive occupants — see [`crate::tsan`].
struct Slot {
    generation: u32,
    process: Option<Process>,
    tsan_fiber: Fiber,
}

/// A pending delayed message (`send_after`). Ordered in the min-heap by
/// `(fire_at, seq)`; `seq` is a unique tie-breaker so the order is total.
/// The message is staged as a finished [`Envelope`] at schedule time, so
/// firing is just a delivery and an unfired or undeliverable entry
/// reclaims its payload by dropping the envelope.
pub(crate) struct TimerEntry {
    pub(crate) envelope: Envelope,
    fire_at: Instant,
    seq: u64,
    pub(crate) target_pid: i64,
}

impl PartialEq for TimerEntry {
    fn eq(&self, other: &Self) -> bool {
        self.fire_at == other.fire_at && self.seq == other.seq
    }
}

impl Eq for TimerEntry {}

impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimerEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.fire_at, self.seq).cmp(&(other.fire_at, other.seq))
    }
}

/// A pending receive deadline. Ordered by `(deadline, seq)`.
#[derive(Eq, Ord, PartialEq, PartialOrd)]
struct DeadlineEntry {
    deadline: Instant,
    seq: u64,
    pid: i64,
}

/// Whether a state counts toward the `active` tally (work the reactor or
/// another worker will make progress on without a timer wakeup).
fn is_active(state: ProcessState) -> bool {
    matches!(state, ProcessState::Running | ProcessState::WaitingIo)
}

/// Whether `from -> to` is a legal process lifecycle edge.
///
/// Built from the audited transition sites: a worker claims a fresh or woken
/// process (`Created`/`Runnable -> Running`); a running process blocks on a
/// message or I/O (`Running -> Blocked`/`WaitingIo`); a wake re-arms a parked
/// process (`Blocked`/`WaitingIo -> Runnable`); and any live process can die
/// via return (`Running -> Dead`) or a kill from another worker (`* -> Dead`).
/// No legal edge is a self-edge, because every call site gates its
/// precondition first.
fn is_legal_transition(from: ProcessState, to: ProcessState) -> bool {
    use ProcessState::*;
    matches!(
        (from, to),
        (Created | Runnable, Running)
            | (Running, Blocked | WaitingIo | Dead)
            | (Blocked | WaitingIo, Runnable)
            | (Created | Runnable | Blocked | WaitingIo, Dead)
    )
}

/// Monotonic invariant counters bumped at the [`ProcessTable`]
/// chokepoints while the `SCHED` lock is already held. `violations` is
/// the machine oracle (must stay zero); the rest are coverage evidence
/// — e.g. a kill-storm fixture observing `parks_refused > 0` knows the
/// kill-vs-park window was actually hit, not merely survived. Exposed
/// to lang fixtures via `koja_rt_sched_violations` /
/// `koja_rt_parks_refused`.
pub(crate) struct ScheduleCounters {
    /// Kills that found the target `on_cpu` and deferred reclaim.
    pub(crate) kills_deferred: u64,
    /// Parks refused because the target was already `Dead` (or stale).
    pub(crate) parks_refused: u64,
    /// Ready-queue entries skipped by `claim_next` (killed, already
    /// resumed, or still `on_cpu`).
    pub(crate) stale_claims_skipped: u64,
    /// Deadline-heap entries rejected by `promote_due_deadlines`.
    pub(crate) stale_deadlines_skipped: u64,
    /// Envelopes bounced off a dead or stale target.
    pub(crate) undeliverable_envelopes: u64,
    /// Illegal lifecycle edges applied by `transition`. Always zero in
    /// a correct runtime; counted (not just debug-asserted) so release
    /// builds can detect ordering bugs too.
    pub(crate) violations: u64,
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

/// The scheduler's process store: a generational slotmap with a ready queue
/// and timer / deadline min-heaps. Protected by `crate::scheduler::SCHED`.
pub(crate) struct ProcessTable {
    /// Earliest receive deadlines, validated on pop (a process woken by a
    /// message before its deadline leaves a stale entry behind).
    deadlines: BinaryHeap<Reverse<DeadlineEntry>>,
    /// Indices of free slots available for reuse.
    free: Vec<u32>,
    /// First spawned process (the program entry). Drives signal delivery and
    /// the shutdown decision; `0` until the first spawn.
    main_pid: i64,
    /// Packed PIDs ready to run, in arrival order.
    ready: VecDeque<i64>,
    /// All slots, indexed by a PID's low 32 bits.
    slots: Vec<Slot>,
    /// Pending delayed messages, soonest first.
    timers: BinaryHeap<Reverse<TimerEntry>>,
    /// Count of processes not yet `Dead` (shutdown when this hits zero).
    alive: usize,
    /// Count of `Running` + `WaitingIo` processes (park-timeout heuristic).
    active: usize,
    /// Invariant counters, exposed to fixtures via `koja_rt_sched_*`.
    counters: ScheduleCounters,
    /// Monotonic tie-breaker for deadline-heap ordering.
    deadline_seq: u64,
    /// Monotonic tie-breaker for timer-heap ordering.
    timer_seq: u64,
    /// Lifecycle event ring, dumped at shutdown under `KOJA_SCHED_TRACE`.
    trace: SchedTrace,
}

impl ProcessTable {
    pub(crate) const fn new() -> Self {
        Self {
            active: 0,
            alive: 0,
            counters: ScheduleCounters::new(),
            deadline_seq: 0,
            deadlines: BinaryHeap::new(),
            free: Vec::new(),
            main_pid: 0,
            ready: VecDeque::new(),
            slots: Vec::new(),
            timer_seq: 0,
            timers: BinaryHeap::new(),
            trace: SchedTrace::new(),
        }
    }

    /// The invariant counters (read under the `SCHED` lock).
    pub(crate) fn counters(&self) -> &ScheduleCounters {
        &self.counters
    }

    /// Recorded lifecycle events, oldest first.
    pub(crate) fn trace_entries(&self) -> impl Iterator<Item = &TraceEntry> {
        self.trace.iter()
    }

    /// The program entry process, or `0` before the first spawn.
    pub(crate) fn main_pid(&self) -> i64 {
        self.main_pid
    }

    /// Looks up a process by packed PID, validating the generation. Returns
    /// `None` for an out-of-range, freed, or recycled (stale) PID.
    pub(crate) fn get(&self, pid: i64) -> Option<&Process> {
        let (index, generation) = decode(pid);
        let slot = self.slots.get(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        slot.process.as_ref()
    }

    /// Mutable [`get`](Self::get).
    pub(crate) fn get_mut(&mut self, pid: i64) -> Option<&mut Process> {
        let (index, generation) = decode(pid);
        let slot = self.slots.get_mut(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        slot.process.as_mut()
    }

    /// Whether `pid` resolves to a live (non-`Dead`) process. Stale and
    /// freed PIDs are not alive.
    pub(crate) fn is_alive(&self, pid: i64) -> bool {
        self.get(pid)
            .is_some_and(|process| process.state != ProcessState::Dead)
    }

    /// Registers a new process in a free (or freshly grown) slot, queues it
    /// as runnable, and returns its packed PID. Creates the process's TSan
    /// fiber here so spawn / free own the fiber lifecycle.
    pub(crate) fn spawn(
        &mut self,
        func: ProcessFn,
        init_state: OwnedPayload,
        stack: ProcessStack,
        sp: *mut u8,
    ) -> i64 {
        let (index, generation) = match self.free.pop() {
            Some(index) => (index, self.slots[index as usize].generation),
            None => {
                let index = self.slots.len() as u32;
                self.slots.push(Slot {
                    generation: 1,
                    process: None,
                    tsan_fiber: tsan::create_process_fiber(),
                });
                (index, 1)
            }
        };

        let pid = encode(index, generation);
        self.slots[index as usize].process = Some(Process::new(func, init_state, stack, sp));
        self.alive += 1;
        self.ready.push_back(pid);
        if self.main_pid == 0 {
            self.main_pid = pid;
        }
        pid
    }

    /// Reclaims a dead process's slot: detaches its resources (to drop
    /// off-lock), bumps the generation, and returns the slot to the freelist.
    /// Idempotent — a second call on the same PID returns `None`.
    pub(crate) fn free(&mut self, pid: i64) -> Option<Reclaim> {
        let (index, generation) = decode(pid);
        let reclaim = {
            let slot = self.slots.get_mut(index as usize)?;
            if slot.generation != generation {
                return None;
            }
            let reclaim = slot.process.as_mut()?.take_resources();
            slot.process = None;
            slot.generation = slot.generation.wrapping_add(1);
            reclaim
        };
        self.free.push(index);
        self.trace.record(pid, TraceEvent::Freed);
        Some(reclaim)
    }

    /// Single chokepoint for lifecycle state changes. Counts (and, in debug
    /// builds, asserts) illegal edges, keeps the `alive` / `active` counts
    /// current, and enqueues the PID when it becomes `Runnable`. A `None`
    /// lookup (stale PID) is a no-op so racing wakeups against a freed slot
    /// are harmless.
    pub(crate) fn transition(&mut self, pid: i64, to: ProcessState) {
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
        }
        if to == ProcessState::Runnable {
            self.ready.push_back(pid);
        }
    }

    /// Parks `pid` as `Blocked`, recording which part of its mailbox it
    /// waits on and an optional wake deadline. Refuses — returning `false`
    /// without touching the state — when the process is dead or stale: a
    /// kill can land while the process is mid-run on another worker
    /// (`* -> Dead` is a legal cross-worker edge), and parking over the
    /// tombstone would resurrect it. A refused caller should still yield;
    /// the worker sees `Dead` on switch-out and reclaims the slot, so the
    /// frame never resumes.
    pub(crate) fn try_park(
        &mut self,
        pid: i64,
        target: WaitTarget,
        deadline: Option<Instant>,
    ) -> bool {
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

    /// Parks `pid` as `WaitingIo`, with the same kill-tombstone refusal as
    /// [`try_park`](Self::try_park). A refused caller must not register
    /// the fd with the reactor — there is no waiter to wake.
    pub(crate) fn try_park_io(&mut self, pid: i64) -> bool {
        if !self.is_alive(pid) {
            self.note_refused_park(pid);
            return false;
        }
        self.transition(pid, ProcessState::WaitingIo);
        true
    }

    fn note_refused_park(&mut self, pid: i64) {
        self.counters.parks_refused += 1;
        self.trace.record(pid, TraceEvent::ParkRefused);
    }

    /// Marks `pid` `Dead` unless it already is (a racing kill may have won)
    /// or the slot is stale — re-marking would be an illegal self-edge and
    /// would double-decrement the `alive` count. Returns whether this call
    /// performed the transition.
    pub(crate) fn mark_dead_if_alive(&mut self, pid: i64) -> bool {
        if !self.is_alive(pid) {
            return false;
        }
        self.transition(pid, ProcessState::Dead);
        true
    }

    /// The "last resort" termination primitive: marks `pid` `Dead` and
    /// reclaims its slot, returning the detached resources for the caller
    /// to drop off-lock. `None` when the target was already dead/stale, or
    /// when it is still `on_cpu` — reclaiming a stack a worker is running
    /// on would be a use-after-free, so the owning worker reclaims on
    /// switch-out instead (it sees `Dead` in [`after_switch`](Self::after_switch)
    /// after persisting `sp`). The mailbox rides along either way, so
    /// envelopes are freed exactly once.
    pub(crate) fn kill(&mut self, pid: i64) -> Option<Reclaim> {
        if !self.mark_dead_if_alive(pid) {
            return None;
        }
        if self.get(pid).is_some_and(|process| process.on_cpu) {
            self.counters.kills_deferred += 1;
            self.trace.record(pid, TraceEvent::KillDeferred);
            None
        } else {
            self.free(pid)
        }
    }

    /// Pops the next claimable process, marking it `Running` and `on_cpu`.
    /// Skips stale ready-queue entries — killed, already resumed, or still
    /// `on_cpu` in the publish-before-save-`sp` window (the owning worker
    /// re-queues those from [`after_switch`](Self::after_switch)).
    pub(crate) fn claim_next(&mut self) -> Option<(i64, *mut u8, Fiber)> {
        while let Some(pid) = self.ready.pop_front() {
            match self.get_mut(pid) {
                Some(process)
                    if !process.on_cpu
                        && matches!(
                            process.state,
                            ProcessState::Created | ProcessState::Runnable
                        ) =>
                {
                    process.on_cpu = true;
                }
                _ => {
                    self.counters.stale_claims_skipped += 1;
                    continue;
                }
            }
            self.transition(pid, ProcessState::Running);
            let (index, _) = decode(pid);
            let slot = &self.slots[index as usize];
            let sp = slot
                .process
                .as_ref()
                .expect("claimed process must exist")
                .sp;
            return Some((pid, sp, slot.tsan_fiber));
        }
        None
    }

    /// After a process yields back to its worker, persists its saved `sp`,
    /// releases the `on_cpu` claim, and then either re-queues it (woken during
    /// the `on_cpu` window) or reclaims its slot (dead). Returns detached
    /// resources for the caller to drop after releasing the lock.
    pub(crate) fn after_switch(&mut self, pid: i64, saved_sp: *mut u8) -> Option<Reclaim> {
        let state = {
            let process = self.get_mut(pid)?;
            process.sp = saved_sp;
            process.on_cpu = false;
            process.state
        };
        match state {
            ProcessState::Dead => self.free(pid),
            ProcessState::Created | ProcessState::Runnable => {
                self.ready.push_back(pid);
                None
            }
            _ => None,
        }
    }

    /// Routes `envelope` into a process's mailbox (see
    /// [`Mailbox::push`]), waking the process if it is parked waiting on
    /// the part of the mailbox this envelope satisfies. Returns an
    /// envelope the caller must drop after releasing the lock: the
    /// original when the target is gone or dead, or a stale reply
    /// displaced from the reply slot.
    pub(crate) fn deliver(&mut self, pid: i64, envelope: Envelope) -> Option<Envelope> {
        if !self.is_alive(pid) {
            self.counters.undeliverable_envelopes += 1;
            self.trace.record(pid, TraceEvent::Undeliverable);
            return Some(envelope);
        }
        let target = Mailbox::target_of(&envelope);
        let process = self
            .get_mut(pid)
            .expect("is_alive implies the process exists");
        let wake = process.state == ProcessState::Blocked && process.waiting == target;
        let displaced = process.mailbox.push(envelope);
        self.trace.record(pid, TraceEvent::Delivered);
        if wake {
            self.transition(pid, ProcessState::Runnable);
        }
        displaced
    }

    /// Schedules a delayed message. Cancellation is lazy: a timer aimed at a
    /// process that later dies is simply dropped (undeliverable) when it
    /// fires, reclaiming its envelope then.
    pub(crate) fn push_timer(&mut self, fire_at: Instant, target_pid: i64, envelope: Envelope) {
        self.timer_seq += 1;
        self.timers.push(Reverse(TimerEntry {
            envelope,
            fire_at,
            seq: self.timer_seq,
            target_pid,
        }));
    }

    /// Removes and returns every timer whose `fire_at` is at or before `now`,
    /// soonest first. The caller delivers each staged envelope.
    pub(crate) fn take_due_timers(&mut self, now: Instant) -> Vec<TimerEntry> {
        let mut due = Vec::new();
        while self
            .timers
            .peek()
            .is_some_and(|Reverse(entry)| entry.fire_at <= now)
        {
            due.push(self.timers.pop().unwrap().0);
        }
        due
    }

    /// Records a receive deadline so the worker loop can promote the waiter
    /// back to `Runnable` when it expires.
    pub(crate) fn push_deadline(&mut self, pid: i64, deadline: Instant) {
        self.deadline_seq += 1;
        self.deadlines.push(Reverse(DeadlineEntry {
            deadline,
            pid,
            seq: self.deadline_seq,
        }));
    }

    /// Promotes every process whose receive deadline has passed. Stale entries
    /// (the process was woken by a message, resumed, or died, or re-blocked
    /// with a different deadline) are validated against the live state and
    /// skipped.
    pub(crate) fn promote_due_deadlines(&mut self, now: Instant) {
        while self
            .deadlines
            .peek()
            .is_some_and(|Reverse(entry)| entry.deadline <= now)
        {
            let entry = self.deadlines.pop().unwrap().0;
            let expired = matches!(
                self.get(entry.pid),
                Some(process)
                    if process.state == ProcessState::Blocked
                        && process.deadline == Some(entry.deadline)
            );
            if expired {
                self.transition(entry.pid, ProcessState::Runnable);
            } else {
                self.counters.stale_deadlines_skipped += 1;
            }
        }
    }

    /// Whether any process is `Running` or `WaitingIo`.
    pub(crate) fn any_active(&self) -> bool {
        self.active != 0
    }

    /// Whether the runtime should tear down: no live processes remain, or the
    /// program entry process has died (or its slot is already reclaimed).
    pub(crate) fn should_shutdown(&self) -> bool {
        self.alive == 0 || (self.main_pid != 0 && !self.is_alive(self.main_pid))
    }

    /// The soonest pending timer or deadline, for sizing the idle park.
    pub(crate) fn nearest_wakeup(&self) -> Option<Instant> {
        let timer = self.timers.peek().map(|Reverse(entry)| entry.fire_at);
        let deadline = self.deadlines.peek().map(|Reverse(entry)| entry.deadline);
        match (timer, deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;
    use std::time::Duration;

    extern "C" fn noop_entry(_state: *const u8) {}

    /// A process with null resources: every owned handle drops as a no-op, so
    /// fake processes are cheap to create and tear down in tests.
    fn fake_spawn(table: &mut ProcessTable) -> i64 {
        table.spawn(
            noop_entry,
            OwnedPayload::default(),
            ProcessStack::null(),
            ptr::null_mut(),
        )
    }

    /// A minimal business envelope (empty payload, no glue).
    fn fake_envelope() -> Envelope {
        unsafe { Envelope::from_payload(crate::wire::TAG_BUSINESS, ptr::null(), 0, None) }
    }

    #[test]
    fn encode_decode_roundtrip() {
        for (index, generation) in [(0u32, 1u32), (5, 2), (0xFFFF, 0x1234)] {
            let pid = encode(index, generation);
            assert_eq!(decode(pid), (index, generation));
        }
    }

    #[test]
    fn first_pid_is_index_zero_generation_one() {
        let mut table = ProcessTable::new();
        let pid = fake_spawn(&mut table);
        assert_eq!(decode(pid), (0, 1));
        assert!(table.get(pid).is_some());
        assert_eq!(table.main_pid(), pid);
    }

    #[test]
    fn free_then_spawn_reuses_slot_with_bumped_generation() {
        // Drive to Dead through a legal path: Created -> Running -> Dead.
        let mut table = ProcessTable::new();
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
        let mut table = ProcessTable::new();
        let pid = fake_spawn(&mut table);
        let (index, generation) = decode(pid);
        let stale = encode(index, generation + 7);
        assert!(table.get(stale).is_none());
        assert!(table.get(0).is_none(), "pid 0 never valid");
    }

    #[test]
    fn ready_queue_is_fifo() {
        let mut table = ProcessTable::new();
        let a = fake_spawn(&mut table);
        let b = fake_spawn(&mut table);
        let c = fake_spawn(&mut table);
        let order: Vec<i64> = std::iter::from_fn(|| table.claim_next().map(|(pid, _, _)| pid))
            .take(3)
            .collect();
        assert_eq!(order, vec![a, b, c]);
    }

    #[test]
    fn timer_heap_pops_in_fire_order() {
        let mut table = ProcessTable::new();
        let base = Instant::now();
        table.push_timer(base + Duration::from_millis(30), 1, fake_envelope());
        table.push_timer(base + Duration::from_millis(10), 2, fake_envelope());
        table.push_timer(base + Duration::from_millis(20), 3, fake_envelope());

        let due = table.take_due_timers(base + Duration::from_millis(25));
        let pids: Vec<i64> = due.iter().map(|entry| entry.target_pid).collect();
        assert_eq!(pids, vec![2, 3], "soonest-first, only due timers");
        assert_eq!(
            table.nearest_wakeup(),
            Some(base + Duration::from_millis(30)),
            "remaining timer still pending"
        );
    }

    #[test]
    fn park_refuses_killed_process() {
        // A cross-worker kill can land while the victim is mid-run; its
        // next park must not resurrect it over the `Dead` tombstone.
        let mut table = ProcessTable::new();
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
        let mut table = ProcessTable::new();
        let pid = fake_spawn(&mut table);
        let (claimed, sp, _) = table.claim_next().unwrap();
        assert_eq!(claimed, pid);

        // The worker is mid-run on this stack: kill must not reclaim it.
        assert!(table.kill(pid).is_none(), "reclaim deferred to the worker");
        assert_eq!(table.counters().kills_deferred, 1);

        // The owning worker switches out, sees `Dead`, and reclaims.
        assert!(table.after_switch(pid, sp).is_some());
        assert!(table.kill(pid).is_none(), "second kill is a no-op");
        assert_eq!(table.counters().violations, 0);
    }

    #[test]
    fn kill_reclaims_parked_process_directly() {
        let mut table = ProcessTable::new();
        let pid = fake_spawn(&mut table);
        let (_, sp, _) = table.claim_next().unwrap();
        assert!(table.try_park(pid, WaitTarget::Receive, None));
        assert!(table.after_switch(pid, sp).is_none(), "parked, not dead");

        assert!(table.kill(pid).is_some(), "off-cpu target frees here");
        assert_eq!(table.counters().kills_deferred, 0);
        assert!(table.get(pid).is_none(), "slot reclaimed");
    }

    #[test]
    fn undeliverable_envelope_bounces_and_counts() {
        let mut table = ProcessTable::new();
        let pid = fake_spawn(&mut table);
        table.transition(pid, ProcessState::Running);
        table.mark_dead_if_alive(pid);

        let bounced = table.deliver(pid, fake_envelope());
        assert!(bounced.is_some(), "caller must reclaim the envelope");
        assert_eq!(table.counters().undeliverable_envelopes, 1);
    }

    #[test]
    fn stale_ready_entry_skip_is_counted() {
        let mut table = ProcessTable::new();
        let pid = fake_spawn(&mut table);
        table.kill(pid);

        assert!(table.claim_next().is_none(), "killed entry never claimed");
        assert_eq!(table.counters().stale_claims_skipped, 1);
    }

    #[test]
    fn stale_deadline_skip_is_counted() {
        let mut table = ProcessTable::new();
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
    #[cfg(debug_assertions)]
    #[should_panic(expected = "illegal process state transition")]
    fn illegal_transition_asserts_in_debug() {
        let mut table = ProcessTable::new();
        let pid = fake_spawn(&mut table);
        // `Created -> Blocked` is not a legal edge.
        table.transition(pid, ProcessState::Blocked);
    }

    #[test]
    fn mark_dead_if_alive_is_idempotent() {
        let mut table = ProcessTable::new();
        let pid = fake_spawn(&mut table);
        table.transition(pid, ProcessState::Running);

        assert!(table.mark_dead_if_alive(pid));
        assert!(!table.mark_dead_if_alive(pid), "second kill is a no-op");
        assert!(!table.mark_dead_if_alive(0), "stale PID is a no-op");
        assert!(table.should_shutdown(), "alive count decremented once");
    }

    #[test]
    fn try_park_records_wait_target_and_deadline() {
        let mut table = ProcessTable::new();
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
        let mut table = ProcessTable::new();
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
}
