# Runtime gaps & architecture smells

A triage of structural problems in `koja-runtime` (~2.7K LOC across 13
files). This is a standing audit document, not a plan: each entry
records a smell, where it lives, the bug class it produces, and the
shape of the fix. Pull individual entries into their own plans as they
get tackled.

## The throughline

Almost every runtime bug we've chased — the message-envelope leaks
(`MESSAGE-LIFECYCLE.md` phases 1–5), the deferred discard-path
nested-heap leak (phase 6), the `on_cpu` scheduler race, the
nondeterministic SIGBUS in tight `call` loops — traces to one root:

> **The runtime manages raw memory and process state by hand, with
> ownership and ordering rules encoded in comments instead of enforced
> by types.**

That is the smell to attack. Every entry below is a specific instance
of it. The highest-leverage fixes (RAII, one allocator, a process
table, sanitizers) each convert a class of "correct by careful review"
into "correct by construction" or "caught by CI."

---

## Ranked by leverage

### 1. No RAII — resources are freed by explicit calls on every path

**Severity: high. Bug class: leaks (and latent use-after-free).**

`Envelope`, `ProcessStack`, `Reclaim`, `Timer.msg_buf`, and `init_state`
all have hand-written frees and **no `Drop` impl** — `wire.rs` documents
the absence as intentional:

> `koja/crates/koja-runtime/src/wire.rs` — "Freeing is always explicit —
> there is no `Drop` impl…"

The delivered-receive path's reason is legitimate (ownership moves to
compiled code). But the consequence is that **every forgotten path
leaks** — which is exactly the family of bugs `MESSAGE-LIFECYCLE.md`
phases 1–5 fixed one path at a time:

- `Timer.msg_buf` is freed by hand in two places (`cancel_timers_for`
  and the worker-loop fire site); any new early-return between them
  leaks.
- `ProcessStack` (an `mmap` mapping) leaks if a `Process` is ever
  dropped without `take_resources()`.
- The whole `Reclaim` dance exists only because `Process` can't free
  itself on drop.

**Fix.** Give these types `Drop`. For the one ownership-transfer case
(delivered receive), use an explicit defuse — `ManuallyDrop`, or an
`into_raw(self) -> *mut u8` that consumes the wrapper — so leaking
becomes a thing you opt into and can grep for, rather than the default
failure mode. This single inversion would have made phases 3–5 largely
unnecessary.

### 2. Two allocators for the same logical types

**Severity: high. Bug class: undefined behavior; blocks phase 6.**

Heap payloads are allocated through **both** `std::alloc` and libc
`malloc`, sometimes for the same logical `String`/`Binary`:

- `std::alloc::alloc`: `alloc_koja_string` (`util.rs`), envelope buffers
  (`scheduler.rs`).
- libc `malloc`: `alloc_binary` (`util.rs`), `__koja_concat_bits`
  (`intrinsics.rs`), `koja_socket_recv_from` / `koja_socket_resolve`
  (`socket.rs`).

Freeing a `std::alloc` block with `free()` (or vice-versa) is undefined
behavior. It works today only because codegen's `payload - 8` free
recipe happens to match the allocator used. It also actively **blocks
`MESSAGE-LIFECYCLE.md` phase 6**: recursive drop glue would need to free
a message's nested `String`s, which may be `malloc`'d while the envelope
is `std::alloc`'d.

**Fix.** Pick one allocator (the global one) for all Koja heap. Funnel
String/Binary/Bits allocation through a single `alloc`/`free` pair so
drop glue can be written once and can't cross allocators.

### 3. `processes: Vec<Process>` never shrinks; `pid - 1` indexing is scattered

**Severity: high. Bug class: unbounded memory growth, O(N) scheduling,
out-of-bounds panics.**

Dead processes are marked `Dead` and their heap is nulled, but **the
slot is never removed** — nothing ever pops `processes`. Consequences:

- **Unbounded growth** for any server that spawns per request (e.g. the
  `multi_process` coordinator spawns a `Ponger` every `RunTest`).
- **Every `worker_loop` iteration is O(total-ever-spawned)**: it runs
  ~6 full linear scans per turn (deadline promote, timer scan, `position`
  for a runnable process, shutdown check, `any_alive`, `any_active` /
  `nearest_deadline`). Scheduling cost climbs forever.
- `let idx = (pid - 1) as usize` is recomputed in ~10 sites, **some
  bounds-checked, some not** — e.g. `koja_rt_receive` indexes
  `guard.processes[idx]` directly, so a stale/bad PID panics across the
  C-ABI boundary (see #6).

**Fix.** One abstraction: a `ProcessTable` with a generational slotmap
(safe PID reuse), a bounds-checked `get(pid) -> Option<&mut Process>`, a
**ready queue** of runnable PIDs, and a **min-heap** of deadlines/timers.
Removes the unbounded growth, the O(N) scans, the scattered indexing,
and the keyspace collision in #7 — all at once.

### 4. Safety-critical ordering lives in prose, not types (the race surface)

**Severity: high. Bug class: nondeterministic crashes / hangs.**

Two correctness invariants are correct-by-comment only:

- The `on_cpu` flag + "publish `Blocked` before the context switch saves
  `sp`" dance (`Process` doc in `scheduler.rs`). A pickup that ignores
  `on_cpu` resumes a stale frame.
- `io_block` **must** set `WaitingIo` *before* `register` — otherwise the
  reactor's `state == WaitingIo` wake guard drops the event and the
  process parks forever (`reactor.rs`).

These can't be checked by the compiler and aren't exercised by any type.
The nondeterministic SIGBUS in tight `call` loops most likely lives here.

**Fix (cheapest first).**
1. Funnel every state change through a single
   `transition(pid, from, to)` method with `debug_assert!` on illegal
   edges.
2. Run the scheduler under **ThreadSanitizer** in CI
   (`RUSTFLAGS=-Zsanitizer=thread`) — purpose-built for this.
3. Longer term: `loom` for exhaustive interleaving tests of the
   switch/handoff.

### 5. A known liveness bug parked in a doc comment

**Severity: medium. Bug class: worker stranded forever.**

`reactor.rs` `release_fd`:

> "Does not wake any process currently `WaitingIo` on `fd`, so
> close-while-blocked from another worker will strand that worker."

A documented "strands a worker forever" is a bug, not a doc.

**Fix.** Wake any waiter on `fd` when releasing it (deliver a synthetic
error/EOF readiness), or at minimum `debug_assert!` that no process is
`WaitingIo` on that fd at release time.

### 6. Panics and `unwrap`s reachable across the C-ABI boundary

**Severity: medium. Bug class: undefined behavior on unwind.**

The `koja_*` functions are `extern "C"`, called from generated code. A
Rust panic unwinding into non-Rust frames is UB. Yet hot paths can
panic:

- `SCHED.lock().unwrap()` — poisoned-lock panic (every scheduler entry).
- `cstr_str(...).unwrap()` — `string.rs`.
- `assert!(ret == 0, "getentropy failed")` — `system.rs`.
- `Layout::from_size_align(...).unwrap()` — pervasive.

Most are "can't happen," but lock poisoning and `getentropy` failure can.

**Fix.** Use the pattern already present in `__koja_panic` (write a
diagnostic + `process::abort`) instead of unwinding, or return error
sentinels. Consider `catch_unwind` shims at the ABI edge for defense in
depth.

### 7. Two keyspaces multiplexed into one integer, with a reachable collision

**Severity: medium. Bug class: misrouted I/O events.**

The reactor distinguishes I/O-block events (key = `pid`) from watch
events (key = `fd + WATCH_KEY_OFFSET`, offset `1_000_000`) by arithmetic
(`reactor.rs`). Because PIDs never get reused and the process vec never
shrinks (#3), a long-running spawner will eventually cross 1,000,000 and
collide the keyspaces.

**Fix.** Fold both into a typed `enum EventKey { Process(Pid),
Watch(Fd) }` resolved through a table, rather than an integer offset.

### 8. `malloc` results unchecked in several places

**Severity: low. Bug class: null-deref on OOM.**

`intrinsics.rs` checks `malloc` and aborts on null; `socket.rs`
(`recv_from`, `resolve`) and `util.rs` (`build_argv`) write straight
through without a check. Inconsistent.

**Fix.** A single `xmalloc`-style helper that aborts on null, used
everywhere (folds into #2's single-allocator wrapper).

### 9. Messages to an I/O-blocked process don't wake it

**Severity: low. Bug class: delivery latency / missed signals.**

`koja_rt_send`, `send_lifecycle_to`, and the timer-fire path only
promote `state == Blocked`, never `WaitingIo`. A lifecycle message
(e.g. `SIGTERM`) sent to a process parked in `accept()` isn't seen until
its I/O happens to complete.

**Fix.** Decide the intended semantics (does a message interrupt an I/O
wait?). If yes, promote `WaitingIo → Runnable` on message arrival and
have the resumed process re-check its mailbox before re-blocking on I/O.

---

## If we only do three things

1. **RAII the resources** (`Drop` + explicit `into_raw` for the one
   transfer). Kills the leak-on-every-path family outright — the family
   `MESSAGE-LIFECYCLE.md` phases 1–5 fixed by hand. (Entry #1.)
2. **One allocator for all Koja heap.** Removes the cross-allocator UB
   hazard and unblocks phase 6's drop glue. (Entry #2.)
3. **A `ProcessTable` abstraction** (generational PIDs + ready queue +
   timer heap + bounds-checked accessor). Removes unbounded growth, the
   O(N) scans, scattered `pid - 1` indexing, and the keyspace collision
   in one move. (Entries #3, #7.)

Orthogonally and cheaply: **turn on ThreadSanitizer in CI.** Highest
leverage for the race class specifically — it converts "SIGBUS in the
field at random N" into "CI failure with a data-race report." (Entry
#4.)

## Suggested sequencing

- #1 (RAII) is the most self-contained and a clean first follow-up.
- #2 (single allocator) is a prerequisite for `MESSAGE-LIFECYCLE.md`
  phase 6 (recursive drop glue); do it before that effort.
- #3 (`ProcessTable`) is the largest blast radius — plan-mode it; it
  subsumes #7 and removes the index-panic surface in #6.
- #4 (TSan/transition guards) can land independently at any time and
  should, to characterize the existing race.
