# Scheduler Protocol

The destination design for the Koja runtime as a **formal protocol**
rather than a single hard-coded scheduler. This is the ROADMAP Phase 5
A1 deliverable: define the interface first, make the existing
multi-threaded native scheduler the _first implementation_ of it, and
shape the interface so a **single-threaded cooperative** backend (the
Phase 6 WASM prerequisite, and the path to `koja-ir-eval` process
parity) can be the _second implementation_ without changing the core or
any user code.

This is a destination doc, not a trajectory. Every claim reduces to a
mechanical check, a trait the native runtime already satisfies, or a
behavior pinned by an existing test.

## Scope

This doc covers the **scheduler protocol only**. In scope: the trait
surface, the agnostic-core / platform-adapter seam, the suspension
model, and the native-side conformance refactor. Out of scope (named in
[Non-goals](#non-goals)): building the cooperative executor for
`koja-ir-eval`, the WASI reactor, monitors / supervision (A0/A2),
preemption, and work-stealing. The protocol must _accommodate_ those;
this effort does not _build_ them.

## Why a protocol

Today the runtime is one concrete scheduler. The _scheduling policy_ is
already platform-agnostic — `ProcessTable`
([`process_table.rs`](../crates/koja-runtime-core/src/process_table.rs)),
`Mailbox` ([`mailbox.rs`](../crates/koja-runtime-core/src/mailbox.rs)), and
the wire envelope ([`wire.rs`](../crates/koja-runtime-core/src/wire.rs))
depend only on `Instant`, raw pointers, and an allocator. But that
policy is reachable only through machinery welded to one platform:

- `SCHED: Mutex<ProcessTable>` + `WORK_AVAILABLE: Condvar` — assumes N
  OS threads contend for the table
  ([`scheduler.rs`](../crates/koja-runtime-posix/src/scheduler.rs)).
- `CURRENT_PID` / `SCHED_SP` / `YIELD_SP` thread-locals — assume one
  process per OS worker.
- `koja_context_switch` (hand-written asm) + `mmap` stacks — assume
  stackful coroutines.
- the `polling`-crate reactor thread
  ([`reactor.rs`](../crates/koja-runtime-posix/src/reactor.rs)) — assumes
  kqueue/epoll.

None of these exist on `wasm32-wasi` (no threads by default, no asm
stack switch, no kqueue). And `koja-ir-eval` can't use any of them
either — it is a synchronous tree-walker, which is why every process
feature there returns `RuntimeError::Unsupported` today. A protocol that
both the native runtime and a single-threaded cooperative backend
satisfy is the one design that unblocks WASM _and_ eval parity at once.

## The seam

Two layers. The **agnostic core** owns scheduling _decisions_; the
**platform adapter** owns _capabilities_ (how a process runs, how I/O
readiness arrives, how time and signals are observed, how the loop is
driven and synchronized).

```mermaid
flowchart TB
  subgraph core [koja-runtime-core: agnostic, no libc/polling/asm/threads]
    PT["ProcessTable&lt;E, M&gt;: slotmap, ready queue, timer/deadline heaps"]
    MB["Mailbox&lt;M&gt;: system/business queues + reply slot"]
    OPS["generic ops: receive/send/spawn/reply/call/send_after logic"]
    TR["protocol traits: Executor, Reactor, Clock, SignalSource, Driver"]
  end
  subgraph native [koja-runtime-posix: POSIX adapter = impl 1]
    ASM["Executor: asm context switch + mmap stacks"]
    PLL["Reactor: polling kqueue/epoll"]
    DRV["Driver: N worker threads + Mutex/Condvar"]
    SIG["SignalSource: sigaction"]
    EXT["extern C koja_rt_* wrappers + global accessor"]
  end
  subgraph coop [future adapter = impl 2: eval / WASI]
    INT["Executor: interpreter-driven, no stack switch"]
    POLLW["Reactor: poll_oneoff / blocking"]
    LOOP["Driver: single-threaded ready-queue loop, no lock"]
  end
  core --> native
  core --> coop
  llvm["koja-ir-llvm (unchanged)"] -->|C ABI| EXT
```

The seam is the commitment: **scheduling decisions live in core exactly
once; platforms supply capabilities.** A new backend implements the
traits and inherits ready-queue order, mailbox priority, deadline
semantics, kill-tombstone discipline, and counter oracles for free.

## What is agnostic vs platform

The current `Process` struct conflates both layers; splitting it is the
central structural move.

| Concern                                                                               | Layer    | Today                                       | Destination                                         |
| ------------------------------------------------------------------------------------- | -------- | ------------------------------------------- | --------------------------------------------------- |
| `state`, `waiting`, `deadline`, `on_cpu`                                              | agnostic | `Process` fields                            | `ProcessControlBlock` in core                       |
| ready queue, slotmap, generations, timer/deadline heaps, transitions, counters, trace | agnostic | `ProcessTable`                              | `ProcessTable<X, M>` in core (`X` = `E::Execution`) |
| mailbox routing (system/business/reply, priority, displacement, wait targets)         | agnostic | `Mailbox`                                   | `Mailbox<M>` in core (generic over message repr)    |
| `func`, `init_state`, `sp`, `stack`, `tsan_fiber`                                     | platform | `Process` fields                            | `E::Execution` owned by the executor                |
| message representation                                                                | platform | byte `Envelope`                             | `M`: native bytes vs coop `Value`                   |
| process activation / suspension                                                       | platform | `koja_context_switch`, `process_trampoline` | `Executor` trait                                    |
| fd readiness                                                                          | platform | `reactor.rs` (`polling`)                    | `Reactor` trait                                     |
| run loop + synchronization                                                            | platform | `worker_loop`, `SCHED` Mutex, `Condvar`     | `Driver` trait                                      |
| clock                                                                                 | platform | `Instant::now()` inline                     | `Clock` trait                                       |
| OS signals                                                                            | platform | `signals.rs` (`sigaction`)                  | `SignalSource` trait                                |
| allocator                                                                             | shared   | `memory.rs` (libc passthrough)              | stays libc passthrough; see [Allocator](#allocator) |

## Glossary

- **Agnostic core.** `koja-runtime-core`: the `ProcessTable`, `Mailbox`,
  wire/message types, scheduling-policy code, generic runtime
  operations, and the protocol traits. No `libc`, no `polling`, no asm,
  no `std::thread`.
- **Platform adapter.** A crate implementing the protocol traits for one
  target. `koja-runtime-posix` is the first; `koja-runtime-wasi` and a
  cooperative eval adapter come later.
- **`ProcessControlBlock` (PCB).** The agnostic per-process record:
  lifecycle `state`, `waiting` target, optional `deadline`, `on_cpu`
  claim flag. Holds an `E::Execution` for the executor's private
  execution state — a PCB is precisely the structure that owns a
  process's saved execution, so the core stores it without inspecting it.
- **Execution state (`E::Execution`).** Everything an executor needs to
  run and resume one process. Native: entry fn, config payload, saved
  `sp`, stack mapping, TSan fiber. Cooperative: entry reference, config
  value, and resumption state.
- **Continuation (`E::Continuation`).** The small `Copy` resume token the
  driver marshals in and out of the PCB around a `resume` — a projection
  of the execution state (native: the saved `sp`). It crosses the lock
  boundary by value so no `&mut Execution` is held across a switch.
- **Yield.** The act of a running process handing control back to the
  driver at a suspension point (`receive`, `io_block`). The driver reads
  _why_ from the PCB's lifecycle `state`, the authoritative record it
  must consult anyway to handle a concurrent kill or wake.
- **Suspension point.** A site in a process body that may yield:
  `koja_rt_receive[_timeout]`, `koja_rt_call_receive`, and `io_block`.
  The **release-before-suspend invariant** governs all of them.

## Capability traits

Sketches, not frozen signatures — the spike refines them. They show the
shape and the obligations.

### `Executor`

Owns how a process is entered, suspended, and resumed. This is the trait
that abstracts stackful-vs-cooperative.

```rust
pub trait Executor {
    /// Per-process execution state the core stores opaquely in the PCB.
    type Execution;
    /// The Copy resume token the driver marshals in/out of the PCB around
    /// a resume — a projection of Execution (native: the saved `sp`).
    type Continuation: Copy;
    /// Message payload representation carried in this executor's mailbox.
    type Message: Message;

    /// Enter or continue `pid` from `continuation`, run it until it
    /// yields, and return the token to resume it next time. Called by the
    /// Driver with the core lock/borrow *released*.
    fn resume(&self, pid: Pid, continuation: Self::Continuation) -> Self::Continuation;
}
```

`resume` deliberately trades only the `Copy` `Continuation`, not
`&mut Execution`: the native switch releases the core lock across the
context switch, and the running process reads its own execution state
mid-switch (the trampoline re-locks to read entry fn + config), so a
borrow can't span the suspension point. The driver reads the prior token
out of the PCB under the lock, drops the lock, calls `resume`, stores the
returned token back, and reads the PCB's lifecycle `state` to decide what
to do next — there is no separate yield-reason channel, because a
concurrent kill or wake means the PCB is the only trustworthy source.

Construction is **not** a trait method: each backend builds its own
`Execution` from its own spawn entry point (native: `koja_rt_spawn` maps
a stack and copies the config; cooperative: the eval driver's spawn
builtin), so there is no generic caller for a `create`.

Native `resume` does a `koja_context_switch` into the process stack and
returns the saved `sp`. Cooperative `resume` calls the interpreter to run
the process until it reaches a suspension point and returns its
resumption token — no stack switch.

The suspension primitive itself (what `koja_rt_receive` calls to give up
control) is the executor's inverse of `resume`. Native: switch back to
the worker's scheduler stack. Cooperative: return up the interpreter
call stack to the driver. Both obey the same invariant below.

### `Reactor`

Abstracts fd readiness. Unifies the two existing modes — `io_block`
(promote a `WaitingIO` waiter) and `Fd.watch` (deliver an `IOReady`
message) — behind one waker vocabulary, which also resolves
[RUNTIME-GAPS.md](RUNTIME-GAPS.md) gap #2 (the two keyspaces multiplexed
into one integer).

```rust
pub trait Reactor {
    fn register(&self, fd: Fd, interest: Interest, waker: Waker);
    fn deregister(&self, fd: Fd);
    /// Drive one readiness pass; return the wakers whose fds fired.
    fn poll(&self, timeout: Option<Duration>) -> Vec<Waker>;
}

pub enum Waker {
    Resume(Pid),                                  // io_block: WaitingIO -> Runnable
    Deliver { fd: Fd, pid: Pid, readiness: Readiness }, // watch: enqueue an IOReady
}

pub enum Readiness { Error, Readable, Writable } // the fired direction
```

The waker is registered as the _action_ to take; `poll` returns it with
the `Deliver` `readiness` filled in from the event that fired (the
`IOReady` variant a watcher observes). The poller tracks one registration
per fd, so the native reactor stores a single `fd -> Waker` map — the last
`register` wins, matching the poller's own semantics, which is why the two
old keyspaces collapse into one.

Native drives `poll` on a dedicated thread (`polling` crate) and applies
the returned wakers (promote under `SCHED`, then send `IOReady`s after).
A cooperative driver calls `poll` inline when the ready queue empties
(WASI `poll_oneoff`; eval may block the single thread on the syscall and
skip the reactor entirely for the blocking path).

### `Clock`, `SignalSource`

Leaf services.

```rust
pub trait Clock { fn now(&self) -> Instant; }

pub trait SignalSource {
    fn install(&self);
    fn drain(&self) -> Vec<Lifecycle>; // Shutdown / Interrupt / Reload
}
```

`signals.rs` already drains into `Lifecycle` variant indices and is
documented as shared between the LLVM runtime and eval; it becomes the
POSIX `SignalSource`. WASM has no POSIX signals — its `SignalSource` is
a no-op or host-specific.

### `Driver`

Owns the run loop and _all_ synchronization. This is where
multi-threaded and single-threaded diverge most.

```rust
pub trait Driver {
    /// Boot the runtime and run until the entry process dies. Replaces
    /// `koja_rt_main_done`.
    fn run(self, core: Core<Self::Executor>);
}
```

- **Native `Driver`:** wraps the core in `Mutex` + `Condvar`, spawns
  `worker_count()` worker threads plus a reactor thread, parks idle
  workers, sets `SHUTDOWN` when PID 1 dies, joins.
- **Cooperative `Driver`:** owns the core with no lock (single thread).
  Loop: drain due timers/deadlines and signals; `claim_next`; `resume`;
  `after_switch`; when the ready queue empties, `reactor.poll` (or
  block) for the nearest wakeup; exit when the entry dies.

The agnostic core exposes `claim_next`, `after_switch`, `deliver`,
`promote_due_deadlines`, `take_due_timers` as plain `&mut self`
operations. **The core contains no locking.** Whether those calls happen
under a `Mutex` (native) or a bare `&mut` borrow (cooperative) is the
Driver's choice.

## Generic runtime operations

The `koja_rt_*` _logic_ (peek mailbox, park if empty, yield, re-peek;
token correlation for `call`; envelope routing for `send`) is
platform-agnostic and lives in core **once**, generic over the traits:

```rust
pub fn receive<E: Executor>(rt: &Runtime<E>, out: *mut u8, cap: i64) -> i64;
pub fn send<E: Executor>(rt: &Runtime<E>, pid: Pid, msg: E::Message);
pub fn spawn<E: Executor>(rt: &Runtime<E>, entry: EntryPoint, config: Config<E>) -> Pid;
// ... reply, call_receive, send_after, kill, is_alive, self_pid
```

Each adapter provides only the thin `#[no_mangle] extern "C"` wrappers
and a global accessor:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_receive(out: *mut u8, cap: i64) -> i64 {
    koja_runtime_core::receive(global_runtime(), out, cap)
}
```

Per-adapter wrappers (≈25 one-liners) rather than `dyn`-dispatched
symbols in core: they stay zero-cost, and Phase 6's "FFI resolves to
WASM imports rather than linker symbols" means the wrapper layer is
exactly where target divergence belongs.

## The suspension model (the crux)

The one decision the spike must nail. A suspension point must give up
control _without_ assuming OS threads or asm stack switching.

Options considered:

1. **OS thread per process** (the archived
   [20260612-EVAL-PROCESS.md](archive/20260612-EVAL-PROCESS.md) plan):
   `receive` blocks a channel. Rejected by the A1 WASM constraint — WASM
   has no threads, and this caps at hundreds of processes.
2. **Stackful coroutines everywhere** (asm context switch): native
   already does this; WASM can't (no portable stack switch without
   Asyncify, which is a toolchain feature, not a runtime one).
3. **Executor-owned suspension** (chosen): the _core_ defines suspension
   abstractly as "executor yields control back to the driver, leaving the
   reason in the PCB"; each executor implements it in its native idiom.

**Decision: option 3.** The `Executor` owns activation and suspension;
the core never names a stack or a thread. Native implements suspension
as a context switch; a cooperative interpreter implements it as
returning up its own Rust call stack to the driver loop. WASM later
implements it via whatever the chosen flavor supports (Asyncify or an
explicit interpreter state machine). The core is identical across all
three.

### The release-before-suspend invariant

The load-bearing rule that makes one set of scheduling code correct on
both a `Mutex`-guarded native core and a single-borrow cooperative core:

> A suspension point must **release its access to the core** (drop the
> `Mutex` guard / end the `&mut` borrow) _before_ yielding, and
> re-acquire it _after_ resuming.

Native already obeys this — `koja_rt_receive` drops the `SCHED` guard
before `yield_to_scheduler`, and `io_block` drops it before
`koja_context_switch`. Stated as a protocol invariant, the same code is
correct cooperatively: the single-threaded driver can reuse the core
the moment the suspending process returns to it, with no aliasing.

## Message representation

`Mailbox<M>` and the routing logic (system drained before business,
reply slot one-shot with displacement, `WaitTarget` partitioning) are
generic over the message type `M: Message`, where:

```rust
pub trait Message {
    fn tag(&self) -> Tag;            // Business / Lifecycle / IOReady / Reply
    fn reply_token(&self) -> i64;
}
```

- **Native `M = Envelope`:** byte transport buffer, exactly as today,
  preserving the `wire.rs` ABI that `koja-ir-llvm` emits against.
- **Cooperative `M = Value`** (future): a typed interpreter value with
  the same tag bits, skipping byte (de)serialization — the on-the-wire
  format is observable to nobody but the runtime.

The _priority and lifecycle semantics_ — the part that must stay
identical for observable parity — live in core and are shared.

## Allocator

Allocation is **not a scheduler-protocol concern.** It is a distinct,
orthogonal seam, and it is called out here only to fix that boundary
explicitly — no allocator trait joins the five capability traits above.

The distinction is categorical. The scheduler protocol is a _policy_
interface with genuinely different implementations (multi-threaded,
cooperative, WASI). Allocation is a _provider_ interface — one correct
behavior (`alloc` / `realloc` / `free`), swapped only to change the
backing allocator. Its eventual formalization is therefore most likely
"conform to Rust's `GlobalAlloc` on the Rust side, plus the existing
`koja_alloc` / `koja_realloc` / `koja_free` C-ABI symbol contract on the
codegen side" — not a bespoke Koja trait.

That seam already half-exists.
[`memory.rs`](../crates/koja-runtime-core/src/memory.rs) is not "the
allocator" — it is the _single current implementation_ of an
already-stable contract: codegen emits calls to `koja_alloc` /
`koja_free`, and `memory.rs` is the libc-backed conformer behind those
symbols (with a load-bearing C-interop passthrough equivalence — see its
module doc). The C-ABI symbols are the protocol; the module is an impl.

For this spike and all of Phase 6's primary path, the libc passthrough
stays a **shared module in core**, behind its existing C-ABI contract.
Every planned tier-1 target has a working `malloc`: POSIX (system libc)
and `wasm32-wasip1` (`wasi-libc`); the `libc` crate links on both. The
only genuinely libc-less target is bare-browser `wasm32-unknown-unknown`
— post-1.0 per [ROADMAP.md](ROADMAP.md), and even then resolvable by
_providing_ `malloc` / `free` at link time (link `wasi-libc`, or export
a `dlmalloc` shim) rather than threading an allocator through every core
type. The real forcing function for a second implementation is an arena
/ GC allocator, which `memory.rs`'s own module doc already anticipates —
not browser WASM. Until such an implementation exists, formalizing the
seam buys nothing.

## Worked example: single-threaded cooperative conformance

How a future eval/WASI adapter satisfies the protocol — the proof the
seam is real. (Not built in this spike; included so the trait surface is
validated against its hardest consumer.)

- **`Executor::Execution`** = `{ entry: FnRef, config: Value, resume:
ResumeState }`, with `Continuation` = the resume token (an interpreter
  state handle; possibly `()` if the state lives in `Execution`).
  `resume(pid, continuation)` re-enters the interpreter at the saved
  point, runs until the body hits `receive`/`io_block`, parks via the
  core (recording the reason in the PCB), and returns the next token. No
  stack, no asm.
- **`Driver::run`** = single loop, no `Mutex`: `promote_due_deadlines`,
  `take_due_timers`, `signals.drain`, `claim_next`, `resume`,
  `after_switch`; when `ready` is empty, `reactor.poll(nearest_wakeup)`
  or block; exit on entry death. Identical control flow to
  `worker_loop`, minus threads and locking.
- **`receive`** runs the _core_ `receive` op: peek the mailbox (core),
  park via `try_park` (core), end the borrow, return to the driver
  (executor suspension), and on resume re-peek (core). The borrow
  discipline is the same statements native runs under the lock.
- **`Reactor`** = either WASI `poll_oneoff` driving `Waker::Resume`, or
  (eval expedient) block the single thread on the syscall and never park
  — both legal under the trait.
- **`SignalSource`** = POSIX `sigaction` (eval) or no-op (WASM).

If this adapter compiles against the core with no changes to the core,
the protocol is correct.

## Native conformance (impl #1)

The proof the protocol describes a _real_ scheduler, not an aspiration:
refactor `koja-runtime` to implement the traits with zero behavior
change.

| Trait          | Native implementation                                                                       | Source today                                    |
| -------------- | ------------------------------------------------------------------------------------------- | ----------------------------------------------- |
| `Executor`     | asm `koja_context_switch`, `mmap` stacks, `process_trampoline`, TSan fibers                 | `scheduler.rs`, `ffi.rs`, `arch/*.s`, `tsan.rs` |
| `Reactor`      | `polling` kqueue/epoll on a dedicated thread                                                | `reactor.rs`                                    |
| `Driver`       | N worker threads, `Mutex<ProcessTable>`, `Condvar`, `SHUTDOWN`, cgroup-aware `worker_count` | `scheduler.rs`                                  |
| `Clock`        | `Instant::now()`                                                                            | inline                                          |
| `SignalSource` | `sigaction` latch + drain                                                                   | `signals.rs`                                    |
| message `M`    | `Envelope` (byte wire)                                                                      | `wire.rs`                                       |

The race/leak oracles (`koja_rt_sched_violations`, `koja_rt_live_blocks`,
`tests/lang/memory/`) and `just tsan` are the guardrails: behavior must
not move.

## Crate structure and the unchanged C ABI

- **New `koja-runtime-core`** (`rlib`): agnostic core + trait defs. No
  `polling`, no asm, no `std::thread`; `libc` only for the allocator.
- **`koja-runtime-posix` (the POSIX adapter, `staticlib`).** Renamed
  from `koja-runtime`; depends on `koja-runtime-core`, implements the
  traits, and hosts the `extern "C" koja_rt_*` wrappers. Its `[lib]
name` is pinned to `koja_runtime`, so `libkoja_runtime.a`, the
  `-lkoja_runtime` link flag, and the embedded-archive bytes in
  [`koja-driver/src/link.rs`](../crates/koja-driver/src/link.rs) are
  **untouched**, and `koja-ir-llvm` needs **zero** code changes.

That `koja-ir-llvm` compiles and passes against an unchanged C ABI is
the integration proof that the shim is a clean seam.

## What moves where

| File                                                                                                               | Destination                                                    |
| ------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------- |
| `process_table.rs`, `mailbox.rs`, `wire.rs`, `scheduler_trace.rs`                                                  | core (generic over `E`, `M`)                                   |
| scheduling-policy + generic `koja_rt_*` logic (extracted from `scheduler.rs`)                                      | core                                                           |
| trait defs (`Executor`, `Reactor`, `Clock`, `SignalSource`, `Driver`, `Message`)                                   | core (new)                                                     |
| `memory.rs`                                                                                                        | core (shared libc passthrough)                                 |
| asm stacks, `process_trampoline`, `worker_loop`, `koja_rt_main_done`, Mutex/Condvar, thread-locals, `worker_count` | `koja-runtime-posix` (native `Executor` + `Driver`)            |
| `reactor.rs`                                                                                                       | `koja-runtime-posix` (native `Reactor`)                        |
| `signals.rs`                                                                                                       | `koja-runtime-posix` (native `SignalSource`)                   |
| `tsan.rs`, `ffi.rs`, `arch/*.s`                                                                                    | `koja-runtime-posix` (native executor detail)                  |
| `extern "C" koja_rt_*` wrappers + global accessor                                                                  | `koja-runtime-posix`                                           |
| `fs.rs`, `socket.rs`, `system.rs`, `intrinsics/`, `string`, `format`, `util`, `parse_text`                         | `koja-runtime-posix` (POSIX externs; not scheduler, untouched) |

## Mechanical checks

- `koja-runtime-core` imports neither `polling` nor any asm. Grep:
  `rg "use polling" koja/crates/koja-runtime-core/` returns nothing;
  `koja-runtime-core/Cargo.toml` has no `polling` dependency.
- `koja-runtime-core` does not spawn OS threads. Grep:
  `rg "std::thread|thread::spawn" koja/crates/koja-runtime-core/`
  returns nothing.
- The scheduling policy is not duplicated: `claim_next`,
  `promote_due_deadlines`, mailbox priority, and the legal-transition
  table exist only in `koja-runtime-core`.
- `koja-ir-llvm` is unchanged: no diff under `koja/crates/koja-ir-llvm/`,
  and `koja-driver/src/link.rs` still links `-lkoja_runtime`.
- The `koja_rt_*` C ABI symbol set is unchanged (same names, same
  signatures). Diff the `#[no_mangle]` symbol list before/after.
- Native behavior unchanged: `koja_rt_sched_violations` stays zero,
  `tests/lang/memory/` leak deltas stay zero, `just tsan` reports no
  races, `tests/lang/process_*` pass.

## Non-goals

- **The cooperative executor / eval process parity.** This spike defines
  the protocol and proves it with the native impl; building the eval
  `Executor`/`Driver` (spawn, mailboxes, `Ref`/`ReplyTo`, business
  `receive`) is the follow-on.
- **The WASI adapter** (`poll_oneoff` reactor, WASM FFI). Phase 6.
- **Monitors / supervision** (A0/A2), **preemption + priority**, and
  **work-stealing run queues**. The protocol must not preclude them; it
  does not add them here.
- **An allocation protocol.** Allocation is a separate, already-latent
  seam (the `koja_alloc` / `koja_free` C-ABI contract), not a sixth
  scheduler trait. See [Allocator](#allocator). It stays a shared core
  module until a second implementation (arena / GC, or a libc-less
  target) actually forces the issue.
- **Behavior changes.** Native observable semantics are frozen by the
  oracles above; this is a refactor, not a redesign.

## References

- [ROADMAP.md](ROADMAP.md) — Phase 5 A1 (scheduler protocol) and Phase 6
  (WASM runtime split into `koja-runtime-core` + per-target adapters).
- [COMPILER-NORTHSTAR.md](COMPILER-NORTHSTAR.md) — backends as siblings
  over a sealed `IRProgram`; this protocol is the runtime analogue.
- [RUNTIME-GAPS.md](RUNTIME-GAPS.md) — gap #2 (typed `EventKey`) folds
  into the `Reactor`/`Waker` vocabulary; gap #3 (wake `WaitingIO` on
  message) is a `Reactor`/core policy question surfaced by the seam.
- [ABI.md](ABI.md) — the `koja_rt_*` and `wire.rs` contract this spike
  preserves verbatim.
- [archive/20260612-EVAL-PROCESS.md](archive/20260612-EVAL-PROCESS.md) —
  the superseded thread-per-process eval plan; its observable-parity
  test strategy and typed-`Value`-mailbox rationale are inherited here.
- [crates/koja-runtime-posix/src/scheduler.rs](../crates/koja-runtime-posix/src/scheduler.rs)
  — the native reference implementation; when observable semantics are
  in doubt, the native runtime is the spec.
