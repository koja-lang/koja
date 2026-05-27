# Entry Points, Inline Functions, and the `fn main` Problem

> **Revision 2026-04-18 — Package-as-Type Model.** The sections below predate this revision and capture the model it supersedes. They are kept for context. The OTP-flavored runtime (`Process`, `Lifecycle`, `handle_signal`, supervisors, `Ref` operations, error handling) carries forward unchanged. What changes is the namespace and entry-point story.

## Revision 2026-04-18: Package-as-Type Model

### What changes

The original design achieves consistency by **restriction**: no free functions, period; `.koja` for structured modules and `.kojs` for scripts; entry-as-`Process`-impl-on-a-type with a casing trick (`entry = "App"` vs `entry = "main"`) to disambiguate.

This revision achieves consistency by **permission**, around one observation: every named container in Koja is type-shaped, and the same `.` member-lookup rule applies recursively. Packages are types too -- degenerate ones with no fields or variants, only members.

That single move dissolves the `.koja` / `.kojs` split, the casing trick, the "Free function codegen gap" ([GAPS.md](GAPS.md)), and most of the nested-types / local-types deferrals ([GAPS.md](GAPS.md)).

### The unifying rule

> `X.y` means "look up `y` in `X`'s member table." `X` is a type -- struct, enum, package, or fn body. `y` is whatever member fits -- variant, static fn, instance method, nested type, constant, sub-package.

The resolver already handles the variant-vs-static-fn case (`Color.Red` vs `String.new`). Adding "package" as another shape of type-with-members is one more arm of an existing match, not a new class of problem.

The fractal hierarchy:

```
package ⊃ {types, fns, constants, sub-packages, impls}
  type ⊃ {types, fns, fields/variants, impls}
    fn ⊃ {types, fns, statements}
```

Recursion bottoms out at primitives. Files have no place in this hierarchy -- they are pure organization.

### Concrete rules

**Packages are PascalCase and comptime-only.** `std` → `Global`, `net` → `Net`, `koja_http` → `Koja.HTTP`. PascalCase (with uppercased acronyms) eliminates the lowercase package-vs-fn ambiguity that would otherwise block free functions at file top-level. Packages are erased after typecheck -- no runtime `Package` type, no member tables, no metadata. Anywhere you'd want to pass a package as a value, write a generic with a protocol bound instead.

**Free functions are members of the implicit package.** A `fn keyword?(s: String) -> Bool ... end` at file top-level in package `MyApp` is the same as defining a fn member of the `MyApp` package-type. Codegen treats it identically to an inline fn on any other type. No special "free function" path.

**Files are organization.** Multiple files in the same package contribute to the same package-type's member table. Renaming, splitting, or merging files does not change semantics. There is no implicit "file struct," no per-file `main`, no file-scoped visibility.

**One file extension: `.koja`.** `.kojs` is removed. The script case is a single `.koja` file with a `fn main`:

```koja
# hello.koja
fn main
  IO.puts("hi")
end
```

`koja run hello.koja` treats the file as a one-file package, looks up `main`, calls it. Top-level statements remain forbidden everywhere -- the existence of `fn main` _is_ the script idiom. This preserves the "no surprises on import" property: qualifying into another package never executes its top-level code, because there is no top-level code to execute.

**One-file packages take their name from the filename.** Running `koja run hello.koja` without an `koja.toml` defines a one-file package `Hello` -- the filename converted to PascalCase (`lexer_demo.koja` → `LexerDemo`). The file's members are accessible internally without qualification (`main`, `keyword?`, `Lexer`); externally they would be `Hello.main`, `Hello.Lexer`, though "external" rarely applies to a script. A one-file package has exactly one file -- additional files belong to a real package defined by `koja.toml`.

**Entry resolution has no casing trick.** The `entry` field in `koja.toml` names a member of the entry package. The compiler resolves it: if it's a fn, the runtime calls it; if it's a type implementing `Process`, the runtime constructs and spawns it.

```toml
[project]
entry = "App"        # type implementing Process → spawn
# or
entry = "Run"        # fn at package top-level → call
```

Both names are PascalCase by package-member convention; the kind is read from the declaration, not from the name.

**Dispatch is always protocols + generics.** Packages stay comptime. The "module as value" pattern in Elixir (`apply(MyMod, :run, args)`) exists because Elixir lacks compile-time generic dispatch. Koja has both -- anywhere you'd want to pass a package as a value, you instead write `fn run<T: Runnable>(x: T)` and let the type system dispatch statically.

### Comparison to the prior model

| Prior model (preserved below)                              | This revision                                                        |
| ---------------------------------------------------------- | -------------------------------------------------------------------- |
| No free functions, period                                  | Free functions are package members; same machinery as inline fns     |
| `.koja` vs `.kojs`                                         | One extension; scripts are `.koja` files with `fn main`              |
| `entry = "main"` vs `entry = "App"` casing-as-semantics    | Entry is a named member; compiler resolves kind from the declaration |
| Files are bags of decls; package is a flat namespace label | Package is a type; files are organization only                       |
| Nested types / local types in fns deferred                 | Same machinery as package-as-type; falls out without separate design |
| Module-as-value not addressed                              | Out: dispatch via protocols + generics; packages stay comptime       |

### What carries forward unchanged

- `Process<C, M, R>` as the protocol for long-running services (entry types and spawned children alike)
- `Lifecycle` enum, `handle_signal` default impl, OS-signal-to-`Lifecycle` mapping
- `StopReason`, `ExitStatus`, `ExitReason`, supervisor shutdown ordering
- `Ref.signal` / `Ref.kill` / `Ref.self()`, child-to-parent messaging via union message types
- Inline functions on `struct` / `enum` bodies (already implemented)
- Error handling philosophy (types first, panics last, supervision as safety net)
- The cookbook distribution model ([PACKAGE.md](PACKAGE.md) -- unchanged, it's about distribution, not language)

### Migration notes

- Stdlib package names rename to PascalCase. One-shot mechanical rewrite touching every import.
- Existing free-function workarounds (wrapping helpers in dummy `impl` blocks) inline back to package level without ceremony.
- `.kojs` never ships; the script case is `fn main`-in-a-`.koja`-file from day one.
- GAPS entries that close as side effects:
  - "Free function codegen gap" -- free fns become package members; existing codegen path handles them.
  - "Nested types deferred" -- the `(package, name)` identity model extends to package-nested types.
  - "Local types in function bodies" -- fn body is a type-shaped container; nested types reuse the same machinery. The `DefId` overhaul flagged there may no longer be required.
- GAPS entries unaffected: iteration protocol, generic-impl gaps, `Debug.format` for tuple variants, nested type-aliased unions, closures-capturing-`self`, etc. These are orthogonal to the namespace model.

### Open questions

- **Sub-package syntax.** `Std.IO` -- declared inline inside `Std`'s sources, implied by directory structure (`src/io/...`), or both? Elixir allows both. Pick one.
- **Visibility at package level.** `priv fn` inside a type body means "private to the type." Inferred meaning at package top-level: "private to the package, not exported." Confirm and write down.
- **Constant naming.** Verify current convention doesn't collide with PascalCase packages.

---

## Problem

`fn main` is the only free-floating function in Koja. All other functions must live inside `impl` blocks. This creates a fractal design inconsistency: the first thing every user learns is the one pattern that doesn't generalize. AI agents hit this hard -- they see `fn main` and assume free functions work everywhere, leading to codegen crashes when they try.

The language has three related open questions:

1. **Where does `fn main` live?** (archive/20260403-IMPORT.md, lines 585-595)
2. **Should `impl Type` migrate to inline functions in type bodies?** (ROADMAP.md, lines 514-521, 538-542)
3. **How does `fn main` relate to `Process<C, M, R>`?** (archive/20260403-PROCESS.md, lines 256-281; ROADMAP.md, lines 304-312)

This doc proposes a unified answer to all three.

---

## Two modes: projects and scripts

Koja has two execution modes with different file extensions and different semantics.

### `.koja` files -- structured modules

`.koja` files contain type definitions, `impl` blocks, constants, and protocol conformance. No top-level statements. No `fn main`. Every function lives on a type. These are the files that make up a compiled project.

### `.kojs` files -- scripts

`.kojs` files are scripts. The entire file body executes as a function -- like a bash script. Top-level statements, inline type definitions, no boilerplate. Run with `koja run script.kojs`.

```koja
# hello.kojs
IO.puts("Hello, Koja!")
```

This is Koja's equivalent of Elixir's `.exs` files. Scripts are for exploration, one-offs, and learning. They use real Koja semantics -- same types, same ownership, same `impl` blocks -- but the file itself is the entry point.

### Why two modes

The split exists because projects and scripts have different needs:

- **Projects** need structure, consistency, and the full `Process` model. The entry point is a type specified in `koja.toml`. No special cases.
- **Scripts** need zero ceremony. The file is the function body. No project config, no type scaffolding, no process protocol.

Conflating the two (allowing top-level statements in `.koja` files, or requiring `Process` in scripts) would compromise both.

---

## Proposal: project entry points are `Process` implementations

### Status: partially implemented

The dual entry mode is implemented. The `entry` field in `koja.toml` determines behavior by casing:

- **lowercase** (`entry = "main"`) -- existing behavior: resolves to a module file containing `fn main`
- **PascalCase** (`entry = "App"`) -- new behavior: names a type that implements `Process<C, M, R>`, codegen generates a C `main` that constructs and spawns it

Both modes coexist. Existing projects and tests are unchanged. The `fn main` path will be removed once `.kojs` scripts are implemented (see below).

### `koja.toml`

```toml
[project]
entry = "App"
name = "my_app"
version = "0.1.0"
```

The `entry` field names the type whose `Process` implementation the runtime invokes at startup.

### Entry type

```koja
# src/app.koja
struct App
  name: String
end

enum AppMsg
  Greet(String)
end

impl Process<App, AppMsg, ()> for App
  fn new(config: App) -> Self
    config
  end

  fn handle(move self, msg: AppMsg, from: Option<ReplyTo<()>>) -> Self | StopReason
    match msg
      AppMsg.Greet(who) ->
        IO.puts("Hello, #{who}!")
        self
    end
  end

  fn handle_signal(move self, event: Lifecycle) -> Self | StopReason
    match event
      Lifecycle.Shutdown ->
        IO.puts("Shutting down #{self.name}")
        StopReason.Shutdown
      _ -> self
    end
  end
end
```

### What this gives you

- **argv as config (implemented):** when `C = List<String>`, the runtime converts `argc`/`argv` (skipping the program name) into an Koja `List<String>` and passes it to `new`. Other config types are zero-initialized.
- **OS signals as lifecycle events (deferred):** `Lifecycle.Shutdown`, `Lifecycle.Interrupt`, `Lifecycle.Reload` are dispatched to `handle_signal`. Graceful shutdown is an override of the default implementation. Not yet implemented -- requires a two-mailbox model in the runtime.
- **Exit codes via protocol (implemented):** the entry type's `StopReason` implements `ExitStatus`, mapping to OS exit codes. Default: `Normal -> 0`, `Shutdown -> 1`. Custom exit codes via `ExitStatus` protocol implementation. The spawn wrapper captures `run`'s return value, calls `ExitStatus.code()`, and stores the result in a global `@__koja_exit_code` that C `main` reads after `koja_rt_main_done()`.
- **Supervision:** the entry type can spawn child processes in `new`. `koja new --sup` scaffolds a supervision tree.

### No special cases

The entry type is just a type. It uses the same `Process` protocol as every other process. There's no `fn main`, no free-floating functions, no "except for the entry point" asterisk. The fractal design is preserved -- the program's top-level structure is the same as any spawned process.

---

## Process lifecycle

The entry type example above uses `Lifecycle`, `StopReason`, and `handle_signal`. This section defines those types and explains how they connect OS signals, process shutdown, and exit codes.

### Lifecycle

```koja
enum Lifecycle
  Shutdown
  Interrupt
  Reload
end
```

`Lifecycle` abstracts OS signals into a platform-agnostic enum. On POSIX systems: `SIGTERM` maps to `Shutdown`, `SIGINT` to `Interrupt`, `SIGHUP` to `Reload`. Non-POSIX runtimes map their own shutdown/interrupt mechanisms to the same variants.

The entry process receives `Lifecycle` events from the OS runtime. Supervised children receive them from their supervisor. Same type, different sender.

### `handle_signal`

The `Process` protocol has two dispatch points: `handle` for business messages, `handle_signal` for lifecycle events.

```koja
protocol Process<C, M, R>
  fn new(config: C) -> Self

  fn handle(move self, msg: M, from: Option<ReplyTo<R>>) -> Self | StopReason

  fn handle_signal(move self, event: Lifecycle) -> Self | StopReason
    match event
      Lifecycle.Shutdown -> StopReason.Shutdown
      Lifecycle.Interrupt -> StopReason.Shutdown
      Lifecycle.Reload -> self
    end
  end
end
```

The mailbox internally accepts `M | Lifecycle`. The runtime dispatches `M` messages to `handle` and `Lifecycle` events to `handle_signal`. This is an implementation detail -- the user declares `M`, not `M | Lifecycle`.

`handle_signal` has a default implementation: stop on `Shutdown`/`Interrupt`, ignore `Reload`. Most processes never override it. Override for graceful drain, hot config reload, or custom shutdown sequencing.

This mirrors `run`, which also has a default implementation that most processes never touch. The pattern is: sensible defaults with opt-in customization.

### StopReason

```koja
enum StopReason
  Normal
  Shutdown
end
```

`handle` and `handle_signal` both return `Self | StopReason`. Returning `Self` continues the process. Returning a `StopReason` variant stops it.

- `Normal` -- the process finished its work (e.g., a `Task` that completed its computation).
- `Shutdown` -- the process was told to stop (lifecycle event, supervisor directive).

There is no `Error` variant. Unrecoverable errors are panics -- the process crashes and the supervisor handles it (see [Error handling philosophy](#error-handling-philosophy)).

### ExitStatus

```koja
protocol ExitStatus
  fn code(self) -> Int
end
```

`ExitStatus` maps a stop reason to an OS exit code. Only the entry process needs this -- it's the boundary between the Koja runtime and the OS.

`StopReason` has a default `ExitStatus` implementation: `Normal -> 0`, `Shutdown -> 1`. User types can implement `ExitStatus` for custom exit codes (e.g., distinguishing between graceful shutdown and specific failure modes).

---

## Supervision

### ExitReason

```koja
enum ExitReason
  Normal
  Shutdown
  Crashed(String)
end
```

`ExitReason` is what a supervisor sees when a child process stops. It is type-erased -- the supervisor manages heterogeneous children with different `M` and `R` types, so it needs a uniform view of termination.

- `Normal` / `Shutdown` map directly from `StopReason`.
- `Crashed(String)` captures the panic message when a child process crashes.

### Shutdown propagation

The supervisor stores an opaque handle per child -- a closure that captures the child's `Ref` and sends `Lifecycle` into the child's mailbox:

```koja
struct ChildHandle
  send_lifecycle: fn (Lifecycle) -> ()
end
```

When the supervisor spawns a child with `Ref<DbPoolMsg, R>`, it captures `fn (event: Lifecycle) -> () ref.cast(event) end` and stores it. The supervisor doesn't know `M` -- it only sends `Lifecycle`, never business messages.

On shutdown, the supervisor iterates children, calls `child.send_lifecycle(Lifecycle.Shutdown)` for each. The child's runtime dispatches to `handle_signal`, which returns `StopReason.Shutdown` (default) or runs custom cleanup before stopping.

### Fractal dispatch

The lifecycle mechanism is the same at every level:

- **OS runtime** sends `Lifecycle` to the entry process mailbox, dispatched to `handle_signal`.
- **Supervisor** sends `Lifecycle` to child process mailboxes, dispatched to `handle_signal`.

Same mechanism, different sender. The entry process is not special -- it's just a process whose lifecycle events happen to come from the OS instead of a supervisor.

### Pid scoping

`Pid` is an internal implementation detail of `Supervisor` and `Process.monitor`. It is not a user-facing type in normal application code. Users interact with processes through typed `Ref<M, R>` handles, which provide compile-time safety for message passing.

### Process discovery

Processes find each other through refs, not names. There are three patterns, matched to the supervisor type:

**Static supervisor:** children are known at compile time. The supervisor holds each child's `Ref` as a struct field and passes refs to siblings through config:

```koja
struct AppSupervisor
  db: Ref<DbMsg, DbResult>
  cache: Ref<CacheMsg, CacheResult>
end

# WebServer receives the db ref through its config -- no lookup needed
struct WebConfig
  db: Ref<DbMsg, DbResult>
end
```

No strings, no registry. Struct fields are the names. Typo `self.dv` instead of `self.db`? Compiler error. Wrong message type? Compiler error.

**DynamicSupervisor:** children are created at runtime (one per connection, one per job). The supervisor is type-parameterized -- all children share the same `M` and `R` types. `start_child` returns the `Ref` to the caller, who stores it in their own state:

```koja
struct DynamicSupervisor<C, M, R>
  fn start_child(move self, config: C) -> Pair<Self, Ref<M, R>>
  fn stop_child(move self, ref: Ref<M, R>) -> Self
end
```

The DynamicSupervisor's job is supervision (start, monitor, restart), not lookup. The caller manages its own mapping (e.g., connection ID to ref).

**Global registry (optional):** for well-known cross-cutting singletons (logger, metrics) where config threading is awkward. Uses constant strings to avoid typos:

```koja
const DB_POOL = "db_pool"

Process.register(ref, DB_POOL)

fn db_pool() -> Option<Ref<DbMsg, DbResult>>
  Process.whereis<DbMsg, DbResult>(DB_POOL)
end
```

The constant is defined once; the typed lookup helper pins both the name and the types. Most applications won't need the global registry -- refs through config and DynamicSupervisor cover the common cases.

---

## Ref API and shutdown primitives

`Ref<M, R>` is a typed handle to a running process. It currently supports two operations: `cast` (fire-and-forget) and `call` (synchronous with timeout). This section adds `signal` for lifecycle events, `kill` for forced termination, `alive?` for liveness checks, and changes `call` to return `Result<R, CallError>` instead of `Option<R>`.

### `Ref.signal`

Every process mailbox already accepts two kinds of messages: business messages (`M`) and lifecycle events (`Lifecycle`). The `run()` default has two receive arms for them. But `Ref` only exposes the business channel through `cast` and `call`. `signal` exposes the lifecycle channel:

```koja
extend Ref<M, R>
  fn cast(self, msg: M)                                # fire-and-forget business message
  fn call(self, msg: M, timeout: Int) -> Result<R, CallError>  # synchronous business message
  fn signal(self, event: Lifecycle)                     # fire-and-forget lifecycle event
end
```

`signal` maps directly to the runtime's `koja_rt_send_lifecycle(pid, variant)`, which pushes lifecycle events to the **front** of the mailbox (priority delivery). The runtime already uses this path for OS signals to PID 1 -- `signal` generalizes it so any process can send lifecycle events to any other process it holds a `Ref` to.

This is the mechanism supervisors use for cooperative shutdown. The supervisor doesn't need to know `M` or `R` to shut down a child -- it captures `ref.signal` in a closure at spawn time:

```koja
struct ChildHandle
  signal: fn (Lifecycle) -> ()
  id: Int
end
```

### `CallError`

`Ref.call` currently returns `Option<R>`, which conflates two failure modes: the process timed out (it's busy or slow) vs. the process is dead. These require different responses -- retry vs. escalate. `CallError` distinguishes them:

```koja
enum CallError
  Timeout
  ProcessDown
end
```

`call` becomes `fn call(self, msg: M, timeout: Int) -> Result<R, CallError>`:

- `Result.Ok(reply)` -- the process replied within the timeout
- `Result.Err(CallError.Timeout)` -- the process is alive but didn't reply in time
- `Result.Err(CallError.ProcessDown)` -- the process is dead

The runtime detects `ProcessDown` by checking the target process's state before sending or after a timeout. If the process is `Dead` in the scheduler, the call returns `ProcessDown` immediately without waiting.

`Task.await` changes to match: `fn await(move reference: Ref<(), R>) -> Result<R, CallError>`.

### `Ref.kill`

Cooperative shutdown via `signal(Lifecycle.Shutdown)` depends on the target process handling the event. If the process is stuck -- infinite loop in a handler, deadlocked on a `call` to a dead process -- it will never respond. The supervisor needs an escape hatch.

```koja
ref.kill()
```

`kill` is uncooperative. The runtime marks the process as `Dead`, frees all memory owned by it, and (once monitoring is implemented) sends `ExitSignal` to any watchers. It does not go through the mailbox. It does not call `handle_signal`. The process simply stops existing.

This is safe because Koja has no shared mutable state. Every allocation belongs to exactly one process. Kill it, free everything, no other process is affected.

`kill` is an instance method on `Ref<M, R>`. You need a handle to kill a process -- you can't kill by raw pid. This is consistent with Koja's typed-handles philosophy and eliminates the need for a separate `Pid` type. The supervisor holds refs (or closures capturing refs) for every child.

The supervisor's shutdown loop:

```koja
for child in self.children.reverse()
  child.handle.signal(Lifecycle.Shutdown)
  match self.wait_for_exit(child.id, child.shutdown_timeout)
    Option.Some(reason) -> ()
    Option.None -> child.handle.kill()
  end
end
```

The supervisor iterates children in reverse start order (last started, first stopped), sends cooperative shutdown, waits up to a per-child timeout, then force-kills if necessary. This matches OTP's shutdown semantics.

### No `Pid` type

`Ref<M, R>` is the only process handle. All operations -- `cast`, `call`, `signal`, `kill` -- go through it. The supervisor achieves type erasure through closures that capture typed refs, not through a separate untyped `Pid`. `ExitSignal` carries an `Int` (the raw process id from `ref.id`) for identification, not a `Pid` struct.

### Summary: the three shutdown primitives

| Primitive                               | Cooperative | Goes through mailbox  | Caller blocks |
| --------------------------------------- | ----------- | --------------------- | ------------- |
| `ref.signal(Lifecycle.Shutdown)`        | Yes         | Yes (front of queue)  | No            |
| `ref.kill()`                            | No          | No                    | No            |
| `supervisor.wait_for_exit(id, timeout)` | N/A         | Receives `ExitSignal` | Yes           |

These three compose into the supervisor shutdown loop. `signal` is the polite request, `kill` is the last resort, `wait_for_exit` is the synchronization point.

---

## Child-to-parent messaging and `Ref.self()`

### The gap

`Ref<M, R>` gives the parent a typed channel to the child. The parent can
`cast`, `call`, and `signal` the child. But the child has no way to push
unsolicited messages back to the parent. The only child-to-parent path is
the synchronous `call` reply via `ReplyTo<R>`, which requires the parent to
ask first.

This is a problem for I/O-driven processes. A TCP listener that accepts
connections and receives data needs to push events to its owner without
being asked. In Erlang, any process can send to any other process it knows
the pid of -- the mailbox is untyped, so `Pid ! {tcp, Socket, Data}` just
works. In Koja, mailboxes are typed: the runtime can't inject a message
unless it matches the process's declared message type `M`.

### `Ref.self()` -- the missing primitive

A static function on `Ref` that returns a typed handle to the current
process:

```koja
impl Ref<M, R>
  fn self() -> Ref<M, R>
    panic("intrinsic")
  end
end
```

The caller provides the type via annotation, same as `CPtr.alloc()` or
`List.new()`:

```koja
me: Ref<AppMsg | TcpEvent, String> = Ref.self()
```

The intrinsic reads the current process's pid from thread-local state
(`CURRENT_PID`) and wraps it in a `Ref`. The type parameters are purely
compile-time -- the runtime representation is just the integer pid.

### Union message types for multi-source mailboxes

A process that handles both business messages and I/O events declares `M`
as a union:

```koja
enum AppMsg
  DoWork(String)
end

enum TcpEvent
  Connection(TCPSocket)
  Data(TCPSocket, Binary)
  Closed(TCPSocket)
end

impl Process<App, AppMsg | TcpEvent, String> for App
  fn start(move config: App) -> Result<Self, StopReason>
    me: Ref<AppMsg | TcpEvent, String> = Ref.self()
    listener = spawn TCPListener.start(ListenerConfig{owner: me, port: 4000})
    Result.Ok(App{listener: listener})
  end

  fn handle(move self, msg: AppMsg | TcpEvent, from: Option<ReplyTo<String>>)
    -> Step<Self>
    match msg
      AppMsg.DoWork(task) ->
        # handle business logic
        Step.Continue(self)

      TcpEvent.Connection(socket) ->
        # new client connected
        Step.Continue(self)

      TcpEvent.Data(socket, bytes) ->
        # data arrived on socket
        Step.Continue(self)

      TcpEvent.Closed(socket) ->
        # client disconnected
        Step.Continue(self)
    end
  end
end
```

The child (`TCPListener`) holds `owner: Ref<AppMsg | TcpEvent, String>`.
When it accepts a connection, it calls `self.owner.cast(TcpEvent.Connection(socket))`.
`TcpEvent` is a member of the union `AppMsg | TcpEvent`, so it widens
automatically -- no wrapping, no adapter.

This is the typed equivalent of Erlang's `Pid ! {tcp, Socket, Data}`. The
difference is that the type system enforces the contract: the child can
only send `TcpEvent` values (its `R` type contributes to the parent's `M`
union), and the parent's `handle` must match on them.

### How it composes with `Process<C, M, R>`

The parent spawns a child and gets `Ref<ChildM, ChildR>`. The child's `R`
is the type it can push back. The parent's `M` is a union that includes
`ChildR`:

```
Parent: Process<..., AppMsg | TcpEvent, ...>
  spawns child → gets Ref<ListenerMsg, TcpEvent>
  passes Ref.self() to child config

Child: Process<ListenerConfig, ListenerMsg, TcpEvent>
  holds owner: Ref<AppMsg | TcpEvent, String>
  calls owner.cast(TcpEvent.Data(...))
```

The type alignment:

- Child's `R = TcpEvent`
- Parent's `M = AppMsg | TcpEvent`
- Child calls `owner.cast(TcpEvent.Data(...))` -- `TcpEvent` widens to
  `AppMsg | TcpEvent` automatically

No new runtime machinery is needed. `cast` already pushes a value into a
process's mailbox by serializing it. Union widening is a compile-time
concept -- the runtime just sees bytes in the mailbox. The `receive` loop
pattern-matches on the tag to determine which variant arrived.

### Why not a third receive clause

`Lifecycle` events have a dedicated receive clause because they are
universal -- every process needs signal handling for supervision to work.
The runtime itself generates lifecycle events (from OS signals).

I/O events are not universal. Most processes don't care about sockets. And
unlike lifecycle events (which are a fixed, known set of three variants),
I/O event types are user-defined -- `TcpEvent`, `HttpEvent`,
`WebSocketFrame`, etc. Adding a dedicated receive clause for each would not
scale.

Union types are the right tool: the process declares exactly which event
types it handles, and the type system ensures completeness. No special
runtime support, no magic mailbox categories.

### Implementation notes

`Ref.self()` requires:

- A new intrinsic in codegen that reads `CURRENT_PID` and constructs a
  `Ref` struct (just the integer pid)
- No runtime changes -- the pid is already available in thread-local
  storage
- Type inference from the annotation, same as existing generic static
  functions

The key invariant: `Ref.self()` must only be called from within a running
process (inside `start`, `handle`, `handle_signal`, or `run`). Calling it
from `fn main` or a script is undefined. The compiler could enforce this
by restricting it to `Process` impl bodies, or it could be left as a
convention (like Erlang's `self()` being meaningless outside a process).

### Relationship to TCP and HTTP

This primitive enables actor-native networking. Instead of the current
blocking `Server.start` accept loop (which takes over the calling process
and prevents signal handling), the architecture becomes:

```
App (entry process)
  - spawns TCPListener, passes Ref.self()
  - stays in default receive loop
  - handles Lifecycle signals (SIGINT → graceful shutdown)
  - handles TcpEvent messages from listener

TCPListener (child process)
  - owns the listen socket
  - accept loop runs in its own process
  - on accept: casts TcpEvent.Connection(socket) to owner
  - on data: casts TcpEvent.Data(socket, bytes) to owner
  - on close: casts TcpEvent.Closed(socket) to owner

http.Server (stdlib, built on top of TCP)
  - a Process that wraps TCPListener
  - accumulates TcpEvent.Data until headers complete
  - parses HTTP request, calls user handler
  - writes response, closes connection
  - user never touches TcpEvent directly
```

Every process stays in its receive loop. Every process can handle signals.
No blocking calls steal control from the actor model.

---

## Error handling philosophy

Koja's error model has three layers, each progressively less precise:

1. **Types are the first line of defense.** `Result<T, E>`, `Option<T>`, and exhaustive `match` catch most errors at compile time. If a function can fail, its return type says so. The caller must handle the failure case -- the compiler enforces it.

2. **Crashes are the last resort.** Panics are for truly unrecoverable situations -- violations of invariants that the type system cannot express. There is no `StopReason.Error` variant because recoverable errors should be modeled as types, not process termination.

3. **Supervision is the safety net.** When a process does crash, the supervisor sees `ExitReason.Crashed(msg)` and restarts it according to the configured strategy. This handles the cases that types and careful programming cannot predict.

This differs from Erlang's "let it crash" philosophy. In Erlang, crashes are routine -- processes crash and restart as a normal control flow mechanism. In Koja, static types mean most error conditions are handled at compile time. Crashes should be rare, not routine. Supervision exists for the genuinely unexpected, not as a substitute for error handling.

---

## Inline functions on types (implemented)

Functions can be defined directly inside `struct` and `enum` bodies. `impl` becomes the mechanism for _extending_ a type from outside (other files, protocol conformance).

### Struct with inline functions

```koja
struct Parser
  input: String
  pos: Int

  fn new(move input: String) -> Self
    Parser{input: input, pos: 0}
  end

  fn parse(move self) -> Self
    self = self.skip_whitespace()
    self
  end

  priv fn skip_whitespace(move self) -> Self
    # ...
    self
  end
end
```

### Enum with inline functions

```koja
enum Token
  Num(Float)
  Plus
  Minus
  Star
  Slash

  fn is_operator?(self) -> Bool
    match self
      Num(_) -> false
      _ -> true
    end
  end
end
```

### `impl` as extension

`impl` survives for two purposes:

1. **Protocol conformance:** `impl Protocol for Type` -- adding protocol methods to a type.
2. **Cross-file extension:** `impl Type` in a different file -- adding methods from outside the defining file.

```koja
# protocol conformance
impl Debug for Parser
  fn format(self) -> String
    "Parser{pos: #{self.pos}}"
  end
end

# cross-file extension (in another file)
extend Parser
  fn from_file(path: String) -> Self
    content = File.read(path).unwrap()
    Parser.new(content)
  end
end
```

### Migration path

Existing `impl Type` blocks in the same file as the type definition can be inlined. The compiler could support both forms during a transition period, or bare `impl Type` in the same file could become a warning.

### Visibility

`priv fn` inside a type body means private to that type -- only callable from the type's own methods, same as today in `impl` blocks.

---

## Scripts (`.kojs`)

### Semantics

A `.kojs` file is a function body. The compiler wraps its contents into an anonymous entry point for codegen. This is an implementation detail -- the user just writes statements.

```koja
# calculator.kojs
struct Calc
  result: Float

  fn add(move self, n: Float) -> Self
    self.result += n
    self
  end
end

c = Calc{result: 0.0}
c = c.add(10.0)
c = c.add(32.0)
IO.puts("Result: #{c.result}")
```

Scripts can define types inline and use them immediately. Types defined in a script are scoped to the script -- they don't leak anywhere.

### Running scripts

```bash
koja run hello.kojs       # compile and execute
```

No `koja.toml` needed. No project structure. The file extension `.kojs` tells the compiler to use script mode.

### Script limitations

- No multi-file support. A script is a single file.
- No dependency imports. Scripts use only the standard library.
- No `@test` annotations. Tests live in projects.
- No `koja build` output. Scripts are run, not compiled to binaries.

### REPL

The REPL (ROADMAP.md, lines 352-359) uses `.kojs` semantics. A REPL session is an interactive script:

- Type definitions and statements are entered interactively.
- Types persist across REPL inputs.
- Statements execute immediately.
- Same scoping, same ownership rules, same semantics. No special REPL mode.

---

## Function-scoped types (future exploration)

Since `.kojs` files already allow types inside a function body (the script _is_ a function body), the fractal design question is: can _any_ function body define types?

```koja
struct DataPipeline

  fn process(move self, raw: List<String>) -> List<Record>
    struct RawRow
      line: String
      line_num: Int

      fn parse(self) -> Record
        # ...
      end
    end

    rows: List<RawRow> = List.new()
    # ...
    rows.map(r -> r.parse())
  end
end
```

Function-scoped types would be visible only within the enclosing function body. They could not appear in the function's parameter or return types. This is a natural extension but not required for the core model -- it can be added later without breaking changes.

---

## Impact on `koja new`

`koja new my_app` scaffolds:

```
my_app/
  koja.toml
  src/
    app.koja
```

Where `src/app.koja` is:

```koja
struct App
end

enum AppMsg
end

impl Process<List<String>, AppMsg, ()> for App
  fn new(config: List<String>) -> Self
    App{}
  end

  fn handle(move self, msg: AppMsg, from: Option<ReplyTo<()>>) -> Self | StopReason
    IO.puts("Hello, Koja!")
    self
  end
end
```

`handle_signal` is not shown -- the default implementation is sufficient for a hello world.

And `koja.toml` is:

```toml
[project]
entry = "App"
name = "my_app"
version = "0.1.0"
```

Future: `koja new my_app --sup` scaffolds a supervision tree with child specs in `new`.

---

## Impact on test files

Test files use `@test` annotations on functions inside types. They are `.koja` files with no top-level statements. No change needed.

---

## Cross-references

- **archive/20260403-IMPORT.md, lines 585-595:** Open question about removing top-level functions -- this doc answers it. No free functions in `.koja` files. Top-level statements exist only in `.kojs` scripts.
- **archive/20260403-IMPORT.md, lines 203-205:** Top-level functions in flat scope -- eliminated. All functions on types.
- **archive/20260403-PROCESS.md, lines 256-281:** `fn main` vs `Process` split -- resolved. Projects always use `Process`. Scripts use `.kojs`.
- **archive/20260403-PROCESS.md, lines 304-312 / ROADMAP.md, lines 304-312:** `fn main` as `Process<C,M,R>` -- the entry type _is_ the `Process` impl. argv as config, lifecycle events via `handle_signal`, exit codes via `ExitStatus` protocol.
- **ROADMAP.md, lines 514-521:** Open question about `impl Type` migrating to inline functions -- this doc proposes it.
- **ROADMAP.md, lines 538-542:** Type system philosophy, inline functions on types -- addressed here.
- **ROADMAP.md, lines 544-551:** Namespace unification -- compatible, modules and types both own functions.
- **ROADMAP.md, lines 352-359:** REPL design -- uses `.kojs` script semantics.
- **archive/20260403-PROJECT.md, lines 555-557:** Entry file may have top-level statements -- replaced: `.koja` files never have top-level statements; `.kojs` files always do.

---

## Open: panic behavior

(Migrated from [archive/20260403-PROCESS.md](archive/20260403-PROCESS.md))

A panic is not a clean exit. The process didn't choose to return
`StopReason` -- it crashed unexpectedly. Supervisors need to distinguish
these two cases to decide restart strategy:

- **Normal exit**: the process returned `StopReason`. The supervisor knows
  the exit was intentional.
- **Panic crash**: the process called `panic()` or hit an unrecoverable
  error. The supervisor needs a different notification.

`ExitReason.Crashed(String)` is defined in `lib/global/src/process.koja` but runtime
delivery is not yet designed. Options:

**Option A: separate crash notification.** The runtime sends a different
signal type for panics. The supervisor's M includes both clean exit
notifications (carrying `StopReason`) and crash notifications (carrying
panic info). This keeps `StopReason` clean.

**Option B: runtime wraps exits.** The supervisor doesn't receive raw
`StopReason` -- it receives `ExitReason` which is either
`Normal`, `Shutdown`, or `Crashed(String)`. The supervisor always
knows whether the child exited cleanly or crashed.

In Erlang, exit reasons include `:normal`, `:shutdown`, and arbitrary crash
terms. The supervisor inspects the reason to decide on restart.

---

## Open: supervision details

(Migrated from [archive/20260403-PROCESS.md](archive/20260403-PROCESS.md))

### Prerequisites

- **`Pid` type** -- type-erased process ID for `ExitSignal` and registries.
  Distinct from `Ref<M, R>`.
- ~~**Trait bounds on generics**~~ -- **Done.**
- **`copy` keyword** -- needed for `child_spec` closures that capture config
  for supervisor restart.

### Exit notification flow

When a child process exits or crashes, the supervisor needs to know.
`ExitSignal` carries `pid: Pid` and `reason: ExitReason`:

```koja
type SupervisorMsg = SupervisorCmd | ExitSignal
```

The supervisor includes `ExitSignal` in its M type via a union. The runtime
sends an `ExitSignal` to the supervisor's mailbox when a monitored child
dies. `Process.monitor(ref)` sets up the monitoring relationship.

Exit notifications are type-erased -- the supervisor receives `ExitReason`
(not the child's typed `StopReason`), since the supervisor may monitor
children of different types.

### Child specs and restart strategies

- `ChildSpec` holds a `start: fn() -> Pid` closure and a `RestartStrategy`
- The closure captures `copy config` for type-erased restart
- Restart strategies: `OneForOne`, `OneForAll`, `RestForOne`
- Max-restarts-exceeded crashes the supervisor (fail-fast)

### Shutdown ordering

Supervisors shut down children in reverse start order (last started, first
stopped). Each child spec may include a shutdown timeout. The supervisor
sends a shutdown signal, waits up to the timeout, then kills the child if
it hasn't exited.

This interacts with the `handle_signal` design: the supervisor sends
`Lifecycle.Shutdown` to each child, which flows through `handle_signal`.

---

## Summary

| Today                                             | Proposed                                                                       |
| ------------------------------------------------- | ------------------------------------------------------------------------------ |
| `fn main` is a special free-floating function     | Entry point is a `Process` impl specified in `koja.toml` (TBD: `project.kojs`) |
| Functions defined in separate `impl` blocks       | Functions defined inline in `struct`/`enum` bodies                             |
| `impl Type` for primary methods                   | `impl` reserved for extensions and protocol conformance                        |
| Free functions disallowed (except `fn main`)      | No free functions, period. Consistent rule.                                    |
| Hello world requires `fn main` ceremony           | Hello world: `IO.puts("Hello!")` in a `.kojs` script                           |
| One file extension (`.koja`)                      | Two: `.koja` (structured modules) and `.kojs` (scripts)                        |
| REPL is a separate design question                | REPL is `.kojs` semantics, interactive                                         |
| OS signals handled ad-hoc (`Signal` placeholder)  | `Lifecycle` enum with `handle_signal` default impl on `Process`                |
| Exit codes as reply type (`ExitCode` placeholder) | `StopReason` enum, `ExitStatus` protocol for OS exit code mapping              |
| No supervision model                              | `ExitReason` for supervisors, `Lifecycle` propagation, fractal dispatch        |

---

## TBD: `project.kojs` replacing `koja.toml`

The introduction of `.kojs` scripts opens the possibility of replacing `koja.toml` with `project.kojs` -- a script that evaluates to a `Project` struct. This would be analogous to Elixir's `mix.exs`: the project config is written in the language itself, with the ability to compute values (e.g., reading a version from a file). The compiler would execute `project.kojs` to obtain the `Project` struct before compiling the project. Design details deferred.

### Status: exploration

Not yet implemented. This is a design exploration capturing the intended direction. Implementation would touch the parser, type checker, codegen, CLI, and project scaffolding, plus a migration of the stdlib and test suite.
