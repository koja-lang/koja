# Concurrency Design — Part 2

Continuation of [CONCURRENCY.md](CONCURRENCY.md). This document captures design
refinements from March 2026 that simplify the ownership-at-concurrency-boundaries
model, introduce the `copy` keyword, and unify the rules for tasks and actors.

The original document remains valuable as exploration history. This document
records the decisions that emerged from that exploration.

---

## Key decision: no borrowing across spawn boundaries

CONCURRENCY.md proposed that tasks could borrow data from their parent scope,
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

|                | Task                          | Actor                          |
| -------------- | ----------------------------- | ------------------------------ |
| Lifetime       | Structured (scoped to parent) | Unstructured (independent)     |
| Overhead       | ~64-256 bytes (state machine) | ~4-8 KB (stack + mailbox)      |
| Ownership rule | Move or clone                 | Move or clone                  |
| Use case       | Async function call           | Long-lived stateful service    |

---

## Why move is the real advantage over Erlang

The original framing ("task borrows are Expo's advantage over Erlang") was
wrong. The real advantage is simpler and more fundamental: **move**.

Erlang's limitation isn't that it copies — it's that copying is the *only*
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

| Keyword | Cost    | Caller keeps it? | Callee owns it? |
| ------- | ------- | ----------------- | --------------- |
| (none)  | Zero    | Yes               | No (read-only)  |
| `move`  | Zero    | No                | Yes             |
| `copy`  | Clone   | Yes               | Yes (own copy)  |

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

CONCURRENCY.md explored `task.async_stream` for fan-out parallelism, relying on
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

| Language | Request handling                     | Cost                        |
| -------- | ------------------------------------ | --------------------------- |
| Erlang   | Deep copy of request per message     | GC pressure per request     |
| Go       | Shared pointer to request            | Data race risk              |
| Rust     | `Arc<Request>` + `Send + 'static`    | Safe but verbose            |
| Expo     | Move the request, move the response  | Zero-copy, zero annotations |

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

See [ROADMAP.md](ROADMAP.md) Phase 3 "Runtime" section for the full scheduler
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

## What changes in CONCURRENCY.md

The following sections of CONCURRENCY.md are superseded or refined by this
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

The rest of CONCURRENCY.md (actor lifecycle, supervision, cache tables, runtime,
scheduler, memory overhead) is unchanged and still current.

---

## Open questions

- **Task vs actor distinction.** With identical ownership rules (move/clone), is
  the task/actor split still justified by the weight difference alone? Or does
  structured lifetime (tasks auto-cancel when parent exits) provide enough
  semantic value? Leaning yes — the structured lifetime guarantee is valuable for
  "fire N requests, collect results" patterns where you want automatic cleanup.

- **`copy` keyword timing.** Should `copy` ship with the initial ownership
  system, or be added later as sugar? It's not blocking — `.clone()` works
  today. But adding it early establishes the three-mode pattern before developers
  build habits around `.clone()` everywhere.

- **Handle drop semantics.** When a `Handle<T>` is dropped without being
  awaited, is the associated task cancelled? Leaning yes — structured
  concurrency implies the task shouldn't outlive its handle. But this needs to
  interact correctly with supervision for actor handles.

- **`spawn` syntax for borrow params.** With the function-call model
  (`spawn f(x)`), the callee's signature determines whether `x` is borrowed,
  moved, or copied. But at the spawn boundary, borrows are disallowed. Should
  the compiler error on `spawn f(x)` when `f` borrows `x`? Or auto-promote the
  borrow to a clone? Leaning error — explicit is better than implicit, and the
  developer should change the signature to `move` or `copy`.
