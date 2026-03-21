# Concurrency Design — Part 2

Continuation of [20260313-CONCURRENCY.md](archive/20260313-CONCURRENCY.md). This document captures design
refinements from March 2026 that simplify the ownership-at-concurrency-boundaries
model, introduce the `copy` keyword, and unify the rules for tasks and actors.

The original document remains valuable as exploration history. This document
records the decisions that emerged from that exploration.

---

## Key decision: no borrowing across spawn boundaries

20260313-CONCURRENCY.md proposed that tasks could borrow data from their parent scope,
enabled by structured concurrency proving the parent outlives the task. This was
framed as Expo's key advantage over Erlang — zero-copy reads across tasks.

After deeper analysis of the compiler implications, this model is being dropped.
Task borrows introduce significant complexity for a theoretical advantage that
rarely matters in practice:

- Cross-spawn borrow tracking requires the compiler to understand that a
  variable is "frozen" (not movable, not droppable) for the duration of a
  concurrent borrow — a new kind of lifetime analysis that doesn't exist in
  single-threaded code.
- Fan-out with N tasks borrowing the same data requires multi-borrow lifetime
  tracking across loop iterations.
- Implicit cancellation semantics (structured concurrency auto-cancels tasks
  when the parent exits) interact badly with outstanding borrows — the compiler
  must prove that cancellation doesn't invalidate a borrow mid-use.

**The new rule: data crosses all concurrent boundaries via move or clone. Never
borrow.**

This applies uniformly to tasks and actors. The only difference between the two
primitives is lifetime and weight, not ownership rules:

|                | Task                          | Actor                       |
| -------------- | ----------------------------- | --------------------------- |
| Lifetime       | Structured (scoped to parent) | Unstructured (independent)  |
| Overhead       | ~64-256 bytes (state machine) | ~4-8 KB (stack + mailbox)   |
| Ownership rule | Move or clone                 | Move or clone               |
| Use case       | Async function call           | Long-lived stateful service |

---

## Why move is the real advantage over Erlang

The original framing ("task borrows are Expo's advantage over Erlang") was
wrong. The real advantage is simpler and more fundamental: **move**.

Erlang's limitation isn't that it copies — it's that copying is the _only_
option. Every message, every spawn, every process boundary: full deep copy, no
exceptions. The BEAM's per-process heap isolation makes this unavoidable.

Expo offers choice:

- **Move** when the sender is done with the data. Zero cost. Ownership
  transfers. The value doesn't exist in the sender's scope anymore. Erlang
  physically cannot do this — its GC model requires each process to own
  independent copies.
- **Clone** when the sender needs to keep a copy. Same cost as Erlang, but
  explicit. The developer writes `.clone()` (or uses the `copy` keyword —
  see below) and understands the cost.

Move covers the majority of real concurrent patterns:

- Request handler spawns a task to process a request → **move**. The handler
  doesn't need the request data anymore.
- Worker receives a job from a queue → **move**. The queue doesn't keep the job.
- Actor processes a message → **move**. The sender fired and forgot.
- Fan-out with unique items → **move** each item. **Clone** the shared config.

Clone is the fallback, not the default.

---

## What this eliminates from the compiler

Dropping task borrows removes the following from the compiler's responsibilities:

- **No cross-spawn borrow tracking.** The borrow checker never needs to track
  a borrow that crosses a `spawn` boundary. Borrows are always scope-local —
  they last for a function call and end when the call returns.
- **No "frozen variable" analysis.** No need to enforce that a variable
  borrowed by a concurrent task cannot be moved, reassigned, or dropped until
  the task completes.
- **No multi-borrow lifetime tracking.** No need to analyze N tasks in a loop
  all borrowing from the same parent-scope variable.
- **No special-casing of structured concurrency in the borrow checker.**
  Structured concurrency still matters for task lifetime (auto-cancel on parent
  exit), but the borrow checker doesn't need to know about it.
- **Borrow checker stays scope-based.** Borrows last for a function call,
  nothing more. The same rules apply whether the code is concurrent or not.

The entire borrow checker implementation can be single-threaded-only. Concurrency
is handled by move and clone, which are already well-understood operations.

---

## The `copy` keyword

A new parameter modifier alongside `move`, completing a three-mode system for
function parameters:

```
fn foo(x: Config)       # borrow — read-only, caller keeps ownership
fn foo(move x: Config)  # move — zero-cost transfer, caller loses access
fn foo(copy x: Config)  # copy — auto-clone at boundary, both own a copy
```

All three are declared in the function signature. All are invisible at the call
site. The caller always writes `foo(config)` — the compiler does the right thing
based on the signature. This is already how `move` works.

### Motivation

Without `copy`, every call site that shares data with a worker must remember to
write `.clone()`:

```
for i in range(0, worker_count)
  spawn Worker.start(config.clone())
end
```

With `copy`, the clone is declared in the right place — the callee:

```
fn start(copy config: Config)
  # each worker gets its own copy
end

for i in range(0, worker_count)
  spawn Worker.start(config)  # clone inserted by compiler
end
```

The callee knows it needs its own copy. That's a property of the function, not
of any particular call site. Every caller benefits from the declaration without
having to remember the `.clone()`.

### Consistency with `move`

`copy` follows the exact same pattern:

- `move` appears only in the function signature, never at the call site. The
  compiler infers the move.
- `copy` appears only in the function signature, never at the call site. The
  compiler infers the clone.
- Default (no keyword) is borrow — read-only access, caller keeps ownership.

The three modes form a clean spectrum:

| Keyword | Cost  | Caller keeps it? | Callee owns it? |
| ------- | ----- | ---------------- | --------------- |
| (none)  | Zero  | Yes              | No (read-only)  |
| `move`  | Zero  | No               | Yes             |
| `copy`  | Clone | Yes              | Yes (own copy)  |

### Edge cases

- **Copy types** (`Int32`, `Bool`, `Float64`): `copy` is redundant — these are
  already copied on assignment. The compiler should allow it silently. It serves
  as documentation of intent.
- **`copy self`**: deferred. An impl method that auto-clones `self` could be
  useful, but `self.clone()` inside the method body is explicit enough for now.
  Keep `copy` for parameters initially.

### "No magic" check

Does `copy` violate the "no magic" principle? No — for the same reason `move`
doesn't. The function signature is the contract. The caller reads the signature
to understand what happens to their data. `copy` adds one more option in the
same system. The behavior is always visible in the callee's declaration, never
hidden at the call site.

---

## Concurrency ownership summary

The full ownership story for concurrency, in four rules:

- **Moving data to a worker?** Use `move`. Zero cost. Erlang can't do this.
- **Sharing config across workers?** Use `copy`. Auto-clone at the boundary.
  Same cost as Erlang but declared in the right place (the callee's signature).
- **Reading data within the same thread?** Default borrow. Same as any function
  call. No concurrency involved.
- **Large shared read-only data across actors?** Use `shared_map` (cache table).
  Store once, borrow on read. Zero copies without borrows across spawn.

Three parameter modes, one system. No special rules for concurrency vs
single-threaded code. The ownership model is the same everywhere — the only
difference is that borrows don't cross spawn boundaries.

---

## Fan-out concurrency: actors, not task borrows

20260313-CONCURRENCY.md explored `task.async_stream` for fan-out parallelism, relying on
task borrows for zero-copy access to shared context. With task borrows removed,
fan-out uses the actor/move model instead.

### The pattern

Without the `copy` keyword:

```
config = load_config()

for order in orders
  spawn generate_invoice(order, config.clone())
end
```

With the `copy` keyword:

```
fn process_order(move order: Order, copy config: Config) -> Invoice
  generate_invoice(order, config)
end

for order in orders
  spawn process_order(order, config)
end
```

Each worker moves its unique item (zero-copy) and clones the shared config.
Config is typically small (a few fields). The per-item data — which is usually
the large part — is moved, not copied.

### Why fan-out is actor-shaped

Fan-out has properties that match actors, not tasks:

- N independent workers, potentially failing independently.
- Each worker processes one item — no shared mutable state.
- Error handling and cancellation per-worker.
- Natural fit for supervision (restart failed workers).
- Backpressure via bounded worker pools.

A stdlib `async_map` function can be built in userland Expo once `spawn` and
`await` exist. It doesn't need to be a language primitive.

### Backpressure

The naive "spawn N tasks immediately" pattern doesn't scale for large N. A
production fan-out needs bounded concurrency — M workers processing N items with
backpressure. This is a worker pool actor pattern, not a language feature.

### What about zero-copy parallel reads?

The case where N tasks need to read the same large dataset without copying is
handled by `shared_map` (cache tables). Store the data in a shared table, pass
keys to workers. Zero copies without borrows. Same pattern as Erlang's ETS, but
with borrow-on-read semantics instead of copy-on-read.

---

## Web server request flow

The move/clone model maps naturally to web server architecture:

```
Client → [accept] → Listener → [move request] → Worker → [move response] → Client
```

- Listener accepts a connection, builds a request struct, **moves** it to a
  worker. Zero-copy.
- Worker processes the request, builds a response, **moves** it to the reply
  channel. Zero-copy.
- Shared config (db pool handle, TLS settings) is **cloned once** at worker
  startup. Small, one-time cost.
- Per-request data is pure moves through the entire pipeline. No GC, no
  refcounting, no cloning in the hot path.

```
actor Worker
  fn start(copy config: Config)
    # each worker gets its own config at startup
  end

  receive HandleRequest(move request: Request) ->
    user = db.get_user(request.user_id)
    response = build_response(user)
    send(request.reply_to, response)   # response moved
  end
end
```

Compare to other languages:

| Language | Request handling                    | Cost                        |
| -------- | ----------------------------------- | --------------------------- |
| Erlang   | Deep copy of request per message    | GC pressure per request     |
| Go       | Shared pointer to request           | Data race risk              |
| Rust     | `Arc<Request>` + `Send + 'static`   | Safe but verbose            |
| Expo     | Move the request, move the response | Zero-copy, zero annotations |

---

## Iterator and concurrency co-design

Iterator methods (`.map()`, `.filter()`, etc.) and concurrent fan-out
(`.async_map()`) are the same operation — sequential vs parallel. They must be
designed together so the sequential API extends cleanly to the concurrent case.

### The key insight

With task borrows removed, there is no ownership tension between sequential and
concurrent iterators:

- **Sequential `.map()`**: closure borrows from enclosing scope, runs one
  element at a time. Normal borrow rules. No concurrency concerns.
- **Concurrent fan-out**: each worker moves/clones its data. No closures
  borrowing across spawn boundaries.

These are cleanly separated. Sequential iterators use borrows (existing rules).
Concurrent fan-out uses move/clone (existing rules). No new ownership concepts
needed for either.

### Recommendation

Design both APIs on paper, then implement sequential iterators first. The
concurrent versions (worker pools, `async_map`) can be built in userland once
`spawn`/`await` exist. Promote the best patterns into the stdlib after real
usage reveals which APIs matter.

---

## Scheduler protocol

See [ROADMAP.md](ROADMAP.md) Phase 3 Track B "Runtime" section for the full scheduler
protocol design. Key points summarized here:

- The runtime is a **protocol interface**, not a monolith. The scheduler, I/O
  reactor, and actor lifecycle manager are defined as Expo protocols with
  default implementations.
- Tasks, actors, and the scheduler are **co-designed** as one system. The
  scheduler protocol shapes how tasks yield, how actors are preempted, and how
  mailboxes are delivered.
- The native scheduler is the **first `impl`**, not a special case. It
  implements the same protocol that alternative runtimes use.
- **WASM**, testing, embedded, and game-tick runtimes can implement the same
  interface. User code doesn't change between runtimes.
- Third-party developers can write **custom runtimes** by implementing the
  protocol — the same fractal design principle that protocols bring to
  everything else in the language.

---

## What changes in 20260313-CONCURRENCY.md

The following sections of 20260313-CONCURRENCY.md are superseded or refined by this
document:

- **"Tasks: borrow or move" (line 353)** — the "task borrow" model described
  there is replaced by uniform move/clone. Tasks no longer borrow across spawn
  boundaries.
- **Comparison table (line 404)** — the "task borrow" rows should be read as
  "move/clone at spawn" instead. The zero-copy read advantage now comes from
  `shared_map`, not task borrows.
- **"Design rationale" (line 598)** — the statement "Tasks can borrow because
  structured concurrency proves safety" is superseded. Structured concurrency
  still proves task lifetime (for auto-cancel), but borrows don't cross spawn.
- **"Open design: task ownership semantics" (line 627)** — the three models
  explored there (clone-at-spawn, borrow-at-spawn, move-at-spawn) are resolved
  in favor of move/clone with the addition of the `copy` keyword.
- **".async() and pipeline concurrency" (line 927)** — the `.async()` sugar
  that clones captures at the boundary is consistent with this document's model.
  The design exploration there remains valid.

The rest of 20260313-CONCURRENCY.md (actor lifecycle, supervision, cache tables, runtime,
scheduler, memory overhead) is unchanged and still current.

---

## Open questions

- **Tasks deferred post-v1.** The task/actor split is no longer planned for the
  core language. `spawn`/`receive` processes are the only concurrency primitive.
  GenServer-like actor patterns will be built on top of processes in the stdlib.
  Tasks as stackless state machines may be revisited if process overhead proves
  too high once the runtime matures, but the current direction is: one primitive,
  compose everything from it.

- **`copy` keyword timing.** Should `copy` ship with the initial ownership
  system, or be added later as sugar? It's not blocking — `.clone()` works
  today. But adding it early establishes the three-mode pattern before developers
  build habits around `.clone()` everywhere.

- **`spawn` syntax for borrow params.** With the function-call model
  (`spawn f(x)`), the callee's signature determines whether `x` is borrowed,
  moved, or copied. But at the spawn boundary, borrows are disallowed. Should
  the compiler error on `spawn f(x)` when `f` borrows `x`? Or auto-promote the
  borrow to a clone? Leaning error — explicit is better than implicit, and the
  developer should change the signature to `move` or `copy`.

---

## Open design: `fn main` as a process and mailbox typing

`fn main` needs to be a process with a mailbox so it can `receive` replies from
spawned children. The problem: `spawn` is currently where the mailbox type `M`
gets declared (via `Process<M>`), but main isn't spawned by user code. Where does
main's mailbox type come from?

### Approaches explored

**Union enum (single typed mailbox).** Main declares a message enum that unions
all message types it expects. Standard OTP pattern — one enum per process.

```
enum MainMsg
  WorkerResult(String)
  HealthCheck
  Shutdown
end
```

Works, but breaks when a third-party library sends its own enum type back. The
library sends `LibResult`, not `MainMsg.Lib(LibResult)`. The types don't align
because the sender determines the message shape and the receiver determines the
mailbox type. When they're written by different people, you get a mismatch. This
is exactly why Erlang chose untyped mailboxes.

**Typed subjects / channels (Gleam-style).** Separate the typed channel from the
process. A process creates `Subject<T>` handles for specific conversations.
Rejected — these are essentially Go channels by another name, and introduce
channel lifecycle management complexity. Expo's philosophy is "the process IS the
channel."

**Turbofished `receive<T>()`.** The mailbox is heterogeneous. Each `receive` call
declares the type it expects. No mailbox type on the process — typing lives at
the receive site.

```
fn main()
  child = spawn(worker)
  child.send(DoWork(reply_to: self()))
  result = receive<LibResult>()
end
```

Solves main's typing problem and third-party interop cleanly. Tradeoffs: loses
compile-time send safety (unless `Process<T>` is kept as an optional send-side
annotation), requires runtime type tags for mailbox filtering, and unmatched
messages accumulate silently.

**Union types on `Process` (`Process<A | B>`).** Main declares its mailbox as a
union of types. `self()` can be narrowed to `Process<A>` for handing to a
consumer that only sends `A`.

```
fn main() : Process<ServerMsg | LibResult>
  lib_handle: Process<LibResult> = self()
  spawn(lib_worker(lib_handle))

  match receive()
    ServerMsg.Shutdown -> break
    LibResult.Success(data) -> print(data)
  end
end
```

Narrowing works because `Process<T>` is contravariant — a process that accepts
`A | B` can be used where `Process<A>` is expected (the sender sends less than
the process accepts).

### Leaning: union types as a general-purpose feature

Union types solve the mailbox problem, but they're far more broadly useful:

- **Heterogeneous collections**: `List<Post | Comment | Ad>` for API responses
  returning mixed JSON object types.
- **Error type composition**: `Result<User, ValidationError | DatabaseError>`
  without manual wrapper enums.
- **Function parameters**: `fn render(content: Text | Image | Video) -> Html`
  for accepting related but distinct types.

Implementation-wise, union types are anonymous enums — the same tagged union
representation Expo already uses for named enums. `A | B` is an unnamed enum
where the variants are the types themselves. Same codegen, same pattern matching,
same exhaustiveness checking.

Design questions that need answers before implementation:

- Order-independent: `A | B` == `B | A` (almost certainly yes).
- Flattened: `A | (B | C)` == `A | B | C` (probably yes).
- Variant name collisions: resolved by qualifying (`ServerMsg.Error` vs
  `LibResult.Error`) in match arms.
- Protocol interaction: does `A | B` implement protocol `P` if both A and B do?

This is not blocking current Phase 2 work. The existing `Process<M>` model with
a single message type works for all current use cases. Union types and the main-
as-process design will be revisited when concurrency work resumes.

---

## 2026-03-19: Protocol-based process model

### Problem with the current approach

The current typed mailbox system infers `M` from the **caller's** `Process<M>`
annotation at the spawn site:

```expo
pid: Process<Msg> = spawn worker
```

The type checker runs a pre-pass (`collect_process_msg_types`) that scans for
this pattern and records `fn_name -> M`, so that `receive` inside the spawned
function can infer the correct message type.

This is backwards. The caller is telling the function what messages it handles.
Three problems:

1. **Source of truth is on the wrong side.** The function's own signature gives
   no indication that it's a process function or what it expects. The caller
   declares the contract, not the callee.
2. **Fragile pre-pass.** The type checker must scan all function bodies for a
   specific assignment pattern before type checking begins. Only works with
   named functions in direct spawn assignments — not closures, not nested.
3. **Mismatch risk.** Multiple callers could annotate the same function with
   different `Process<M>` types. Nothing prevents it.

### Exploration: return type as mailbox type

First idea: move M to the spawned function's return type. The function declares
what messages it handles, and `spawn` infers `Process<M>` from it:

```expo
fn worker() -> Msg
  match receive
    Msg.Greet(name) -> print(name)
    Msg.Stop -> print("done")
  end
end

pid = spawn worker   # pid : Process<Msg>
```

This puts the source of truth on the right side (the function), but has a
semantic problem: `-> Msg` means "this function produces a Msg." Process
functions consume messages — the arrow points the wrong direction. A reader sees
`fn worker() -> Msg` and thinks "this returns a Msg value." It doesn't.

**Variant: `-> Stream<Msg>`.** A new generic type that describes the function's
relationship more honestly — "this function processes a stream of Msg values."
This resolves the semantic overload (`Stream<Msg>` is distinct from a regular
return type), and the compiler can distinguish process functions from regular
functions by checking for `Stream<M>` in the return position.

But `-> Stream<Msg>` still describes an **output**: the function produces a
stream. In the mailbox model, the message type describes an **input** — what the
process accepts. If taken literally, `-> Stream<Msg>` means the spawner should
read from the stream, not send to it. Following that honestly leads to a model
where `spawn` returns a read handle and the spawned function writes to it —
which is channels. Go channels, Rust `mpsc`, Gleam subjects.

Return types describe outputs. Mailbox types describe inputs. The return type is
the wrong place for this annotation.

### The protocol model

Instead of annotating functions, **structs implement a protocol to become
processes**. The mailbox type lives in the protocol's type parameter — on the
receiving side, declared by the implementor.

The protocol is `Process<M, R>` — because that's what it is. A struct that
implements `Process<M, R>` IS a process.

Two handle types replace the existing `Process<M>` struct:

- **`Pid`** — type-erased process identifier. Just the raw process ID. Storable,
  comparable, monitorable. Used in `ExitSignal`, registries, and anywhere you
  need to identify a process without knowing its message types.
- **`Ref<M, R>`** — typed capability to send messages. Wraps a `Pid` plus the
  message type `M` and reply type `R`. What `spawn` returns. What you call
  `cast`/`call` on.

After `spawn`, the struct has been moved into the process; what you hold is a
`Ref`, not the struct itself.

`Ref<M, R>` echoes the removed `ref T` keyword but is a different concept: not
a reference to data in memory, but a typed reference to a running process.
Lowercase `ref` was a keyword modifier (removed); PascalCase `Ref<M, R>` is a
type. Follows the convention: lowercase keywords modify, PascalCase names are
types.

```expo
protocol Process<C, M, R>
  fn new(config: C) -> Self

  fn handle(move self, msg: M, from: Option<Ref<R>>) -> Self

  fn run(move self)
    pair = receive
    new_self = self.handle(pair.first, pair.second)
    new_self.run()
  end
end
```

> **Rename note:** `init` was renamed to `new` (aligns with Expo's struct
> constructor convention -- `Map.new()`, `Set.new()`), and `start` was renamed
> to `run` (describes the internal receive loop, not external control like
> Elixir's `GenServer.start_link`). Old names appear in earlier design sections.

Three type parameters, three concerns:

- **C** — what you receive to construct (config/args → `new`)
- **M** — what you receive while running (messages → `handle`)
- **R** — what you send back (replies)

Internal state is `Self` — never exposed to the caller. The caller only sees
C, M, and R.

`new` transforms a public config into private process state — the bridge
between what callers know (config) and what the process needs (internal state).
`handle` is the message handler. Both are implemented by the user. `run` is a
default implementation that provides the receive loop — it blocks on `receive`,
dispatches to `handle`, takes the returned state, and tail-recurses. The user
never writes the loop.

Default protocol implementations are a new language feature motivated by this
design. Well-understood semantics (Rust default trait methods, Swift protocol
extensions, Kotlin interface defaults).

A struct becomes a process by implementing `Process<C, M, R>`:

```expo
struct CounterConfig
  initial_count: Int
end

enum CounterMsg
  Increment
  Decrement
  GetCount
end

struct Counter
  count: Int
end

impl Process<CounterConfig, CounterMsg, Int> for Counter
  fn new(config: CounterConfig) -> Self
    Counter{count: config.initial_count}
  end

  fn handle(move self, msg: CounterMsg, from: Option<Ref<Int>>) -> Self
    match msg
      CounterMsg.Increment -> Counter{count: self.count + 1}
      CounterMsg.Decrement -> Counter{count: self.count - 1}
      CounterMsg.GetCount ->
        from.map(fn (move f: Ref<Int>) -> ()
          f.send(self.count)
        end)
        self
    end
  end
end
```

Counter accepts `CounterConfig` to construct, `CounterMsg` while running, and
replies with `Int` — a typed service contract. The config is public, the
`Counter` struct's internals are private. The caller never constructs `Counter`
directly — `new` handles that.

Spawning:

```expo
fn main
  pid: Ref<CounterMsg, Int> = spawn Counter.new(CounterConfig{initial_count: 0})
  pid.cast(CounterMsg.Increment)
  pid.cast(CounterMsg.Increment)
  count = pid.call(CounterMsg.GetCount, 5000)   # count : Option<Int>
end
```

`spawn` takes a constructed struct, calls `run` on it in a new process.
`run` provides the receive loop via its default implementation. The user only
writes `new` and `handle`.

Where do C, M, and R come from? `impl Process<CounterConfig, CounterMsg, Int>
for Counter`. The compiler sees `spawn Counter.new(...)`, checks that `Counter`
implements `Process<C, M, R>`, extracts the types, and types the handle as
`Ref<CounterMsg, Int>`.

### What this solves

- **Source of truth is on the receiving side.** The struct declares what messages
  it handles via its protocol impl. Not the caller.
- **No pre-pass.** The compiler resolves M through normal protocol resolution —
  the same machinery that handles `ListLiteral<T>`, `Hash`, `Equality`.
- **No mismatch risk.** Every caller gets the same type from the same impl.
- **Struct IS the state.** `handle` takes `move self` and returns `Self` — each
  message transforms the state. No separate state management.
- **No new syntax.** Protocols, impl blocks, `move self`, `Self` return types
  are all existing language features. Default protocol implementations are the
  one new feature, motivated by this design but useful far beyond it.
- **Fractal design.** `print(x)` dispatches to `Display`. `spawn x` dispatches
  to `Process`. Language operations backed by protocol dispatch.
- **No compiler magic for the loop.** The receive loop is a default `run`
  method on the protocol, using `receive` and tail recursion — existing
  primitives. The user can override `run` if they need custom loop behavior.

### Unified handler: call and cast via Option

OTP's GenServer has three handlers: `handle_call` (synchronous, reply
expected), `handle_cast` (async, fire and forget), and `handle_info` (system
messages, catch-all). These can be unified.

**Call and cast are the same operation with an optional reply handle.** One
`handle` function, one `from` parameter:

- `from` is `Option.Some(process)` — it's a call. The handler should reply.
- `from` is `Option.None` — it's a cast. Fire and forget.

On the caller side, `Ref<M, R>` provides `cast` and `call`:

```expo
impl Ref<M, R>
  fn cast(self, msg: M)
    self.send(msg, Option.None)
  end

  fn call(self, msg: M, timeout: Int) -> Option<R>
    self.send(msg, Option.Some(self()))
    receive
      reply -> Option.Some(reply)
    after timeout
      Option.None
    end
  end
end
```

`cast` sends the message with `from = None` — fire and forget. `call` sends
with `from = Some(self())` and blocks on `receive` with an `after` clause until
the handler replies or the timeout expires. Returns `Option<R>` — `Some(reply)`
on success, `None` on timeout.

```expo
pid.cast(CounterMsg.Increment)                    # fire and forget
count = pid.call(CounterMsg.GetCount, 5000)      # blocks, returns Option<Int>
```

On the receiving side, the default `run` impl receives a `Pair<M, Option<Ref<R>>>`
and dispatches to `handle`:

```expo
fn run(move self)
  pair = receive
  new_self = self.handle(pair.first, pair.second)
  new_self.run()
end
```

`receive` in the `run` loop returns a pair of `(msg, from)`, not just the raw
message. The runtime always delivers both, even when `from` is None. This is an
internal detail — users implement `handle` and never see the pair.

### No handle_info

`handle_info` exists in Elixir because mailboxes are untyped — any process can
send any term to any process. Monitor `:DOWN` messages, timer fires, raw TCP
tuples, and stray messages all arrive in the same untyped inbox. `handle_info`
is the catch-all for messages that don't come through `call` or `cast`.

In a typed mailbox system, this is unnecessary. `send` is type-checked — you can
only send `M` to a `Ref<M, R>`. Nothing else gets in. The type system does what
`handle_info` was compensating for.

System-level concerns that currently arrive as `handle_info` messages need
first-class typed mechanisms instead:

- **Monitors/links:** process exit signals as a typed protocol, not surprise
  tuples in the mailbox.
- **Timers:** timer handles that send a specific message type, not raw terms.
- **I/O:** TCP/UDP data through typed stream abstractions.

These are better as explicit typed interfaces than as untyped messages that
require a catch-all handler to not crash the process.

### Simple processes: empty structs

Not every process needs state or config. The simplest process is an empty struct
with a unit config:

```expo
struct Printer end

impl Process<(), String, ()> for Printer
  fn new(config: ()) -> Self
    Printer{}
  end

  fn handle(move self, msg: String, from: Option<Ref<()>>) -> Self
    print(msg)
    self
  end
end

pid = spawn Printer.new(())
pid.cast("hello")
```

`C = ()` means no config needed. `R = ()` signals "this process never replies" —
cast-only, fire and forget.

One model for everything. Stateful processes have struct fields and config
structs. Stateless processes have empty structs and unit config. Same protocol,
same spawn, same API.

### GenServer parallel

The protocol model is GenServer expressed in Expo's type system:

| Elixir GenServer                    | Expo Process protocol                                          |
| ----------------------------------- | -------------------------------------------------------------- |
| `handle_call(msg, from, state)`     | `fn handle(move self, msg: M, from: Option<Ref<R>>)` with Some |
| `handle_cast(msg, state)`           | same `fn handle`, with `from` = None                           |
| `handle_info(msg, state)`           | eliminated — typed mailboxes prevent untyped messages          |
| `init(args)` returns `{:ok, state}` | `fn new(config: C) -> Self` — config in, process state out     |
| state is an opaque term             | state is the struct itself, fully typed                        |
| receive loop in GenServer module    | default `run` impl on the Process protocol                     |

### What changes in the current implementation

The protocol model supersedes:

- **`receive` moves from user code to the protocol.** `receive` remains as a
  language primitive, but users don't write it directly. The default `run`
  implementation on the Process protocol contains the receive loop. Users
  implement `handle` to process one message at a time.
- **`receive ... after` for timeouts.** `receive` gains an optional `after`
  clause: `receive msg -> ... after timeout -> ... end`. If no message arrives
  within the timeout (in milliseconds), the `after` branch executes. No
  separate `receive_timeout` primitive — one construct handles both blocking
  and timed receives. Used internally by `Ref.call` for call timeouts.
  Also useful for processes that need periodic work (heartbeats, cache expiry)
  via overridden `run` loops. Grammar and parser need updating to support
  the `after` clause on `receive` blocks.
- **The `collect_process_msg_types` pre-pass.** M comes from the protocol impl,
  resolved through normal protocol machinery.
- **Caller-side `Process<M>` annotations.** C, M, and R are inferred from the
  struct's Process impl at the spawn site.
- **Two systems for process typing.** Everything is struct + protocol + spawn.
- **`Process<M>` struct becomes `Pid` + `Ref<M, R>`.** The protocol owns the
  `Process` name. `Pid` is the type-erased raw identifier; `Ref<M, R>` is the
  typed handle carrying request and response types. `spawn` returns `Ref<M, R>`.

### Open questions

- **Reply type — decided.** `Process<C, M, R>` with R as a fixed reply type per
  process. Same shape as a service contract: C is the config, M is the request,
  R is the response. Heterogeneous reply types use a union for R. Pure-cast
  processes use `R = ()`. Per-variant reply handles in the message enum
  (option B, Gleam's approach) are more flexible but lose the clean unified
  handler signature.

  Fixed R means the type system doesn't enforce which response variant
  corresponds to which message variant — that mapping is a convention, like a
  REST API spec. The caller matches on the response union to see what came back.
  Wrapper functions narrow the type, serving the same role as a typed API client
  over raw HTTP. If the process replies with the wrong variant, it's a contract
  violation (the equivalent of a 500). This matches how real service contracts
  work — per-endpoint precision is a gentleman's agreement, not a protocol-level
  guarantee.

- **Default protocol implementations.** Required for the `run` loop. This is
  a new language feature — protocols currently only declare signatures. Adding
  default implementations is well-understood (Rust, Swift, Kotlin) and useful
  beyond processes (e.g., a `Display` protocol with a default `to_string` built
  on `display`, or an `Equality` protocol with a default `ne` built on `eq`).

- **`fn main` framing.** Main remains a bare function that spawns processes and
  communicates via their Ref handles. Application startup uses `Supervisor` —
  main creates child specs, passes them to a Supervisor, and spawns it. No need
  for main to implement Process.

- **System signals — decided.** Exit signals are regular typed messages, not a
  separate callback or protocol. `ExitSignal` is a stdlib struct:

  ```expo
  struct ExitSignal
    pid: Pid
    reason: ExitReason
  end
  ```

  `ExitSignal` carries a `Pid` (type-erased), not a `Ref<M, R>`. A supervisor
  monitoring workers with different types needs a common signal type — `Pid`
  identifies which process died without requiring knowledge of its M/R.

  A process that needs to monitor children includes `ExitSignal` in its message
  type via union: `type PoolMsg = PoolCmd | ExitSignal`. `Process.monitor(ref)`
  tells the runtime to send an `ExitSignal` to the caller's mailbox when the
  monitored process dies. The type checker verifies that the caller's M includes
  `ExitSignal` at the `Process.monitor` call site — if it doesn't, compile
  error. Match exhaustiveness forces handling.

  `Process.monitor` is a static function, not a protocol method — monitoring
  behavior is always the same (tell the runtime to watch a process), there is
  nothing to customize per-process.

  This is opt-in. Only processes whose M includes `ExitSignal` can call
  `Process.monitor` — typically library authors building supervision patterns,
  not application developers. Application developers use stdlib Supervisor
  abstractions that handle monitoring internally.

  ```expo
  enum PoolCmd
    Scale(Int)
  end

  type PoolMsg = PoolCmd | ExitSignal

  struct PoolConfig
    size: Int
  end

  struct PoolManager
    workers: Map<Pid, Ref<WorkerMsg, WorkerResult>>
  end

  impl Process<PoolConfig, PoolMsg, ()> for PoolManager
    fn new(config: PoolConfig) -> Self
      PoolManager{workers: Map.new()}
    end

    fn handle(move self, msg: PoolMsg, from: Option<Ref<()>>) -> Self
      match msg
        PoolCmd.Scale(n) -> self
        e: ExitSignal ->
          new_worker = spawn Worker.new(WorkerConfig{})
          Process.monitor(new_worker)
          self
      end
    end
  end
  ```

  Layering: `receive` (primitive) → `Process<M, R>` + `handle` (app devs) →
  `Process.monitor` + `ExitSignal` (library authors) → Supervisor stdlib
  patterns (app devs configure). Timers and I/O follow the same pattern —
  stdlib types composed into M via union, not special callbacks.

- **Process discovery and registration.** Monitoring, named processes, and
  registries are all the same problem: how do you get a handle to a process you
  didn't spawn? In Erlang, this is trivial because pids are untyped — a registry
  is just `name -> pid`. In Expo, refs are typed (`Ref<M, R>`), so discovery
  must bridge type-erased storage (`Pid`) and type-safe retrieval (`Ref<M, R>`).

  Two levels:

  **Runtime-level global registration.** Simple `name -> Pid` mapping in the
  runtime. `Process.register(ref, "counter")` and `Process.whereis<M, R>("counter")`
  returning `Option<Ref<M, R>>`. The `Option` return makes absence explicit —
  the type system forces handling, unlike Erlang where `whereis` returns `nil`
  and you might not check. Good for well-known singletons (database pool,
  metrics collector).

  **Registry as a stdlib process.** A typed `Registry<M, R>` process for
  dynamic, composable, scoped registries. Worker pools, connection managers,
  anything with N processes registered under keys. The registry implements
  `Process` like everything else, monitors its entries via `ExitSignal`, and
  auto-removes dead entries. No stale refs.

  Both coexist. Runtime provides the primitive, stdlib builds richer
  abstractions on top. Same layering as supervision.

- **Supervision, child specs, and application startup.** In Elixir, a child spec
  is `{Module, args}` — the module name and the init params. In Expo, the
  equivalent is an instance of the config struct. The config struct IS the child
  spec — it carries everything needed to start a process. The M and R come from
  the struct's `Process<C, M, R>` impl.

  The problem: a generic reusable `Supervisor` needs a `List<???>` of
  heterogeneous config types. `DatabasePoolConfig` and `WebServerConfig` are
  different types. Without dynamic dispatch (decided: static dispatch via
  monomorphization, no vtables), heterogeneous collections need type erasure.

  **Solution: config structs implement a protocol.** Like Elixir's `use
GenServer` auto-defining `child_spec/1`, config structs implement a protocol
  that produces a uniform `ChildSpec` struct. The protocol bridges typed configs
  to type-erased child specs via a closure:

  ```expo
  struct ChildSpec
    start: fn() -> Pid
    strategy: RestartStrategy
  end
  ```

  The `start` closure captures the config and calls `new` + `spawn` internally.
  The supervisor never sees the typed process — it only needs `Pid` for
  monitoring and the closure for restart.

  The `Process` protocol provides `child_spec` as a third default implementation
  (alongside `run`):

  ```expo
  protocol Process<C, M, R>
    fn new(config: C) -> Self
    fn handle(move self, msg: M, from: Option<Ref<R>>) -> Self

    fn child_spec(config: C) -> ChildSpec
      ChildSpec{
        start: fn() -> spawn(Self.new(copy config)).pid(),
        strategy: RestartStrategy.Permanent
      }
    end

    fn run(move self)
      pair = receive
      new_self = self.handle(pair.first, pair.second)
      new_self.run()
    end
  end
  ```

  Most processes never override `child_spec` — the default starts the process
  with a `Permanent` restart strategy. Processes that need custom restart
  behavior (transient, temporary) override it.

  Application startup looks like Elixir:

  ```expo
  fn main()
    children = [
      Counter.child_spec(CounterConfig{initial_count: 0}),
      DatabasePool.child_spec(DatabasePoolConfig{url: "postgres://...", pool_size: 10}),
      WebServer.child_spec(WebServerConfig{port: 4000}),
    ]

    sup = Supervisor{children: children, strategy: SupervisorStrategy.OneForOne}
    spawn sup
  end
  ```

  On restart: the supervisor calls the `start` closure again, which calls `new`
  with a copy of the original config, producing fresh process state every time.

  **Open: protocol naming.** Config structs could also implement a separate
  protocol (e.g., `Child`, `Service`, or another noun TBD) instead of
  `child_spec` living on the `Process` protocol itself. The separate protocol
  would let config structs declare supervision independently from the process
  impl — different module, different concern. The naming is unresolved:
  `ChildSpec` conflicts with the struct name, `Service` is a candidate but needs
  consideration. The mechanism is settled — a protocol on config structs
  producing a uniform `ChildSpec` — the name is not.

  Three default impls total: `run` (receive loop), `child_spec` (supervision
  bridge), and two required: `new` (config → state), `handle` (message
  dispatch).

- **Task: one-off async work.** The `Process<C, M, R>` model is optimized for
  long-running stateful processes. One-off work (compute something in the
  background, fire-and-forget side effects) shouldn't require defining a struct,
  implementing a protocol, and writing a no-op `handle`.

  `Task` is a kernel struct that absorbs this boilerplate. Under the hood it
  implements `Process<fn() -> R, (), ()>` — the config is a closure, the message
  and reply types are unit. It overrides `run` to execute the closure and exit
  instead of entering a receive loop.

  The API:

  ```expo
  handle = Task.async(fn() -> expensive_computation() end)
  // ... do other work ...
  result = handle.await()
  ```

  `Task.async` spawns the closure in a new process and returns a
  `TaskHandle<R>`, where `R` is inferred from the closure's return type.
  `TaskHandle<R>` has one method: `.await()`, which blocks until the task
  finishes and returns the result. No `cast`, no `call`, no message sending —
  just "wait for the result."

  For fire-and-forget, don't await:

  ```expo
  Task.async(fn() -> send_notification_email() end)
  ```

  `TaskHandle<R>` is a purpose-built handle type, not `Ref<M, R>`. As a kernel
  struct, `Task` can define its own handle with exactly the right interface.
  `Ref<M, R>` would expose `cast`/`call` methods that are nonsensical for a
  one-off task — `TaskHandle<R>` is honest about what you can do: await or
  ignore.

  This validates the `Process<C, M, R>` model: the protocol handles the
  stateful long-running case (GenServer), and stdlib/kernel builds ergonomic
  abstractions for simpler patterns on top. One model, multiple ergonomic
  surfaces.

---

## 2026-03-20: `Process.self()` and main's process identity

### `Process.self()`

A static method on `Process` that returns `Ref<M, R>` for the calling process.
Scoped under `Process` to avoid collision with the instance `self` parameter in
method bodies:

```expo
fn handle(move self, msg: CounterMsg, from: Option<ReplyTo<Int>>) -> Self
  self.count          # struct field access — the instance
  Process.self()      # process identity — the Ref
end
```

`self` (lowercase, no parens) is always the struct instance. `Process.self()`
(static call) is always the process Ref. No ambiguity.

Mirrors Elixir's namespacing of process operations under `Process`
(`Process.send_after`, `Process.alive?`, `Process.exit`). Opens the same
namespace for future additions:

- `Process.self()` — `Ref<M, R>` for the current process
- `Process.send_after(ref, msg, delay_ms)` — scheduled cast (GenServer tick)
- `Process.alive?(ref)` — liveness check
- `Process.exit(ref, reason)` — termination

Implementation: emits `expo_rt_self()` (returns raw pid), wraps in `Ref<M, R>`.
Inside a `handle` function, the compiler knows M and R from the
`impl Process<C, M, R>` declaration. This is the primary use case — the
GenServer self-tick pattern:

```expo
fn handle(move self, msg: TimerMsg, from: Option<ReplyTo<()>>) -> Self
  match msg
    TimerMsg.Tick ->
      do_periodic_work()
      Process.self().cast(TimerMsg.Tick)
      self
  end
end
```

### Main as a process: the `Process<C, M, R>` mapping

Main is pid=1 in the runtime. Making `Process.self()` work in main raises the
question: what are main's C, M, and R?

**R = ()** — main is the top-level process. It doesn't reply to anyone. Start
with unit. If union widening is later needed, the type system handles it.

**M = SystemMsg** — main receives system-level messages. The runtime translates
OS signals into typed messages delivered to pid=1:

```expo
enum SystemMsg
  Shutdown        # SIGTERM — Kubernetes pod termination, docker stop
  Interrupt       # SIGINT — Ctrl+C
  ChildExited(Int)  # monitored process died
end
```

This is how Erlang works — the init process receives system signals as messages.
The same mechanism that handles `CounterMsg` handles `SystemMsg`. No special
signal handlers, no callbacks — just messages in a typed mailbox.

**C = CLI args** — the process config for main is the arguments to the binary.
This is `String[] args` from Java, but hidden until needed through progressive
disclosure:

```expo
# Simple script — no ceremony
fn main
  print("hello")
end

# CLI tool — access args when needed
fn main
  args = Process.config()
  filename = args.get(0)
end

# Server — receive system signals for graceful shutdown
fn main
  sup = spawn Supervisor.new(children)
  receive
    SystemMsg.Shutdown -> sup.cast(SupervisorMsg.StopAll)
    SystemMsg.Interrupt -> sup.cast(SupervisorMsg.StopAll)
  end
end
```

The runtime passes `argv` as main's config. Simple scripts ignore it. CLI tools
access it via `Process.config()`. Servers use `receive` to handle system
signals. Same process model at every level of complexity.

This is the same insight as `Process<C, M, R>`: C is what you receive to start
(config/args → `new`), M is what you receive while running (messages →
`handle`), R is what you send back (replies). Main follows the same pattern —
C is argv, M is system signals, R is unit. The process model is universal.
