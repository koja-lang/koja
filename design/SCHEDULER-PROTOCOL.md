# Scheduler Protocol

Koja defines scheduling once in a platform-neutral runtime core. Platform
adapters provide execution, readiness, clocks, signals, and the outer run loop.
The native LLVM runtime and the cooperative interpreter both implement this
protocol and expose the same language-level process semantics. Their execution
and ready-queue policies are not structurally identical.

This document is the runtime architecture contract. Process reliability
semantics build on it in [SUPERVISION.md](SUPERVISION.md), and binary interfaces
are specified in [ABI.md](ABI.md).

## Crate boundary

`koja-runtime-core` owns policy.

- process identity and generations
- legal lifecycle transitions
- per-process mailboxes
- calls and reply correlation
- timers and deadlines
- monitors, parenting, and kill cascades
- graceful drain
- invariant counters and scheduler traces

`koja-runtime-posix` owns native capabilities.

- assembly context switching
- process stacks and trampolines
- worker threads and work-stealing queues
- kqueue or epoll readiness through `polling`
- POSIX signals
- the C ABI linked into native binaries

`koja-ir-eval` owns interpreter capabilities.

- interpreter continuation state
- typed runtime values
- a single-threaded cooperative driver
- interpreter I/O integration

The interpreter uses this process protocol while a cooperative runtime is
installed. Plain function or script evaluation outside process mode may block
the interpreter thread directly for I/O.

Core does not import either adapter. Adapters depend on core and implement its
protocol traits.

## Capability traits

The runtime protocol is expressed through `Executor`, `Reactor`, `Clock`,
`SignalSource`, `Driver`, `Message`, and `MessageSource`.

`MessageSource` is required by the shared cooperative driver so it can
construct lifecycle and I/O messages. The native adapter builds those envelopes
inside its signal and reactor paths and does not implement `MessageSource`.

### Executor

`Executor` owns backend-specific process execution state. Core stores that
state opaquely and asks the executor to resume one process until it suspends,
yields, or exits.

The native executor switches between a worker stack and a process stack. The
eval executor re-enters interpreter continuation state. Core observes the
result through the process lifecycle word rather than a backend-specific yield
reason.

### Reactor

`Reactor` translates file-descriptor readiness into typed wakers. A waker
either resumes a process waiting on I/O or delivers an `IOReady` message to a
watcher.

The native adapter polls on a dedicated thread. A cooperative adapter may poll
inline when no process is runnable. Both apply the same core wake and delivery
operations.

### Clock and signals

`Clock` supplies time for deadlines, timers, and drain grace. `SignalSource`
translates platform lifecycle events into Koja lifecycle values. An adapter
without POSIX signals may provide a no-op or host-specific source without
changing language contracts.

### Driver

`Driver` owns the run loop and synchronization strategy.

The native driver uses worker-local priority deques, global injectors, stealing,
and idle parking. The cooperative driver uses one ready queue on one thread.
Both repeatedly apply the same core operations for claim, resume, suspension,
timer promotion, delivery, and exit. They share priority levels and reduction
budgets, but not every queue-selection rule.

## Process lifecycle

Each process slot has a generation and one lifecycle word containing state and
the execution claim. Generational PIDs prevent a stale handle from targeting a
new process that reused the slot.

The internal lifecycle states are:

- `Created`
- `Runnable`
- `Running`
- `Blocked`
- `WaitingIO`
- `Dead`

Every lifecycle change goes through a generation-aware atomic edge. Debug
builds assert that the edge is legal, while release counters cover selected
scheduler chokepoints. Only one worker may claim a process at a time. Death is
a tombstone. Once a process is marked dead, no park operation may make it
runnable or blocked again.

The process table stores hot messaging state under the slot lock. Cold
cross-process relationships such as monitors and the child index use the
registry lock. Timer bookkeeping and adapter ready queues remain outside the
process table.

Core uses a flat slot and registry hierarchy. Death records off-process
work under its owning lock and applies delivery, cascades, and reclamation
after releasing that lock.

`TimerService` has no internal lock. The native adapter serializes it with a
mutex, while the cooperative adapter uses single-threaded interior mutability.
Adapters must not call into the process table while holding their timer
storage.

## Release before suspend

A process cannot hold a Rust borrow or runtime lock across a suspension point.
Receive, call, and I/O blocking follow one sequence.

1. Record the wait target and state under the slot lock.
2. Publish the blocked state before leaving runtime code.
3. Release the lock.
4. Suspend through the adapter's executor.
5. Reacquire state only after the process resumes.

Delivery and parking inspect messaging state under the same slot lock. A
message therefore lands either before the receiver's final mailbox check or
after the blocked state becomes visible. There is no lost-wakeup window.

## Ready queues and fairness

Core returns wake facts. The active driver decides where to enqueue them.
Native sends, replies, and spawns prefer the current worker's deque so
communicating processes remain on one core. External wakes use the global
injectors. Idle workers steal from siblings.

Both drivers select among high, normal, and low priority levels and grant the
same reduction budgets. Queue fairness differs by adapter.

- The cooperative `ReadyQueue` ages nonempty lower levels and forces service
  after a starvation threshold.
- The native driver uses strict priority ordering across worker-local deques,
  injectors, and steals. It has no equivalent aging layer.

Compiler-inserted yield checks requeue a process when its budget is exhausted.
Checks occur at loop back-edges, tail calls, and entries of call-containing
functions. This is cooperative preemption. A long foreign call or another
region that does not reach a check can occupy a native worker beyond one
nominal quantum.

## Mailboxes

Every process owns:

- a system queue for lifecycle traffic
- a business queue for casts, calls, timers, I/O, and exit signals
- one reply slot for the current synchronous call

`Mailbox::pop_received` selects system traffic before business traffic, so an
already-running receive loop cannot leave shutdown behind a business backlog.
The process must still reach `receive` and handle lifecycle traffic. Custom run
logic and unpreemptible foreign calls can delay that point.

Business queues preserve insertion order. Concurrent senders have no stronger
global ordering guarantee. Replies never enter `receive` and are accepted only
when their correlation token matches the in-flight call.

Mailboxes are intentionally unbounded. The runtime does not suspend senders or
reject business traffic at a capacity boundary. The observability contract is
tracked in [ROADMAP.md](ROADMAP.md).

## Timers and deadlines

`TimerService` owns the bookkeeping for delayed messages, receive deadlines,
call deadlines, and drain grace. Entries use tokens so completed waits can
cancel their deadlines eagerly.

The adapter owns synchronization around that service. It drains due entries,
ends the timer hold or borrow, then applies ordinary core actions such as
message delivery and deadline promotion. Timer and process-table access are not
nested.

## Message representation

Core is generic over `Message`.

- Native execution carries owned byte envelopes.
- Eval execution carries typed interpreter values.

Tags route lifecycle, business, reply, I/O, and exit traffic identically.
Ownership differs by representation, but discard and delivery outcomes remain
the same. Native reply envelopes carry `Message::reply_token`. Eval also tracks
reply metadata in its typed call path. Both validate against the core
`awaiting_reply` state before delivery.

## C ABI

Native emitted code calls the current `koja_rt_*` C ABI hosted by
`koja-runtime-posix`. The wrapper layer translates those calls into core
operations and adapter capabilities.

LLVM codegen does not import runtime Rust types. Both sides conform to the
language-neutral contracts in [ABI.md](ABI.md).

## Portability

The protocol does not promise a WASI or browser backend. It preserves the
invariants required to investigate one without redesigning process semantics.
The upstream-gated WebAssembly strategy lives in
[ROADMAP.md](ROADMAP.md#portability-and-webassembly).

A future adapter must provide compatible suspension, timers, I/O wakeup,
cooperative preemption or an equivalent fairness mechanism, and continuation
resumption before becoming a supported target. OS-level preemption is not part
of the current native contract.

## Verification

Default automated coverage includes:

- debug assertions for legal lifecycle edges
- release-build invariant counters at scheduler chokepoints
- generation, stale-handle, mailbox, timer, and process-table unit tests
- native kill, monitor, spawn, scheduler, and reactor integration tests
- dual-backend language fixtures for process-visible behavior
- native live-block fixtures for selected payload-reclamation paths

The violation counter is an oracle, not an exhaustive proof that every
interleaving was explored. Timer and I/O behavior has language and integration
coverage, but not every combination has a dedicated race harness.

Optional diagnostics and stress tools include:

- `KOJA_SCHED_TRACE=1` for the lifecycle event ring
- `just tsan` for a reduced native scheduler stress configuration with known
  suppressions

Long-running process-churn and HTTP tests under `benchmarks/soak/` are manual
endurance evidence. They are not default CI gates.

The original Phase 5 milestone is preserved in
[archive/20260722-ROADMAP.md](archive/20260722-ROADMAP.md). The superseded eval
execution plan is preserved in
[archive/20260612-EVAL-PROCESS.md](archive/20260612-EVAL-PROCESS.md).
