# Process Protocol Lifecycle

Design notes for the `Process<C, M, R>` protocol: exit semantics, panic
behavior, signal propagation, supervision, and how `fn main` fits in.
Everything after "Current state" is an open design question.

---

## Current state

### Protocol definition

`Process<C, M, R>` is a three-parameter protocol defined in `std.kernel`:

- **C** -- config type passed to `new` at spawn time
- **M** -- message type the process receives
- **R** -- reply type for synchronous `call` responses

```expo
protocol Process<C, M, R>
  fn new(config: C) -> Self

  fn handle(move self, msg: M, from: Option<ReplyTo<R>>) -> Self

  fn run(move self)
    receive
      pair: Pair<M, Option<ReplyTo<R>>> ->
        self.handle(pair.first, pair.second).run()
    end
  end
end
```

`Ref<M, R>` is the typed handle returned by `spawn`. It supports two
message-sending modes:

- **`cast(msg: M)`** -- fire-and-forget. Handler receives `from = Option.None`.
- **`call(msg: M, timeout: Int) -> Option<R>`** -- synchronous. Handler
  receives `from = Option.Some(reply_to)`. The caller blocks until the
  handler sends a reply via `reply_to.send(value)` or the timeout expires.

`ReplyTo<R>` wraps the caller's pid and enforces the reply type at compile
time. A convenience function `reply(from, value)` handles the `Option` match.

### What works today

Four golden tests exercise the protocol:

- `spawn_process.expo` -- Worker process handles `Greet`, `Add`, `Stop`
  messages via `cast`
- `call_roundtrip.expo` -- Calc process handles `Add`, replies via
  `ReplyTo<Int>`, caller uses `call` with timeout
- `cast_loop.expo` -- Counter process maintains state across `cast` messages
- `task.expo` -- `Task.async` / `Task.await` for one-off async work

`Task<R>` is built on `Process<Task<R>, (), R>` and overrides `run` to
execute work before entering the receive loop.

### Current limitations

- **No exit mechanism.** `handle` returns `Self` -- a process has no way to
  signal that it should stop. The `run` loop recurses forever.
- **No panic handling.** If a process panics, the runtime behavior is
  undefined from the protocol's perspective. No notification reaches
  supervisors or monitors.
- **No signal delivery.** OS signals (SIGTERM, SIGINT) have no path into
  the process protocol. There is no mechanism for main to receive or
  propagate them.

---

## Open: process exit semantics

### The problem

`handle` returns `Self`, so a process can never stop. The `run` loop is
infinite by design. Even a process that handles a `Stop` message (like the
Worker test) just returns `self` after printing -- it doesn't actually
terminate.

### Proposal: `handle` returns `Self | R`

Change `handle`'s return type so that returning an R value exits the process:

```expo
fn handle(move self, msg: M, from: Option<ReplyTo<R>>) -> Self | R
```

- Return `Self` -- continue the receive loop
- Return an R value -- exit the process with that value

R serves double duty: it's the reply type for synchronous `call` responses
AND the exit value. The exit R flows to supervisors/monitors as the process's
final output.

Example -- a database pool where R covers both replies and exits:

```expo
enum DbOutput
  QueryResult{rows: List<Row>}
  Ack
  Stopped{reason: String}
end

impl Process<DbConfig, DbMsg, DbOutput> for DbPool
  fn handle(move self, msg: DbMsg, from: Option<ReplyTo<DbOutput>>) -> Self | DbOutput
    match msg
      DbMsg.Query{sql} ->
        reply(from, DbOutput.QueryResult{rows: self.execute(sql)})
        self
      DbMsg.Shutdown ->
        DbOutput.Stopped{reason: "clean shutdown"}
    end
  end
end
```

For main: `Process<List<String>, Signal, ExitCode>`. Returning `ExitCode`
from handle exits the program. The OS is effectively the "supervisor" of
main, and `ExitCode` is the DOWN message to the operating system.

### What happens when a process panics?

A panic is not a clean exit. The process didn't choose to return R -- it
crashed unexpectedly. Supervisors need to distinguish these two cases to
decide restart strategy:

- **Normal exit**: the process returned R. The supervisor receives R and
  knows the exit was intentional. Depending on the R value and the restart
  strategy, it may or may not restart the child.
- **Panic crash**: the process called `panic()` or hit an unrecoverable
  error. The supervisor needs a different notification -- the process never
  produced an R.

In Erlang, exit reasons include `:normal`, `:shutdown`, and arbitrary crash
terms. The supervisor inspects the reason to decide on restart.

Options for Expo:

**Option A: separate crash notification.** The runtime sends a different
signal type for panics, independent of R. The supervisor's M includes both
clean exit notifications (carrying R) and crash notifications (carrying
panic info). This keeps R clean -- it only represents intentional exits.

**Option B: R includes a crash variant.** Each process's R type includes a
variant for unexpected crashes. This makes R represent "any way the process
can stop," but it means every R must account for crashes, even if the user
doesn't want to think about them.

**Option C: runtime wraps exits in a result type.** The supervisor doesn't
receive raw R -- it receives something like `ExitResult<R>` which is either
`Normal(R)` or `Crashed{pid: Pid, reason: String}`. The supervisor always
knows whether the child exited cleanly or crashed, and gets the R value
only for clean exits.

---

## Open: `run` overridability

### Current behavior

`run` has a default implementation in the protocol (the receive-handle-recurse
loop) but is overridable. Task already overrides it to execute work before
entering the receive loop:

```expo
impl Process<Task<R>, (), R> for Task<R>
  fn run(move self)
    result = self.work()
    receive
      pair: Pair<(), Option<ReplyTo<R>>> ->
        match pair.second
          Option.Some(reply_to) -> reply_to.send(result)
          Option.None -> ()
        end
        self
    end
  end
end
```

### The footgun

If a user overrides `run` and forgets the receive loop, the process ignores
all messages -- including shutdown signals. The binary just exits immediately
after setup, or worse, runs setup and then falls off the end of `run` while
children are still alive.

### Direction

`run` stays overridable. Sealing it was considered and rejected as too
restrictive -- Task's pattern (compute before receiving) is legitimate, and
similar patterns may emerge for other process kinds (batch workers, one-shot
initializers).

Possible mitigations for the footgun:

- **Compiler warning** if an overridden `run` doesn't contain a `receive`
  expression. Not foolproof (the receive could be conditional or in a called
  function) but catches the common case.
- **Documentation and convention.** Make the "always include a receive loop"
  rule prominent in guides and examples.
- **Lifecycle callback.** Add an optional `start(move self) -> Self` method
  that runs once before the default `run` loop. This covers Task's use case
  without overriding `run`. But it adds another method to the protocol and
  may not cover all cases where `run` customization is needed.

---

## Open: shutdown propagation

### The problem

When the OS sends SIGTERM to main, main needs to orchestrate graceful
shutdown of the entire process tree. Children like web servers need to drain
in-flight requests. Database connections need to close cleanly.

### Signal flow

1. OS sends SIGTERM
2. Main receives it as a typed M message (e.g., `Signal.Term`)
3. Main tells each child to shut down
4. Main waits for children to confirm they're done
5. Main exits with ExitCode

Step 3 is the design question: how does main tell children to shut down?

**Option A: shutdown is just an M message.** Every process that needs
graceful shutdown includes a `Shutdown` variant in its message type. Main
sends `ref.cast(ServerMsg.Shutdown)`. The child handles it in `handle` and
eventually returns R to exit.

Explicit and typed. No special protocol machinery. The trade-off: it's a
convention the developer must remember. There's no compile-time guarantee
that a process handles shutdown.

**Option B: built-in `shutdown` on the protocol.** Add an optional
`shutdown(move self) -> R` callback to the Process protocol, with a default
that exits immediately. Add `ref.shutdown()` to Ref. The runtime calls
`shutdown` instead of delivering a normal message.

This guarantees every process has a shutdown path, but adds a second message
channel alongside the normal mailbox.

### Timeouts

What if a child hangs during shutdown? In Erlang, each child spec includes
a shutdown timeout -- the supervisor waits that long for a clean exit, then
kills the child forcefully.

This depends on `receive...after` semantics or a supervisor-level timeout
mechanism. Both are open design areas (see ROADMAP Phase 4B).

---

## Open: `fn main` vs `Process` impl

### The problem

Expo needs to support two very different use cases:

1. **Hello world / one-off scripts.** A beginner writes `fn main` and calls
   `print`. No processes, no signals, no supervision. The learning curve
   should be Ruby-shaped.

2. **Long-running applications.** A web server, a CLI with signal handling,
   a service with a supervision tree. This needs the full `Process<C, M, R>`
   lifecycle.

These are fundamentally different entry points with different ergonomics.

### Possible split

- **`expo run file.expo`** uses `fn main`. No Process protocol, no signal
  handling, no supervision. The program runs and exits. This is what you
  get when you write a standalone `.expo` file.

- **`expo new project_name`** scaffolds a project with a full
  `Process<List<String>, Signal, ExitCode>` implementation. The entry point
  is a struct implementing the Process protocol, not a bare `fn main`.
  Library-only projects get a flag to disable the entry point.

### Unresolved tensions

- **CLIs need signal handling.** A CLI that spawns background work or long
  computations should handle SIGINT gracefully. Does it use `fn main` or
  the full Process impl? The boundary between "script" and "application"
  is fuzzy.

- **Discoverability.** If hello world is `fn main` but real applications
  are `impl Process for App`, there's a cliff. The user goes from one
  paradigm to a completely different one. Is there a gradient?

- **Library-only projects.** No entry point at all. `expo new --lib` or
  a flag in `project.expo`.

---

## Open: `System` stdlib

### The problem

Some operations are genuinely global OS state, not tied to any process's
C/M/R types:

- `System.get_env(key) -> Option<String>`
- `System.set_env(key, value)`
- `System.cwd() -> Result<String, String>`
- `System.hostname() -> String`

These don't fit into the process protocol. Environment variables are
per-OS-process, not per-Expo-process. Any Expo process can read them.

### Design

`System` is a zero-field struct in the stdlib (a namespace, like `IO`).
Methods are thin wrappers over C stdlib calls via runtime intrinsics.

Note: `System.argv` is NOT part of this -- argv is the C type parameter of
main's Process impl (`List<String>`). Similarly, exit codes are main's R,
not a `System.exit()` call. The System stdlib handles operations that are
orthogonal to the process lifecycle.

Already noted on ROADMAP Phase 4A.

---

## Open: supervision

### Overview

Supervision depends on multiple prerequisites (see ROADMAP Phase 4B):

- **`Pid` type** -- type-erased process ID for `ExitSignal` and registries.
  Distinct from `Ref<M, R>`.
- **Trait bounds on generics** -- needed for `child_spec` and generic
  process utilities.
- **`copy` keyword** -- needed for `child_spec` closures that capture config
  for supervisor restart.

### Exit notification flow

When a child process exits or crashes, the supervisor needs to know. The
current ROADMAP design uses `ExitSignal` with a `pid` and `reason`:

```expo
type SupervisorMsg = SupervisorCmd | ExitSignal
```

The supervisor includes `ExitSignal` in its M type via a union. The runtime
sends an `ExitSignal` to the supervisor's mailbox when a monitored child
dies. `Process.monitor(ref)` sets up the monitoring relationship.

### Interaction with exit semantics

How the exit notification carries information depends on the exit semantics
decision (Section 2):

- If `handle` returns `Self | R` and the child exits cleanly by returning R,
  does the supervisor receive the R value? R is specific to each child's
  type, and the supervisor may monitor children of different types. The
  `ExitSignal` may need to be type-erased (carrying a reason string or enum)
  rather than carrying the child's typed R.

- If the child panics, the supervisor receives a crash notification. The
  distinction between clean exit and panic crash determines restart behavior
  (permanent processes restart on any exit, transient processes only restart
  on crashes, temporary processes never restart).

### Child specs and restart strategies

From the ROADMAP design:

- `ChildSpec` holds a `start: fn() -> Pid` closure and a `RestartStrategy`
- The closure captures `copy config` for type-erased restart
- Restart strategies: `OneForOne`, `OneForAll`, `RestForOne`
- Max-restarts-exceeded crashes the supervisor (fail-fast)

### Shutdown ordering

Supervisors shut down children in reverse start order (last started, first
stopped). Each child spec may include a shutdown timeout. The supervisor
sends a shutdown signal, waits up to the timeout, then kills the child if
it hasn't exited.

This interacts with the shutdown propagation design (Section 4): the
supervisor needs a way to tell each child to shut down (message or built-in)
and a way to enforce the timeout (`receive...after` or runtime support).

---

## Summary of open questions

| Area | Core question |
| --- | --- |
| Exit semantics | Should `handle` return `Self \| R`? What is R -- reply type, exit type, or both? |
| Panic behavior | How does the supervisor distinguish a clean exit from a panic crash? |
| `run` overridability | Stays overridable, but how do we mitigate the "forgot the receive loop" footgun? |
| Shutdown propagation | Is shutdown a regular M message or a built-in protocol mechanism? How are timeouts enforced? |
| `fn main` vs Process | Where's the boundary between simple scripts and full applications? Is there a gradient? |
| System stdlib | Scope and API surface for genuinely global OS operations |
| Supervision | How do typed per-process exit values (R) flow through a type-erased supervisor? |
