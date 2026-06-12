# Eval Process Scheduler

> **Archived 2026-06-12 — superseded by the Phase 5 A1 scheduler
> protocol.** ROADMAP.md commits to defining the runtime as a formal
> protocol interface before building further backends: the native
> scheduler is the first implementation, the eval scheduler becomes
> the second, and the protocol must be expressible by a
> single-threaded *cooperative* backend (the Phase 6 WASM
> prerequisite). That constraint invalidates this doc's central
> thread-per-process design choice — a coop single-threaded eval
> scheduler would double as the cheapest testbed for the WASM shape.
> The unbuilt remainder of this doc (spawn, mailboxes, `Ref`
> messaging, monitors/supervision, `koja shell` project mode) gets
> re-planned under that protocol effort rather than implemented as
> designed here.
>
> **What shipped before archival** (June 2026, the entry-process
> slice): `koja run` (interpreter default) executes a project's
> `Process` entry in-process — argv-shaped `List<String>` config,
> blocking socket/TLS externs (eval-native, no reactor),
> `IRInstruction::Receive` over OS-signal-delivered `Lifecycle`
> events plus `after` timeouts, and `IRInstruction::BinaryMatch`.
> Parity is pinned by the `*_interpreted` tests in
> `koja-driver/tests/lang_suite.rs` and the `koja-ir-eval`
> integration tests. Everything else surfaces
> `RuntimeError::Unsupported` with a `--backend=llvm` hint.
>
> **Worth inheriting** when the scheduler-protocol doc is written:
> the observable-parity test strategy (runtime-is-the-spec, Rust
> tests pinning eval to LLVM behavior), the typed-`Value`-in-mailbox
> rationale, the lifecycle-priority semantics, and the
> mechanical-checks list at the bottom.

Design for a real process scheduler inside `koja-ir-eval` so the
interpreter implements `spawn` / `receive` / mailboxes / supervision
the same way the LLVM-emitted runtime does. Closes the last
intentional parity gap between the two backends and unlocks
`koja shell` running a project as if it were a compiled binary.

This is a destination doc, not a trajectory. Every claim reduces
to a behavior the LLVM runtime already exhibits, plus a Rust
test that pins the eval-side behavior to it.

## Why

Eval today is feature-complete for synchronous code plus the
entry-process slice above. Anything touching
`IRInstruction::Spawn` or `FunctionKind::SpawnWrapper` returns
`RuntimeError::Unsupported` and bails. That carve-out forces a
sharp split:

- LLVM mode: real Koja program, with processes.
- Eval mode: single entry process, no spawn.

The split shows up in three concrete places we want to close:

1. **`koja shell` of a project.** Today the shell is a synchronous
   REPL — handy for stdlib helpers and pure functions, useless
   for prodding a running actor system. The goal is: open
   `koja shell` inside a project that names a `Process` entry,
   the entry boots, and the REPL talks to it via the same `Ref`
   handles user code uses.
2. **`koja eval` of process-using scripts.** Currently fails
   immediately. Should succeed and produce the same observable
   output as the compiled binary.
3. **Iteration speed.** Tests of actor logic shouldn't need a
   full LLVM compile/link cycle. Eval-mode test runs would close
   the loop in milliseconds instead of seconds.

The non-goal is performance parity: eval will be slower per-message
than the compiled runtime, by some constant factor we don't bound.
What we promise is **observable behavior parity** — same message
ordering rules, same lifecycle semantics, same `Ref` API, same
exit-code semantics.

## Model

| Axis                  | LLVM runtime                                                    | Eval scheduler                                                 |
| --------------------- | --------------------------------------------------------------- | -------------------------------------------------------------- |
| Concurrency primitive | Cooperative coroutines on N worker OS threads                   | One OS thread per Koja process                                 |
| Process body          | Native function pointer dispatched via `koja_rt_spawn`          | `IRFunction` interpreted on the thread                         |
| Mailbox               | `VecDeque<Vec<u8>>` of byte-serialized messages                 | `crossbeam::channel::Sender<Envelope>` carrying typed `Value`  |
| Yield point           | `koja_rt_receive` swaps stacks back to the worker loop          | `recv` blocks the OS thread                                    |
| Lifecycle priority    | Front-of-queue insert via `koja_rt_send_lifecycle`              | Two channels per process: lifecycle (priority) + business      |
| Reactor / I/O         | `polling`-based reactor wakes blocked processes on fd readiness | Reactor unchanged — eval reuses `koja-runtime`'s reactor as-is |
| PID space             | `i64` minted by scheduler under `SCHED.lock()`                  | `i64` minted by eval scheduler under its own `Mutex`           |
| Termination           | `Process` removed from `SCHED.processes`, mailbox freed         | Thread joined, registry entry removed, channels dropped        |

The mapping is structural. Every primitive in the LLVM column has
a direct counterpart in the Eval column with the same observable
contract. Where eval can reuse runtime infrastructure (the
reactor, OS signal handlers) it does. Where the LLVM model
depends on byte-serialized payloads (mailbox, spawn config) eval
substitutes typed `Value` to skip the marshaling round-trip — the
on-the-wire format is observable to nobody but the runtime, so
substituting is safe.

### Why one thread per process

The interpreter is a synchronous `match`-on-instruction loop.
Three options for layering concurrency on top:

- **One OS thread per process.** `Receive` calls `recv` on a
  blocking channel. Interpreter stays sync. Caps at ~hundreds of
  processes per shell session — fine for testing and REPL use.
- **Tokio async, async interpreter.** Every interpreter method
  becomes `async fn`. Requires `Box::pin` / `async-recursion`
  through the entire `match` tree. Heavy refactor for use cases
  that don't need >1000 concurrent processes.
- **Custom green threads matching the runtime's coroutines.**
  Highest fidelity. ~1000 LOC of new scheduler code to mirror
  what `koja-runtime/src/scheduler.rs` already does.

We pick the first. The interpreter doesn't change shape; only the
`Spawn` / `Receive` instruction handlers grow. If we ever need to
scale eval to thousands of concurrent processes (we don't today,
and probably won't — that's what `koja build` is for), the
scheduler can be swapped without touching the interpreter; the
public eval API doesn't expose the threading model.

### Why `Value` in the mailbox

LLVM mailboxes hold `[i8 tag][i8* payload]` byte sequences. The
sender memcpys the message struct into the buffer; the receiver
memcpys it back into a typed local. The byte-level shape is
internal — no Koja-level construct observes it.

Eval has a richer in-process value model (`Value::Struct`,
`Value::Enum`, `Value::String`, etc.) and runs entirely in one
address space. Serializing to bytes and back would burn cycles
to produce the exact same `Value` on the receiving end. The
mailbox carries `Value` directly. The `Envelope` shape carries
the same tag bits the runtime uses (Business / Lifecycle), just
discriminating an enum instead of a leading byte.

```rust
enum EvalEnvelope {
    Business(Value),
    Lifecycle(LifecycleVariant), // Shutdown / Interrupt / Reload
    IOReady { fd: i32, kind: IOReadyKind }, // future fast path; not in initial scope
}
```

## Subsystems

### PID registry

A process global `Mutex<EvalScheduler>` holds the mapping.
Mints `i64` PIDs from a monotonic counter starting at 1 (PID 1
is the entry process, mirroring the runtime). Each entry stores:

```rust
struct EvalProcess {
    business: crossbeam::channel::Sender<Value>,
    lifecycle: crossbeam::channel::Sender<LifecycleVariant>,
    handle: Option<thread::JoinHandle<StopReason>>,
    state: ProcessState, // Created / Running / Dead
}
```

`Ref.self()` reads the current PID from a thread-local set when
the worker thread is constructed. Same shape as the runtime's
`CURRENT_PID` thread-local. `Ref.cast`, `Ref.call`, `Ref.signal`
all do a registry lookup keyed by PID and write into the matching
sender; if the lookup misses (process is dead / never existed)
the operation surfaces `CallError::ProcessDown` (for `call`) or a
silent drop (for `cast` / `signal`), matching the runtime's
behavior.

### Mailbox and receive priority

Two channels per process: business + lifecycle. `Receive` reads
from lifecycle first, falls through to business if empty.
Mirrors `koja_rt_receive`'s priority-queue semantics where
lifecycle messages are inserted at the front. Implementation:

```rust
fn recv(&self) -> EvalEnvelope {
    match self.lifecycle.try_recv() {
        Ok(v) => return EvalEnvelope::Lifecycle(v),
        Err(TryRecvError::Empty) => {}
        Err(TryRecvError::Disconnected) => panic!("..."),
    }
    select! {
        recv(self.lifecycle) -> v => EvalEnvelope::Lifecycle(v?),
        recv(self.business) -> v => EvalEnvelope::Business(v?),
    }
}
```

`Receive` with `after Ns body`: same shape, `select_timeout`
returns `Timeout`, falls through to the `after` arm. The
`after` clause can call `body` directly with no message
binding — same as the runtime.

### Spawn

`IRInstruction::Spawn { config, config_type, wrapper, ref_type }`
in eval:

1. Materialize the config `Value` from the SSA operand. (No
   serialization — pass the `Value` directly into the spawn
   thread closure.)
2. Look up the spawn wrapper `IRFunction` by symbol. The wrapper
   is `FunctionKind::SpawnWrapper { state }`; eval treats it
   structurally (call `state.start(config)` then `state.run()`)
   instead of going through the LLVM-shaped `void(*)(i8*)` thunk.
3. Mint a PID, allocate channels, register in the scheduler.
4. `thread::Builder::new().name(format!("koja-pid-{pid}"))
.spawn(move || run_process(pid, wrapper, config))`.
5. Return `Value::Struct(Ref { id: pid })` matching `ref_type`'s
   layout.

`run_process` is the Rust-side entry of every Koja process:

1. Set the `CURRENT_PID` thread-local.
2. Call the state struct's `start(config)` method. On
   `Result::Err(stop_reason)` — early termination, skip `run`.
3. Call `run(state)`. The default `run` impl drives the
   `receive` loop; user impls drive whatever loop they like.
4. On return, capture the `StopReason`. Update the registry
   entry to `Dead`. Notify any monitors. Drop channels.

### `ReplyTo<R>` and `call`

`Ref.call(self, msg, timeout) -> Result<R, CallError>`:

1. Allocate a `crossbeam::channel::bounded(1)` for the reply.
2. Wrap the user's `msg` plus the reply sender into the union
   shape the receiver expects (`(M, Option<ReplyTo<R>>)` is
   already how `handle` reads it; eval reuses the same passing
   shape — `ReplyTo` is just `(sender, receiver_pid)` at the
   interpreter level).
3. Push the envelope onto the target's business channel.
4. `recv_timeout(timeout)` on the reply receiver.
5. Map result: `Ok(value)` → `Result::Ok(value)`, `Err(Timeout)`
   → `CallError::Timeout`, `Err(Disconnected)` →
   `CallError::ProcessDown`.

`ReplyTo<R>::send` on the receiver side just writes to the bound
sender. Same observable behavior as the runtime's reply path
that goes through a private mailbox slot.

### `Ref.self()`

Thread-local `CURRENT_PID` set when `run_process` starts. The
intrinsic reads the local, wraps in `Ref { id }`. Calling outside
a running process (e.g. from the REPL prompt thread before any
process has started) panics with `Ref.self() called outside a
process` — matching the runtime's "undefined" position with a
specific diagnostic.

### Lifecycle / OS signals

Same scheme as `koja-runtime/src/scheduler.rs`: per-signal
`AtomicBool` flag set by a `sigaction` handler, polled from the
worker loop, delivered into PID 1's lifecycle channel.
Implementation extracts the runtime's currently-private
`install_signals` / `poll_signals` pair into a helper crate
shared by both backends rather than duplicating the bit-level
detail. For the REPL case where there is no PID 1 (no project
entry), signals fall through to the host's default handler — the
shell exits on `SIGINT` like a normal Rust binary.

### Termination, supervisors, exit codes

When `run_process` returns, the eval scheduler:

1. Marks the registry entry `Dead`.
2. Sends `ExitSignal { pid, reason }` to every monitor recorded
   on the entry (monitors live as a `Vec<Sender<ExitSignal>>` on
   the `EvalProcess`). Mirrors the runtime's monitor delivery.
3. If this is the entry process (PID 1), captures the
   `StopReason`, runs it through `ExitStatus.code()`, stores in
   the eval-side `EXIT_CODE` global, and signals the main thread
   to wake.

`Interpreter::run_program` (the entry is always a Process state —
`fn main` entries no longer exist):

1. Spawns the entry as PID 1 with config materialized from
   `argv` (or zero-init for non-`List<String>` configs).
2. Blocks the main thread on a oneshot channel that the entry's
   `run_process` triggers on termination.
3. Returns the captured exit code as the program's exit value.

### Panic handling

A process's interpreter loop panics if the IR is malformed or if
user code calls `Kernel.panic(...)`. Eval today surfaces these as
`RuntimeError`. With the scheduler, panics on a process thread:

1. Caught by `std::panic::catch_unwind` wrapping the body of
   `run_process`.
2. Translated into `ExitReason::Crashed(panic_msg)`.
3. Delivered to monitors as if it were a normal termination.

The `koja shell` thread sees panics through the same exit-signal
machinery, so a crashed REPL-spawned process surfaces a
diagnostic in the prompt rather than tearing the whole shell
down.

## Shell ergonomics

`koja shell` inside a project:

1. Compile the project IR (eval-mode, no LLVM emit).
2. If `koja.toml` declares `entry = "App"` for some `App`
   implementing `Process`, scheduler spawns it as PID 1.
3. REPL prompt has access to all top-level definitions plus
   PID-1's `Ref` exposed as `App` (or the entry name).
4. Subsequent inputs run synchronously on the REPL thread. They
   can `App.cast(...)` to send messages, `Ref.self()` panics
   (the REPL thread is not a process), but `let r = MyActor.spawn(...)`
   works the same way it does inside `start`.

The shell becomes a live debugger for actor programs: type
expressions, watch logs interleave from running processes,
inspect state via `call`. This is the visible payoff of all the
plumbing above.

## Out of scope

- **Perf parity.** Eval will spend OS-thread context-switch time
  per message. Programs that spam millions of messages will run
  visibly slower than under LLVM. We don't bound the constant.
- **IOReady fast path.** The runtime has a third envelope type
  for I/O-readiness messages from the reactor. Initial eval
  scope drops this — fd-blocking ops in eval just block the
  process thread on the underlying syscall. The reactor still
  drives `Watch` / `Unwatch` correctness, but we don't deliver
  IOReady envelopes through the mailbox.
- **Distributed processes.** Both backends are single-node only.
- **Preemption.** Same as runtime: cooperative.
- **Persistence between REPL inputs.** Each REPL line runs
  through `lower_program` against the merged session-package set
  and hands the resulting sealed `IRProgram` to a long-lived
  scheduler instance. Spawned processes survive across REPL
  inputs; new code added in later inputs can `cast` to processes
  spawned by earlier inputs.

## Implementation phases

1. **Scheduler skeleton.** `koja-ir-eval/src/scheduler.rs` with
   the registry, channels, `run_process` entry, PID minting.
   No interpreter changes yet — just the data structures and a
   smoke test that creates a registry, spawns a thread that
   does nothing, joins it.
2. **`Spawn` instruction handler.** Replace the `Unsupported`
   stub. Tested by spawning a process whose body is a no-op
   `start` + `run` returning `StopReason::Normal`; assert PID
   minted, thread joined, exit signal delivered.
3. **`Receive` instruction handler + business channel.**
   Replace the `Unsupported` stub. Tested by spawning a process
   that receives one message and replies.
4. **Lifecycle channel + priority semantics.** `Ref.signal`,
   `Receive` priority order, `handle_signal` dispatch. Tested
   against the runtime's lifecycle-priority test fixtures.
5. **`ReplyTo<R>` and `Ref.call`.** Tested via the existing
   `Ref.call` round-trip suite.
6. **Monitors / exit signals / supervisors.** Tested via the
   stdlib supervisor fixtures (which today eval-skip).
7. **OS signal integration for PID 1.** Tested by sending
   `SIGINT` to the eval test process and asserting the entry
   sees `Lifecycle::Interrupt`.
8. **`koja shell` project mode.** Wire the scheduler boot into
   the shell command. Tested manually + a small CLI integration
   test that drives the REPL via stdin and asserts entry
   process startup.

Each phase is independently testable and reverts cleanly. Phases
1-3 are the meat; 4-6 close the supervision story; 7-8 are the
shell-experience polish.

## Mechanical checks

Greppable / assertable invariants:

- `RuntimeError::ExternNotSupported`, `RuntimeError::Unsupported
{ detail: ... spawn ... }`, `RuntimeError::Unsupported {
detail: ... receive ... }` no longer fire on any
  process-using stdlib fixture. Grep `tests/lang/process_*` for
  `--backend=interpreter` skips and remove them.
- `koja eval` of every `tests/lang/process_*/main.koja` produces
  identical stdout to `koja run` of the same fixture. Pinned by a
  diff-based golden test.
- `koja-ir-eval` does not import `koja-ir-llvm` (parity must not
  flow through the LLVM backend). Grep:
  `rg "use koja_ir_llvm" koja/crates/koja-ir-eval/`.
- Eval scheduler thread names match `koja-pid-{N}` (debugger
  visibility; sanity check during shell sessions).
- `Interpreter::run_program` runs the Process entry wrapper — no
  `unimplemented!()` on the Process path.
- `lib/global/src/process.koja`'s test suite passes under both
  backends. Today the `process` package's test directory is
  skipped in eval mode; that exclusion is removed.

## Cross-references

- `koja/design/COMPILER-NORTHSTAR.md` — pipeline shape; eval is
  a backend on the same sealed `IRProgram` as LLVM.
- `koja/design/archive/20260609-FNMAIN.md` — process model,
  lifecycle semantics, `StopReason` / `ExitStatus` / supervisor
  design that this doc defers to.
- `koja/crates/koja-runtime/src/scheduler.rs` — the LLVM-side
  reference implementation. When in doubt about observable
  semantics, the runtime is the spec.
- `koja/crates/koja-ir-eval/src/interpreter.rs` — the
  `Spawn` / `Receive` arms that today return `Unsupported`.
  These are the surgical sites for phases 2-3.
