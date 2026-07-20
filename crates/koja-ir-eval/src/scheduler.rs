//! Eval's cooperative implementation of the `koja-runtime-core`
//! scheduler protocol, the second implementor after the native
//! `koja-runtime-posix` adapter.
//!
//! The defining asymmetry with native: eval's [`Executor`] **is** the
//! interpreter. A process is an `async` interpreter future owned here in
//! [`EvalExecutor`], and [`resume`](EvalExecutor::resume) polls it until
//! it next awaits (a `receive` park) or completes. Because the suspended
//! state lives inside the boxed future rather than a saved stack pointer,
//! the protocol's `Execution` and `Continuation` are both `()`. There is
//! nothing for the driver to marshal across the resume, which is exactly
//! the [`CooperativeDriver`]'s `Continuation = ()` contract.
//!
//! The run loop itself is the shared [`CooperativeDriver`]. This module
//! supplies the capabilities behind it. The running process reaches back
//! into the core table through the [`CORE`] and [`CURRENT_PID`]
//! thread-locals, so `receive` and `spawn` need no parameter threading
//! through the interpreter. `spawn` requests are staged in
//! [`PENDING_SPAWNS`] and fulfilled by the executor (which holds the
//! program) right after the spawning resume.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll, Wake};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use koja_ir::IRSymbol;
use koja_runtime_core::{
    Clock, CooperativeDriver, CrashInfo, Executor, ExitReason, Lifecycle, Message, MessageSource,
    Pid, Priority, ProcessTable, Readiness, SignalSource, Tag, WaitTarget,
};

use crate::interpreter::{CallResolver, build_exit_signal_value};
use crate::reactor::EvalReactor;
use crate::value::Value;

/// The cooperative process table: agnostic control blocks (no executor
/// execution state, hence `()`) keyed against eval's typed message repr.
pub(crate) type EvalTable = ProcessTable<(), EvalMessage>;

/// Shared owner of the core table. The driver loop and the per-process
/// await points (which park / peek the mailbox) all reach the table
/// through this handle. The borrow is held only across single core
/// operations, never across a `resume`.
pub(crate) type CoreHandle = Rc<RefCell<EvalTable>>;

/// A suspended process body: the interpreter's `async` call tree, boxed
/// so the executor can store and re-poll it across suspensions. Borrows
/// the program for the run (`'a`), so it is not `'static`.
pub(crate) type ProcessFuture<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

/// The fully-applied cooperative driver for eval, generic over the
/// [`CallResolver`] backing the run: an `IRProgram` in project mode, an
/// `IRScript` in script mode. Both can `spawn` processes, so the executor
/// builds children's futures off whichever resolver it holds.
pub(crate) type EvalDriver<'a, R> =
    CooperativeDriver<EvalExecutor<'a, R>, EvalReactor, EvalClock, EvalSignals>;

thread_local! {
    /// The core table for the in-flight `run_program`, installed for the
    /// duration of the run. The cooperative analog of native's global
    /// `SCHED`, per-thread so parallel test runs stay isolated.
    static CORE: RefCell<Option<CoreHandle>> = const { RefCell::new(None) };
    /// The PID the executor is currently resuming, set around each
    /// `resume`. Mirrors native's per-worker `CURRENT_PID`.
    static CURRENT_PID: Cell<Pid> = const { Cell::new(0) };
    /// Reductions the resuming process may still spend before a `YieldCheck`
    /// forces it to yield. Seeded from the PCB's budget on each resume so the
    /// per-check decrement is a plain `Cell` write, never a table borrow.
    /// The cooperative analog of native's `REDUCTIONS_LEFT`.
    static REDUCTIONS_LEFT: Cell<u32> = const { Cell::new(0) };
    /// `spawn` requests raised during a resume, drained and fulfilled by
    /// the executor before the driver claims the next process.
    static PENDING_SPAWNS: RefCell<Vec<PendingSpawn>> = const { RefCell::new(Vec::new()) };
    /// Monotonic `Ref.call` correlation-token source, the cooperative
    /// analog of native's `koja_rt_call_token`. Reset per run so token
    /// values stay deterministic across parallel tests.
    static NEXT_TOKEN: Cell<i64> = const { Cell::new(1) };
}

/// A staged `spawn`: the child PID is already allocated in the table (so
/// the spawning process can return a stable `Ref`), and the executor
/// builds and installs the child's future from `wrapper` + `config` right
/// after the resume that raised it.
struct PendingSpawn {
    config: Value,
    pid: Pid,
    wrapper: IRSymbol,
}

/// Clears the per-run thread-local state on drop, so a panic mid-run
/// can't leak the installed core (test threads are reused).
pub(crate) struct RuntimeGuard;

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        CORE.with(|core| *core.borrow_mut() = None);
        CURRENT_PID.with(|pid| pid.set(0));
        REDUCTIONS_LEFT.with(|remaining| remaining.set(0));
        PENDING_SPAWNS.with(|queue| queue.borrow_mut().clear());
        NEXT_TOKEN.with(|token| token.set(1));
    }
}

/// Installs `core` as the running table for the current `run_program`.
/// The returned guard restores the thread-locals when dropped.
pub(crate) fn install_runtime(core: CoreHandle) -> RuntimeGuard {
    CORE.with(|slot| *slot.borrow_mut() = Some(core));
    CURRENT_PID.with(|pid| pid.set(0));
    REDUCTIONS_LEFT.with(|remaining| remaining.set(0));
    PENDING_SPAWNS.with(|queue| queue.borrow_mut().clear());
    NEXT_TOKEN.with(|token| token.set(1));
    RuntimeGuard
}

/// The PID of the process the executor is currently resuming.
pub(crate) fn current_pid() -> Pid {
    CURRENT_PID.with(Cell::get)
}

/// Mark the currently-running process `Crashed` with its rendered
/// [`CrashInfo`], so the `mark_dead_if_alive` on resume reports a crash
/// rather than the default `Normal`.
pub(crate) fn record_crash(crash_info: CrashInfo) {
    let pid = current_pid();
    with_table(|table| {
        table.set_exit_reason(pid, ExitReason::Crashed);
        table.set_crash_info(pid, crash_info);
    });
}

/// Runs `f` against the installed core table. Panics if no runtime is
/// installed, since `receive` / `spawn` only run inside a driven process.
fn with_table<T>(f: impl FnOnce(&mut EvalTable) -> T) -> T {
    CORE.with(|slot| {
        let guard = slot.borrow();
        let handle = guard
            .as_ref()
            .expect("eval runtime not installed: receive/spawn ran outside a process");
        let mut table = handle.borrow_mut();
        f(&mut table)
    })
}

/// Pops the next received message (system traffic before business) for
/// `pid`, or `None` when its receive queues are empty.
pub(crate) fn pop_received(pid: Pid) -> Option<EvalMessage> {
    with_table(|table| {
        table
            .get_mut(pid)
            .and_then(|pcb| pcb.mailbox.pop_received())
    })
}

/// Parks `pid` as `Blocked` on its receive queues, with an optional wake
/// deadline. The caller then yields ([`YieldOnce`]) so the driver regains
/// control until a delivery or the deadline promotes the process.
pub(crate) fn park_receive(pid: Pid, deadline: Option<Instant>) {
    with_table(|table| table.try_park(pid, WaitTarget::Receive, deadline));
}

/// Parks `pid` as `Blocked` on its one-shot reply slot, with an optional
/// timeout deadline. Used by `Ref.call` so only a reply delivery (not
/// queued business/lifecycle traffic) wakes the caller. Calls are atomic.
pub(crate) fn park_reply(pid: Pid, deadline: Option<Instant>) {
    with_table(|table| table.try_park(pid, WaitTarget::Reply, deadline));
}

/// Disarms `pid`'s wake deadline, cancelling its timer-wheel entry.
/// Called on the wake paths (message arrival or timeout expiry) so
/// completed waits leave nothing behind in the wheel.
pub(crate) fn clear_deadline(pid: Pid) {
    with_table(|table| table.clear_deadline(pid));
}

/// Parks `pid` as `WaitingIO` for the reactor (`io_block`). Returns
/// whether the park took. A refused park means a kill landed mid-run, and
/// the caller must not arm the fd because there is no waiter to wake.
pub(crate) fn park_io(pid: Pid) -> bool {
    with_table(|table| table.try_park_io(pid))
}

/// Whether a cooperative run is in flight on this thread (the driver is
/// looping over an installed core). The reactor consults this to choose
/// its `io_block` strategy: cooperative park (process mode) vs. blocking
/// the single thread on the fd (function mode, no driver).
pub(crate) fn runtime_installed() -> bool {
    CORE.with(|slot| slot.borrow().is_some())
}

/// Illegal state-transition count for the installed cooperative core,
/// eval's analog of the native `koja_rt_sched_violations` oracle. Reads
/// eval's own `ProcessTable` (not the native `SCHED`, which never runs
/// under eval) so the kill/park race fixtures actually exercise the
/// cooperative scheduler's transition guard.
pub(crate) fn sched_violations() -> i64 {
    with_table(|table| table.counters().violations as i64)
}

/// Routes `message` into `pid`'s mailbox, waking it if it is parked on the
/// matching target. A bounced (target gone) or displaced (stale reply)
/// message is dropped here, off the table borrow.
pub(crate) fn deliver(pid: Pid, message: EvalMessage) {
    let leftover = with_table(|table| table.deliver(pid, message));
    drop(leftover);
}

/// Schedules `message` for delivery to `pid` at `fire_at` (`Ref.send_after`).
pub(crate) fn schedule_timer(pid: Pid, fire_at: Instant, message: EvalMessage) {
    with_table(|table| table.push_timer(fire_at, pid, message));
}

/// Takes the pending reply from `pid`'s one-shot reply slot, if one has
/// landed (`Ref.call`'s resume check).
pub(crate) fn take_reply(pid: Pid) -> Option<EvalMessage> {
    with_table(|table| table.get_mut(pid).and_then(|pcb| pcb.mailbox.take_reply()))
}

/// Registers `pid` as awaiting the reply for `token` (`Ref.call`), so
/// `reply` can tell whether the caller is still listening.
pub(crate) fn set_awaiting_reply(pid: Pid, token: i64) {
    with_table(|table| table.set_awaiting_reply(pid, token));
}

/// Clears `pid`'s awaited-reply token once its call completes.
pub(crate) fn clear_awaiting_reply(pid: Pid) {
    with_table(|table| table.clear_awaiting_reply(pid));
}

/// Routes `value` to `coords.caller_pid`'s reply slot if that process is
/// still awaiting `coords.token`, returning whether it was delivered. A
/// reply for a caller that already gave up is dropped here (`false`), the
/// cooperative analog of native's `reply_or_expire`.
pub(crate) fn reply(coords: ReplyInfo, value: Value) -> bool {
    let caller = coords.caller_pid;
    let token = coords.token;
    with_table(|table| {
        if !table.is_awaiting_reply(caller, token) {
            return false;
        }
        let leftover = table.deliver(
            caller,
            EvalMessage {
                reply: Some(coords),
                tag: Tag::Reply,
                value,
            },
        );
        drop(leftover);
        true
    })
}

/// Terminates `pid` (`Ref.kill`), dropping any reclaimed resources here.
pub(crate) fn kill(pid: Pid) {
    let reclaim = with_table(|table| table.kill(pid));
    drop(reclaim);
}

/// Registers the currently-resuming process as a monitor of `target`
/// (`Process.monitor`). Staged `ExitSignal`s are delivered by the
/// executor at the end of the current resume.
pub(crate) fn monitor(target: Pid) -> i64 {
    let watcher = current_pid();
    with_table(|table| table.monitor(watcher, target))
}

/// Removes the monitor identified by `token` (`Process.demonitor`).
pub(crate) fn demonitor(token: i64) {
    with_table(|table| table.demonitor(token));
}

/// Whether `pid` resolves to a live process (`Ref.alive?`).
pub(crate) fn is_alive(pid: Pid) -> bool {
    with_table(|table| table.is_alive(pid))
}

/// The currently-resuming process's parent (`Process.parent`), `None`
/// for the entry process.
pub(crate) fn parent() -> Option<Pid> {
    let pid = current_pid();
    with_table(|table| table.parent(pid))
}

/// Sets the currently-resuming process's scheduling priority from a
/// `Priority` variant index (0=Low, 1=Normal, 2=High). The cooperative
/// analog of native's `koja_rt_set_priority`.
pub(crate) fn set_priority(level: i64) {
    let pid = current_pid();
    with_table(|table| table.set_priority(pid, Priority::from_index(level)));
}

/// Records the currently-resuming process's exit reason from a wire code
/// (0=Normal, 1=Shutdown, ...). The cooperative analog of native's
/// `koja_rt_process_exit`. Runs in the process-body tail just before the
/// process completes, so the reason is set when `mark_dead_if_alive` fires.
pub(crate) fn process_exit(reason: i64) {
    let pid = current_pid();
    with_table(|table| table.set_exit_reason(pid, ExitReason::from_index(reason)));
}

/// The SIGTERM drain grace window, from `KOJA_GRACE_MS` (default 30s, to
/// match Kubernetes' `terminationGracePeriodSeconds`). After this elapses,
/// the driver force-kills any straggler. Read here so the core driver stays
/// env-free. The native adapter has its own copy.
pub(crate) fn grace_period() -> Duration {
    const DEFAULT_GRACE_MS: u64 = 30_000;
    let millis = std::env::var("KOJA_GRACE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_GRACE_MS);
    Duration::from_millis(millis)
}

/// Spends one reduction for the currently-resuming process. Returns `true`
/// when its budget is exhausted, having re-queued it (`Running -> Runnable`)
/// so the caller should yield ([`YieldOnce`]) and let the driver round-robin
/// to a peer. The cooperative analog of native's `koja_rt_yield_check`: the
/// common (not-exhausted) path is a lock-free [`REDUCTIONS_LEFT`] decrement,
/// touching the table only to re-queue at zero. A no-op (`false`) in function
/// mode, where IR runs with no driver to yield to.
pub(crate) fn reduce() -> bool {
    if !runtime_installed() {
        return false;
    }
    let exhausted = REDUCTIONS_LEFT.with(|remaining| {
        let next = remaining.get().saturating_sub(1);
        remaining.set(next);
        next == 0
    });
    if exhausted {
        let pid = current_pid();
        with_table(|table| table.yield_running(pid));
    }
    exhausted
}

/// Mints the next `Ref.call` correlation token. Monotonic within a run.
pub(crate) fn mint_token() -> i64 {
    NEXT_TOKEN.with(|cell| {
        let token = cell.get();
        cell.set(token + 1);
        token
    })
}

/// Allocates a child PCB and stages its `spawn` request. The PID is
/// returned immediately (for the `Ref` the spawning process produces).
/// The child's future is installed by the executor after this resume.
pub(crate) fn spawn_child(wrapper: IRSymbol, config: Value) -> Pid {
    // Refuse new processes once draining (SIGTERM seen): the program is
    // shutting down. The invalid pid 0 makes the returned `Ref` behave like
    // a ref to an already-dead process. Dropping `config` runs its glue.
    if with_table(|table| table.is_draining()) {
        return 0;
    }
    let pid = with_table(|table| table.spawn((), Some(current_pid())));
    PENDING_SPAWNS.with(|queue| {
        queue.borrow_mut().push(PendingSpawn {
            config,
            pid,
            wrapper,
        })
    });
    pid
}

/// A mailbox message carrying a typed [`Value`] (vs. the native byte
/// `Envelope`). `tag` is the routing class. `reply` carries the call/reply
/// correlation coordinates when present.
pub(crate) struct EvalMessage {
    /// The routing class (business / lifecycle / reply / IO-ready).
    pub tag: Tag,
    /// The payload: the message `M` for business traffic, the reply `R`
    /// for a reply, or the lifecycle variant index for a signal.
    pub value: Value,
    /// Correlation coordinates: `Some` for a `Ref.call` business request
    /// (the caller's `ReplyTo` coordinates, surfaced to the receiver as
    /// `Option::Some(ReplyTo { .. })`) and for a `ReplyTo.send` reply (the
    /// token the awaiting `call` matches on). `None` for cast / send_after
    /// business traffic and lifecycle signals.
    pub reply: Option<ReplyInfo>,
}

/// The `ReplyTo<R>` coordinates threaded through call/reply traffic: the
/// caller's PID plus a per-call correlation `token`. Mirrors the native
/// `ReplyTo` struct layout (`{ id, token }`) and the reply-token protocol
/// in `koja-ir-llvm/src/intrinsics/process.rs`.
pub(crate) struct ReplyInfo {
    pub caller_pid: Pid,
    pub token: i64,
}

impl Message for EvalMessage {
    fn tag(&self) -> Tag {
        self.tag
    }
}

/// Yields control back to the driver exactly once. The caller parks
/// itself in the table first. The first poll then returns `Pending` (the
/// driver won't re-resume a `Blocked` process), and the next poll, which
/// only happens after a delivery or deadline promotes the process,
/// returns `Ready`. The receive loop re-checks the mailbox after each.
pub(crate) struct YieldOnce {
    yielded: bool,
}

impl YieldOnce {
    pub(crate) fn new() -> Self {
        Self { yielded: false }
    }
}

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            Poll::Pending
        }
    }
}

/// The cooperative executor: owns the per-process interpreter futures and
/// the [`CallResolver`] (to build spawned children's futures and mint
/// `IOReady` values). Process execution state and the resume token both
/// stay `()`.
pub(crate) struct EvalExecutor<'a, R: CallResolver> {
    core: CoreHandle,
    futures: RefCell<HashMap<Pid, ProcessFuture<'a>>>,
    resolver: &'a R,
}

impl<'a, R: CallResolver> EvalExecutor<'a, R> {
    pub(crate) fn new(core: CoreHandle, resolver: &'a R) -> Self {
        Self {
            core,
            futures: RefCell::new(HashMap::new()),
            resolver,
        }
    }

    /// Register a process's body. The entry future is installed here by
    /// `run_program`. Children are installed by [`install_pending_spawns`].
    pub(crate) fn install_future(&self, pid: Pid, future: ProcessFuture<'a>) {
        self.futures.borrow_mut().insert(pid, future);
    }

    /// Builds and installs the futures for every `spawn` raised during the
    /// resume that just finished. Runs before the driver claims the next
    /// process, so a child's future is always present by the time it is
    /// picked up.
    fn install_pending_spawns(&self) {
        let pending: Vec<PendingSpawn> =
            PENDING_SPAWNS.with(|queue| queue.borrow_mut().drain(..).collect());
        for spawn in pending {
            let future =
                crate::interpreter::build_spawn_future(self.resolver, &spawn.wrapper, spawn.config);
            self.futures.borrow_mut().insert(spawn.pid, future);
        }
    }

    /// Settles the death edges from the resume that just finished:
    /// force-kills the staged kill-cascade targets until none remain
    /// (each kill can stage grandchildren), dropping their futures and
    /// reclaimed resources, then delivers the staged `ExitSignal`s.
    /// The single-threaded analog of native's per-death-site settles.
    fn settle_exits(&self) {
        loop {
            let staged = self.core.borrow_mut().take_pending_kills();
            if staged.is_empty() {
                break;
            }
            for pid in staged {
                let reclaim = self.core.borrow_mut().kill(pid);
                drop(reclaim);
                self.futures.borrow_mut().remove(&pid);
            }
        }
        self.deliver_exit_signals();
    }

    /// Synthesizes and delivers every staged `ExitSignal`, waking
    /// parked watchers.
    fn deliver_exit_signals(&self) {
        let notices = self.core.borrow_mut().take_exit_notices();
        for notice in notices {
            deliver(
                notice.watcher,
                EvalMessage {
                    reply: None,
                    tag: Tag::ExitSignal,
                    value: build_exit_signal_value(self.resolver, &notice),
                },
            );
        }
    }
}

impl<R: CallResolver> Executor for EvalExecutor<'_, R> {
    type Continuation = ();
    type Execution = ();
    type Message = EvalMessage;

    fn resume(&self, pid: Pid, _continuation: ()) {
        CURRENT_PID.with(|current| current.set(pid));
        // Seed this quantum's reduction budget (reset by `claim_next` on the
        // `-> Running` edge) so `reduce` decrements a `Cell` rather than the
        // table on every `YieldCheck`.
        let budget = self.core.borrow().reductions_left(pid);
        REDUCTIONS_LEFT.with(|remaining| remaining.set(budget));
        // Take the future out so the map is not borrowed across the poll.
        // A process whose future has already completed (or was killed) is
        // a no-op resume.
        let taken = self.futures.borrow_mut().remove(&pid);
        if let Some(mut future) = taken {
            let mut context = Context::from_waker(std::task::Waker::noop());
            // Backstop for an unexpected host unwind. A user `Kernel.panic` is
            // a `RuntimeError`, not a Rust panic, so it never reaches here.
            let poll = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                future.as_mut().poll(&mut context)
            }));
            match poll {
                Ok(Poll::Ready(())) => {
                    self.core.borrow_mut().mark_dead_if_alive(pid);
                }
                Ok(Poll::Pending) => {
                    self.futures.borrow_mut().insert(pid, future);
                }
                Err(_payload) => {
                    self.core
                        .borrow_mut()
                        .set_exit_reason(pid, ExitReason::Crashed);
                    self.core.borrow_mut().mark_dead_if_alive(pid);
                }
            }
        }
        self.install_pending_spawns();
        self.settle_exits();
    }
}

impl<R: CallResolver> MessageSource<EvalMessage> for EvalExecutor<'_, R> {
    fn lifecycle_message(&self, event: Lifecycle) -> EvalMessage {
        EvalMessage {
            reply: None,
            tag: Tag::Lifecycle,
            value: Value::Int(event as i64),
        }
    }

    fn io_ready_message(&self, readiness: Readiness, fd: i32) -> EvalMessage {
        EvalMessage {
            reply: None,
            tag: Tag::IOReady,
            value: crate::interpreter::build_io_ready_value(self.resolver, readiness, fd),
        }
    }
}

/// Monotonic time for receive deadlines and timer firing. Eval shares the
/// host clock with native, the same instant the LLVM backend would observe.
pub(crate) struct EvalClock;

impl Clock for EvalClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// OS lifecycle signals. Reuses the process-wide latching handlers shared
/// with the native scheduler ([`koja_runtime::signals`]). The driver
/// drains these into `Lifecycle` messages for the entry process.
///
/// Handlers are always installed (so a SIGTERM latches rather than killing
/// the host), but the latched flags are only **drained** when the program
/// actually has a `Lifecycle`-arm `receive` (`drain_lifecycle`). The flags
/// are process-global, so an eval run that ignores them must not consume
/// them out from under a concurrent run that wants them.
pub(crate) struct EvalSignals {
    drain_lifecycle: bool,
}

impl EvalSignals {
    pub(crate) fn new(drain_lifecycle: bool) -> Self {
        Self { drain_lifecycle }
    }
}

impl SignalSource for EvalSignals {
    fn install(&self) {
        koja_runtime::signals::install();
    }

    fn drain(&self) -> Vec<Lifecycle> {
        if !self.drain_lifecycle {
            return Vec::new();
        }
        koja_runtime::signals::drain()
            .into_iter()
            .filter_map(lifecycle_from_index)
            .collect()
    }
}

/// Maps a drained signal's variant index (SIGTERM=0, SIGINT=1, SIGHUP=2)
/// to its [`Lifecycle`] event, matching the runtime's wire encoding.
fn lifecycle_from_index(index: i64) -> Option<Lifecycle> {
    match index {
        0 => Some(Lifecycle::Shutdown),
        1 => Some(Lifecycle::Interrupt),
        2 => Some(Lifecycle::Reload),
        _ => None,
    }
}

/// Drive `future` to completion on the current thread, parking between
/// polls. The synchronous entry seams ([`crate::interpreter`]'s
/// `run_function` / `run_script`) use this to run a non-process body
/// (which never parks) to its value. The driver loop polls process
/// futures directly rather than through here.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let waker = std::task::Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => thread::park(),
        }
    }
}

/// A waker that unparks the thread blocked in [`block_on`].
struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}
