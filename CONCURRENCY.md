# Expo Concurrency Model

Expo has two concurrency primitives: **tasks** and **actors**. They exist as
separate language-level concepts because they serve different purposes, have
different performance characteristics, and interact with ownership differently.

Most languages unify concurrency into a single primitive (goroutines, BEAM
processes, OS threads). Expo separates them because in native compiled code
without a VM, the cost difference between a short-lived concurrent computation
and a long-lived stateful entity is significant. The language should honor that
distinction rather than force everything through the same abstraction.

---

## Tasks

Tasks are lightweight concurrent computations. They run a function and return a
result. Under the hood, the compiler transforms them into stackless state
machines (similar to Rust's futures) -- no call stack, no mailbox, no
supervision overhead.

```
handle = spawn fn -> fetch_user(id) end
user = await handle
```

**Key properties:**

- **Structured concurrency.** Tasks are scoped to their spawner. They cannot
  outlive the function that created them. If the spawner returns or crashes,
  its tasks are cancelled first.
- **Can borrow.** Because tasks cannot outlive their spawner, they can safely
  borrow data from the parent scope. No copies, no moves -- just read access
  to existing data. The compiler proves this is safe at compile time.
- **Cooperative yielding.** Tasks yield at `await` and I/O points. No
  preemption overhead, no compiler-inserted checks.
- **Tiny overhead.** A task is a state machine on the heap -- typically 64-256
  bytes depending on how much state crosses yield points. No stack allocation.

**Fan-out pattern (map):**

```
data = load_large_dataset()

# Each task borrows a slice of data -- zero copies
results = task.async_stream(data.chunks(100), fn chunk ->
  transform(chunk)
end)
```

Each task borrows its chunk read-only, produces a new result, and returns it.
The compiler verifies that `data` is not mutated or freed while the tasks are
borrowing it. This is safe by construction: structured concurrency guarantees
the data outlives all tasks. No lifetime annotations, no `'static` bounds.

Peak memory is ~2x the data size (original + results). The original is freed
after `async_stream` completes. Compare to Erlang, which copies data into each
worker process (~3-4x peak).

**In-place mutation (future):**

For cases where 2x memory is unacceptable, Expo will support ownership
splitting. Instead of borrowing read-only, the parent splits a collection into
owned non-overlapping pieces, moves each to a task, and reassembles the
results:

```
data = task.async_stream(data.split_owned(100), fn owned_chunk ->
  owned_chunk.each(fn item -> item.normalize() end)
  owned_chunk
end)
|> reassemble()
```

`split_owned` moves ownership of each chunk to its task. The original binding
is invalidated (use-after-move). Each task can mutate freely because it owns
its chunk. Peak memory: 1x.

This requires no new language concepts -- it uses the existing rule that
ownership allows mutation. No mutable borrows, no new keywords. It is a stdlib
addition (`split_owned` + `reassemble`) built on top of existing move
semantics.

**Error handling:**

If a task fails, the error propagates through `await`:

```
handle = spawn fn -> might_fail() end

match await handle
  Ok(value) -> use(value)
  Err(e) -> log.error("task failed: #{e}")
end
```

For task groups (`task.async_stream`), if any task fails, remaining tasks are
cancelled and the error propagates. No orphaned tasks, no resource leaks.

---

## Actors

Actors are long-lived concurrent entities with their own memory, a message
mailbox, and supervision. They are the building block for stateful services --
GenServers, workers, supervisors, and anything that needs to persist across
requests.

```
spawn counter.start(0), restart: Permanent
```

**Key properties:**

- **Isolated memory.** Each actor owns its data. No shared mutable state
  between actors. When an actor crashes, its memory is freed deterministically
  via ownership (see `MEMORY.md`).
- **Mailbox.** Actors receive messages through a typed mailbox. The compiler
  knows which message types an actor accepts.
- **Supervision.** Actors can be supervised with restart strategies. A crashed
  actor is restarted by its supervisor automatically.
- **Real stack.** Actors have their own call stack (typically 4-8KB). They can
  call arbitrary functions and block in `receive` without special annotations.
- **Preemptive scheduling.** The compiler inserts yield checks at function
  calls and loop back-edges. Actors cannot starve the scheduler.
- **Unstructured lifetime.** Actors outlive their spawner. They run until they
  exit, crash, or are explicitly stopped.

**Defining an actor:**

The actor's message type is an enum. The `receive` clauses match against its
variants:

```
enum CounterMsg
  Increment
  Decrement
  Get(Handle)
end

actor Counter
  state count: Int32 = 0

  receive Increment ->
    @count += 1
  end

  receive Decrement ->
    @count -= 1
  end

  receive Get(from) ->
    reply(from, @count)
  end
end
```

**Typed messages:**

The compiler knows `Counter` accepts `CounterMsg` variants. Sending a message
of the wrong type is a compile error. The `receive` clauses are checked for
exhaustiveness against the message enum -- something BEAM developers have
wanted but can't have in a dynamically typed language.

---

## Actor lifecycle

### Starting an actor

`spawn` starts an actor and returns a handle. The handle is how you send
messages to it:

```
handle = spawn counter.start(0)
```

Actors can optionally be registered by name, allowing other parts of the
system to reach them without passing handles around:

```
# Option A: handle only
handle = spawn counter.start(0)
send(handle, Increment)

# Option B: named registration
spawn counter.start(0), name: "main_counter"
send("main_counter", Increment)
```

Both approaches are design candidates. Handle-based is explicit and avoids
global state. Named registration is convenient for singleton actors (the one
database pool, the one metrics collector). The final API may support both.

### Sending messages

Messages are fire-and-forget. Ownership of the message data transfers to the
receiver (see "Ownership at concurrency boundaries" below).

```
send(handle, Increment)
send(handle, Increment)
send(handle, Increment)
```

The compiler type-checks messages against the actor's message enum. Sending a
value that isn't a `CounterMsg` variant is a compile error.

### Request / reply

Fire-and-forget covers most messages, but sometimes you need a response.
Message variants that carry a `Handle` enable the reply pattern -- the actor
calls `reply(from, value)` explicitly:

```
# In the actor definition
receive Get(from) ->
  reply(from, @count)       # caller unblocks immediately
  @read_count += 1          # keep doing work after replying
  log.info("value was read")
end
```

The caller uses `call`, which blocks until a reply arrives or the timeout
expires (default: 5 seconds):

```
# Default timeout (5 seconds)
count = call(handle, Get(self()))

# Explicit timeout
count = call(handle, Get(self()), timeout: 10_000)
```

`call` returns `Result<T, CallError>` so the caller handles failure:

```
match call(handle, Get(self()))
  Ok(count) -> print("count is #{count}")
  Err(Timeout) -> print("actor didn't respond")
  Err(ActorDown) -> print("actor crashed")
end
```

**Compiler safety:** If a `receive` clause accepts a `Handle` but has a code
path that never calls `reply`, the compiler warns. This catches forgotten
replies at build time. The timeout is the runtime safety net for edge cases
the compiler can't fully trace (actor crashes mid-handler, complex conditional
logic).

**Why `reply` is an explicit call, not the return type:** actors sometimes
need to reply early and then continue doing work (logging, state bookkeeping,
side effects). If reply were the return value, the caller would be blocked
until the entire handler finishes. Explicit `reply` lets the caller unblock
immediately while the actor keeps processing.

**Open design question:** the `self()` / `Handle` plumbing in these examples
is placeholder syntax. `self()` only makes sense if the caller is an actor
with a mailbox -- it doesn't work from a regular function or `main`. The
runtime will likely inject the reply channel automatically (similar to how
Erlang's `GenServer.call` handles this behind the scenes), but the exact
mechanism and how it interacts with the message enum type system needs to be
properly designed.

### Stopping an actor

```
# Graceful: the actor finishes processing its current message, then exits
stop(handle)

# Immediate: the actor is killed, its memory is dropped
kill(handle)
```

In both cases, ownership ensures deterministic cleanup -- all data owned by
the actor is freed when it exits. No finalizers, no GC, no leaked state.

---

## Supervision

Supervisors are actors from the standard library, not a language keyword.
Their job is to start child actors, monitor them, and restart them on failure.

### Crash propagation

When a supervised actor crashes, the supervisor restarts it. If the actor
crashes too many times within a time window (e.g., three times in five
seconds), the supervisor gives up and crashes itself. This propagates up the
supervision tree:

```
root supervisor
├── database actor       (crashes 3x → supervisor gives up)
├── http server actor
└── background worker
```

If the root supervisor crashes, the OS process exits. There is no VM to keep
running. The orchestrator (Kubernetes, systemd, Docker) restarts the service
at the infrastructure level. This is the correct behavior -- a service that
can't start its critical actors should fail fast, not limp along in a degraded
state.

### Why ownership matters for crash recovery

When an actor crashes, its memory is freed deterministically through
ownership. Every value the actor owned is dropped. Then the supervisor starts
a fresh instance with clean state. There is no risk of:

- **Leaked memory** -- ownership drops everything, no GC needed.
- **Zombie state** -- the new instance starts from scratch, not from corrupted
  state.
- **Dangling references** -- other actors communicated via messages (moved
  data), not shared pointers. Nothing points into the dead actor's memory.

This is the same cleanup guarantee Erlang gets from per-process heaps and
per-process GC, achieved through ownership instead.

### Restart strategies

The exact API and available strategies are stdlib design decisions. Likely
candidates inspired by Erlang's proven model:

- **one_for_one** -- only restart the crashed child.
- **one_for_all** -- restart all children if any one crashes (for tightly
  coupled actors that depend on each other's state).
- **rest_for_one** -- restart the crashed child and everything started after
  it (for ordered dependencies where later children depend on earlier ones).

### What other languages do

- **Erlang**: built-in supervision, but the VM stays running even if the root
  supervisor crashes. Applications can be restarted within the VM. More
  resilient for multi-app deployments, but adds VM overhead.
- **Go**: no built-in supervision. Goroutine crashes are either recovered with
  `defer`/`recover` or they bring down the process. Supervision is a library
  concern with no standard approach.
- **Rust**: no built-in supervision. Panics in spawned tasks are caught by the
  runtime (tokio), but there's no restart mechanism. Supervision is entirely
  application-level.

Expo provides supervision as a standard library feature with the same
semantics Erlang developers rely on, without the VM overhead.

---

## Ownership at concurrency boundaries

The interaction between ownership and concurrency is where Expo's model
differs most from existing languages. The rules are simple:

### Tasks: borrow or move

Tasks can borrow data from their parent scope because structured concurrency
guarantees the data outlives the task. This enables zero-copy fan-out:

```
fn analyze(records: List<Record>) -> Summary
  # records is borrowed -- no copy, no move
  counts = task.async_stream(records.chunks(50), fn chunk ->
    count_types(chunk)  # chunk borrows a slice of records
  end)

  merge_counts(counts)
end
```

If a task needs to take ownership (e.g., because the parent wants to return
before the task completes -- which structured concurrency normally prevents),
use `move`:

```
handle = spawn move fn ->
  consume(data)  # data is moved into the task
end
```

### Actors: move or clone only

Actors have isolated memory. Data crosses actor boundaries via ownership
transfer (move) or explicit clone. No borrowing across actors -- their
lifetimes are independent.

```
# Ownership of config moves into the actor
spawn worker.start(move config), restart: Permanent

# Clone when the sender needs to keep a copy
spawn logger.start(config.clone())
```

**Messages are moved.** When you send a message to an actor, ownership of the
message data transfers to the receiver. Zero-copy, no reference counting, no
garbage collector.

```
send(counter, Increment)           # unit variant, trivially copied
send(worker, move large_payload)   # ownership transfers, zero-copy
```

### What this replaces

| Pattern                     | Rust               | Go                | Erlang           | Expo        |
| --------------------------- | ------------------ | ----------------- | ---------------- | ----------- |
| Concurrent read access      | `Arc<RwLock<T>>`   | mutex + goroutine | copy per process | task borrow |
| Hand off data to worker     | `move` + `'static` | channel send      | copy (message)   | actor move  |
| Fan-out over large dataset  | rayon (clone/ref)  | goroutine + mutex | copy per process | task borrow |
| Shared state across threads | `Arc<Mutex<T>>`    | `sync.Mutex`      | ETS / agent      | cache table |

The "task borrow" row is the key differentiator. Rust can't do it because
spawned tasks require `'static`. Erlang can't do it because processes are
isolated. Go can't do it safely because goroutines share mutable state. Expo
can do it because structured concurrency + ownership proves safety at compile
time.

---

## Shared data: cache tables

Backend services need shared caches -- session stores, config tables, routing
data. Actor isolation means actors can't read each other's memory, so the
naive approach is routing all reads through a single cache actor. This
serializes reads and creates a bottleneck at scale.

The stdlib will provide a concurrent shared map where reads borrow and writes
move, fitting Expo's ownership model (working name `shared_map` -- needs a
proper name):

```
import shared_map

table = shared_map.new()

# Write: value moves into the table. Writer can't use it anymore.
shared_map.put(table, "session_123", move session_data)

# Read: borrows a reference. Zero copy, read-only.
data = shared_map.get(table, "session_123")

# Delete
shared_map.delete(table, "session_123")
```

**Why this doesn't explode memory:** reads return borrows, not copies. Fifty
actors reading the same key all get read-only references to the same data.

**Why this doesn't corrupt:** writes move ownership in. The old binding is
invalidated -- no stale writes through a held reference. The implementation
uses sharded locks or lock-free algorithms internally, but the programmer
never sees that.

This is cleaner than Erlang's ETS, which copies data on every read and allows
concurrent writes that can race. Expo borrows on read (zero-copy) and moves
on write (no races by construction).

**Guidance:** for broadcasting large payloads to multiple actors, prefer
storing the data in a cache table and sending lightweight keys in messages
rather than cloning and sending the payload to each actor.

---

## The runtime

Expo does not have a virtual machine. The runtime is a native library linked
into the compiled binary -- similar to Go's runtime, not the BEAM.

**Components:**

- **Work-stealing scheduler.** One OS thread per CPU core. Actors and tasks
  are distributed across scheduler threads. Idle threads steal work from busy
  ones.
- **Actor lifecycle manager.** Handles actor creation, mailbox delivery,
  supervision tree management, and crash recovery.
- **Timer wheel.** Manages timeouts, intervals, and delayed messages. One
  shared timer wheel, not per-actor timers.
- **I/O reactor.** epoll (Linux) / kqueue (macOS) backed async I/O. Actors
  and tasks that perform I/O are suspended and resumed by the reactor
  transparently.

**What the runtime does NOT include:**

- No bytecode interpreter or JIT -- code is native machine code.
- No garbage collector -- ownership handles memory (see `MEMORY.md`).
- No atom table -- there are no atoms, strings are strings.
- No code server -- all code is linked at compile time.
- No distribution protocol -- clustering is a library concern, not runtime.

---

## Scheduler and priority

### How actors are preempted

The compiler inserts lightweight yield checks at two points in actor code:

1. **Function call preambles.** Before each function executes, decrement a
   counter. If zero, yield to the scheduler.
2. **Loop back-edges.** At the jump instruction ending each `while`/`for`/`loop`
   iteration, same check.

This catches both deep call chains and tight CPU loops. The overhead is one
compare-and-branch instruction per check -- negligible on modern CPUs because
the "don't yield" path is predicted with near-100% accuracy.

Tasks do not have these checks. They yield only at `await` and I/O points.

### Priority levels

Actors run at `Normal` priority by default. The priority controls how large
the actor's scheduling budget is -- higher priority actors get longer
timeslices before yielding.

```
# Default priority
spawn worker.start(config)

# Lower priority: yields more often, won't hog the scheduler
spawn logger.start(), priority: Low

# Higher priority: longer timeslices for compute-heavy work
spawn cruncher.start(data), priority: High
```

Three levels: `Low`, `Normal`, `High`. A sensible default that requires no
annotation for the vast majority of actors.

---

## Memory overhead

The separation between tasks and actors directly impacts memory consumption at
scale.

|                      | BEAM (Elixir) | Go       | Rust + tokio | Expo (projected) |
| -------------------- | ------------- | -------- | ------------ | ---------------- |
| Bare binary RSS      | 40-60 MB      | 3-5 MB   | 1-3 MB       | 2-5 MB           |
| Small service        | 50-150 MB     | 10-30 MB | 5-15 MB      | 5-20 MB          |
| Per-task overhead    | ~2.5 KB       | 2 KB     | ~64 bytes    | ~64-256 bytes    |
| Per-actor overhead   | (same)        | (same)   | N/A          | ~4-8 KB          |
| 10K concurrent tasks | ~25 MB        | ~20 MB   | ~640 KB      | ~1-2 MB          |
| 10K actors           | ~25 MB        | ~20 MB   | N/A          | ~40-80 MB        |

BEAM's overhead comes from the VM infrastructure (interpreter, per-process GC,
atom table, code server, distribution protocol), not from the actor model
itself. Expo drops all of that. The runtime is a scheduler and a mailbox
system.

The critical optimization: most concurrent work in backend services is
task-shaped (parallel HTTP calls, fan-out queries, data processing), not
actor-shaped (stateful services, supervisors). By making tasks cheap, the
common case is cheap. Actors cost more per unit but you spawn fewer of them --
they represent system components, not individual computations.

---

## Design rationale

### Why concurrency drives the design

Most languages design memory management first and concurrency second. This
creates a pattern where concurrency feels bolted on:

- **Rust** designed ownership and borrowing in isolation, then layered
  concurrency on top. The result is correct but hostile: `Send + Sync +
'static` bounds, `Arc<Mutex<T>>`, `Pin<Box<dyn Future<...>>>`, and async
  function coloring.
- **Zig** designed allocators first. `async/await` was added and then removed
  in 0.11. The concurrency story remains unresolved.
- **C++** designed manual memory management. Threads and mutexes were bolted
  on in C++11.

Languages that designed concurrency first ended up with simpler, more natural
concurrent programming:

- **Erlang** designed around lightweight processes and message passing. The
  memory model (per-process heaps, share nothing, copy on send) fell out of
  the concurrency design. Nobody thinks about memory management in Erlang.
- **Go** designed goroutines and channels as day-one features. GC was chosen
  specifically to make concurrency easy.

Expo takes the concurrency-first approach: the actor model and task primitives
are the primary design drivers, and ownership rules are shaped to serve them.
The ownership questions ("who can borrow across a spawn?", "what can cross an
actor boundary?", "when is data freed after an actor crash?") have clean
answers because they were asked during concurrency design, not retrofitted
afterward.

### Why two primitives instead of one

Erlang unifies everything into processes because on the BEAM, the cost
difference between a short computation and a long-lived service is negligible
(~2.5 KB either way). In native code without a VM, the difference between a
64-byte state machine and a 4-8 KB stack with a mailbox is significant.

More importantly, the two primitives enable different ownership rules:

- Tasks can borrow because structured concurrency proves safety.
- Actors must move because their lifetimes are independent.

If everything were a single primitive, you'd have to pick one rule. Erlang
picks "copy everything" (safe but expensive). Rust picks "move everything +
`'static`" (safe but verbose). Go picks "share everything" (easy but unsafe).
Expo picks the right rule for each case.

### No mutable borrows, ever

Expo has exactly two access modes: "I own it and can do anything" or "I'm
borrowing it and can only read." Concurrency does not add a third mode. Tasks
that need to mutate data receive ownership of it (via `split_owned`), not a
mutable borrow. This keeps the ownership model simple and avoids the complexity
of Rust's `&mut T` in concurrent contexts.

The roadmap path is:

1. Ship the map pattern with read-only task borrows (day one).
2. Add `split_owned` + reassembly as a stdlib feature (later).
3. Never add mutable borrows to the language.

### The pitch

The performance of Rust's concurrency. The ergonomics of Erlang's actor model.
A zero-copy task primitive that neither of them can offer.
