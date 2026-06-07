# Runtime gaps & architecture smells

A triage of structural problems in `koja-runtime` (~2.7K LOC across 13
files). This is a standing audit document, not a plan: each entry
records a smell, where it lives, the bug class it produces, and the
shape of the fix. Pull individual entries into their own plans as they
get tackled.

## The throughline

Almost every runtime bug we've chased — the message-envelope leaks
(`archive/20260529-MESSAGE-LIFECYCLE.md` phases 1–5), the deferred
discard-path nested-heap leak (envelope `drop_glue` wiring, §1), the
`on_cpu` scheduler race, the
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
leaks** — which is exactly the family of bugs
`archive/20260529-MESSAGE-LIFECYCLE.md` phases 1–5 fixed one path at a
time:

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

**Status: done.** `OwnedBuf` and `ProcessStack` (`scheduler.rs`),
`Reclaim` (its fields are RAII owners), and `Envelope`
(`Drop` + `free_transport(self)` defuse in `wire.rs`) all reclaim by
drop now. `Timer.msg_buf`'s hand-free is gone — the timer payload is an
`OwnedBuf` on the table's heap entry, freed on drop (lazy cancellation,
no second free site). The remaining hand-free is the deliberate
ownership transfer on delivered receive (`free_transport` consumes the
`Envelope`; nested heap moves to the receiver). Note the one leak still
open is _nested_ heap inside discarded payloads. The recursive drop
glue that frees it has since landed in the compiler (per-type `drop_T`,
value-semantics RC — see `MEMORY-MODEL.md`), but it is **not yet wired
into message passing**: `Envelope::new` still sets `drop_glue: None`
([wire.rs](koja/crates/koja-runtime/src/wire.rs)), so the discard path
has no glue to run. The remaining work is codegen stamping the message
type's `drop_glue_symbol` into the envelope at the `send` site — not a
missing `Drop`.

### 2. Two allocators for the same logical types

**Severity: high. Bug class: undefined behavior; blocks recursive drop glue.**

Heap payloads are allocated through **both** `std::alloc` and libc
`malloc`, sometimes for the same logical `String`/`Binary`:

- `std::alloc::alloc`: `alloc_koja_string` (`util.rs`), envelope buffers
  (`scheduler.rs`).
- libc `malloc`: `alloc_binary` (`util.rs`), `__koja_concat_bits`
  (`intrinsics.rs`), `koja_socket_recv_from` / `koja_socket_resolve`
  (`socket.rs`).

Freeing a `std::alloc` block with `free()` (or vice-versa) is undefined
behavior. It works today only because codegen's `payload - 8` free
recipe happens to match the allocator used. It also actively **blocked
recursive drop glue**: drop glue needs to free a message's nested
`String`s, which may be `malloc`'d while the envelope is `std::alloc`'d.

**Fix.** Pick one allocator (the global one) for all Koja heap. Funnel
String/Binary/Bits allocation through a single `alloc`/`free` pair so
drop glue can be written once and can't cross allocators.

**Status: done.** [`memory.rs`](koja/crates/koja-runtime/src/memory.rs)
is the single funnel: codegen calls `koja_alloc`/`koja_realloc`/
`koja_free`, runtime Rust calls the `pub(crate)` `alloc`/`realloc`/
`free` helpers, and both bottom out in one libc `malloc`/`free` pair. A
documented passthrough invariant keeps it interop-safe for
`CPtr`/`CString` and user `extern fn malloc/free`. Frees are sizeless
(matching codegen's `free(payload - 8)`), so drop glue can free a
message's nested heap without ever crossing allocators — unblocking
phase 6.

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

**Status: done.** [`process_table.rs`](koja/crates/koja-runtime/src/process_table.rs)
implements `ProcessTable`: a generational slotmap (PID packs
`(generation << 32) | index`, generation starting at 1), a single
bounds- and generation-checked `get`/`get_mut`, a `ready` `VecDeque`,
and timer + deadline `BinaryHeap` min-heaps. All ~14 PID sites in
`scheduler.rs`/`reactor.rs` route through `get`/`get_mut`/`transition`
(the `pid - 1` indexing and the #6 index-panic surface are gone), the
`transition` chokepoint moved here so it maintains the ready queue and
O(1) `alive`/`active` counts, and `worker_loop` now pops `next_runnable`

- due timers/deadlines instead of the ~6 linear scans. Slots are freed
  and recycled on death (bounded growth), with the generation bump making
  a stale `Ref` to a recycled slot report `ProcessDown` (preserved, now
  reuse-safe). Timer cancellation is lazy (validated on fire). Verified by
  `process_table.rs` unit tests, the spawn-and-die churn phase in
  [scheduler_stress.rs](koja/crates/koja-runtime/tests/scheduler_stress.rs),
  the full lang + stdlib suites, and `just tsan`.

* **#7 de-risked.** Live packed PIDs are `>= 2^32`, far above the
  reactor's `WATCH_KEY_OFFSET = 1_000_000`, so the keyspace collision is
  practically unreachable in the interim. The typed-`EventKey` fix (#7)
  is left as a separate follow-up.
* **TSan + churn caveat.** Concurrent, rapid TSan fiber _reuse_ across
  worker threads trips TSan's own cooperative-fiber bookkeeping (a
  non-deterministic SEGV inside `__tsan_func_entry`, not a race in our
  code — same asm-context-switch fragility as #4). `just tsan` therefore
  runs the heavy ping-pong concurrency soak with churn disabled
  (`KOJA_STRESS_WAVES=0`); the wide spawn-and-die reuse churn is exercised
  for correctness under the normal debug build instead. To keep TSan's
  fiber count bounded, fibers are now bound to slots (created once per
  slot, reused across occupants) rather than created/destroyed per
  process — see [tsan.rs](koja/crates/koja-runtime/src/tsan.rs).

#### PID design: generational index (Erlang-style), not a monotonic counter

The current scheme is monotonic `i64` with `idx = pid - 1` as a direct
index. It is, perhaps surprisingly, **stale-safe today**: because a PID
is never reused, a stale `Ref` always maps back to its original (now
`Dead`) slot and correctly reports `ProcessDown`. The only problem is
that the table never shrinks (this entry's growth + O(N) scans).

The catch: **slot reuse and generations are the same decision.** The
moment the table reuses a slot to bound memory, a monotonic/plain index
reintroduces an ABA hazard — a new process inheriting a dead one's slot,
so an old `Ref` misdelivers to the wrong process. Erlang's pids avoid
exactly this. Despite _looking_ random in the shell (`<0.123.0>`), an
Erlang pid is a structured opaque handle `{node, index, serial,
creation}`; the `serial`/`creation` are the generation counters that
make reuse safe. The substantive properties we want are **opacity** (a
PID is a handle, not array arithmetic) and **reuse-safety**, not
randomness.

Concrete design — pack a generation into the existing 64-bit PID:

- low 32 bits = **slot index** into the `ProcessTable`
- high 32 bits = **generation**, bumped each time the slot is recycled

`send` / `is_alive` / etc. decode `idx = pid & 0xFFFF_FFFF`,
`gen = pid >> 32`, and check `table[idx].generation == gen` before
touching the process; a mismatch is a stale handle → `ProcessDown`.
Properties:

- **O(1) lookup preserved** — still an array index, just validated.
- **Reuse-safe** — stale `Ref`s can never alias a recycled slot.
- **Opaque** — `pid - 1` arithmetic stops being meaningful; all access
  goes through `table.get(pid)`, which removes the index-panic surface
  in #6.
- **ABI-transparent** — a PID stays an `i64`. A `Ref<M, R>` is just a
  struct whose field 0 is that `i64` (`build_insert_value(.., 0)` in
  [intrinsics/process.rs](koja/crates/koja-ir-llvm/src/intrinsics/process.rs)),
  and every `koja_rt_*` call already passes `pid: i64`. So this is a
  **pure runtime refactor — no codegen or `Ref` layout change.**

This is essentially what modern ERTS does, minus distribution.

**Optional "random-looking" PIDs** (only if desired):

- _Cosmetic:_ XOR / lightweight-encrypt the packed `(index | generation)`
  with a per-runtime-instance random key on the way out and reverse it on
  the way in. PIDs look random; decode stays O(1); ABI stays `i64`.
- _Unguessable:_ generate a 63-bit random PID with a
  `HashMap<Pid, slot>`. Adds genuine unforgeability (relevant only if
  PIDs ever cross a trust boundary, e.g. distributed Koja over a wire),
  at the cost of trading the array index for a hash lookup plus
  (astronomically rare) collision handling.

The generation counter is the core mechanism; "random PIDs" are an
optional layer on top, not a substitute for it.

### 4. Safety-critical ordering lives in prose, not types (the race surface)

**Severity: high. Bug class: nondeterministic crashes / hangs.**

Two correctness invariants are correct-by-comment only:

- The `on_cpu` flag + "publish `Blocked` before the context switch saves
  `sp`" dance (`Process` doc in `scheduler.rs`). A pickup that ignores
  `on_cpu` resumes a stale frame.
- `io_block` **must** set `WaitingIo` _before_ `register` — otherwise the
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

**Status (steps 1-2 done).**

- _Transition guards (step 1)._ Every `ProcessState` write now goes
  through `Process::transition`, which `debug_assert!`s the edge against
  `is_legal_transition` (`scheduler.rs`). All 12 audited sites in
  `scheduler.rs` / `reactor.rs` were routed through it; the existing
  per-site preconditions mean no legal edge is a self-edge. The full lang
  suite and stdlib tests run with the guards active (debug runtime) and
  stay silent.
- _ThreadSanitizer (step 2)._ A multi-worker stress harness lives at
  `crates/koja-runtime/tests/scheduler_stress.rs`: a controller process
  ping-pongs one-byte messages with N children for R rounds, driving the
  context switch and mailbox handoff across real worker threads. Run it
  under TSan with `just tsan` (nightly + `-Zbuild-std`; see
  `INSTALLING.md`).

  TSan cannot follow `koja_context_switch` on its own — the hand-written
  assembly stack swap faults its shadow stack (`DEADLYSIGNAL`) the first
  time a process yields mid-function. This is resolved by modelling each
  process and each worker's scheduler context as a TSan **fiber** and
  announcing every switch via the `__tsan_*_fiber` API (`tsan.rs`). The
  annotations are gated on the `koja_tsan` cfg (set only by `just tsan`),
  so normal and release builds are byte-for-byte unaffected. With them in
  place a 16-child / 2000-round soak (~32k cross-worker handoffs) reports
  **no data races**.

- _Remaining (step 3)._ The TSan harness only exercises the
  spawn/send/receive path; it does not yet cover `kill`, timers, or I/O
  readiness, and TSan still cannot see the asm stack swap itself (only the
  Rust state around it). `loom` remains the path to exhaustive
  interleaving coverage of the switch/handoff.

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

**Status: done.** A process-global panic hook
([`panic::install_panic_hook`](koja/crates/koja-runtime/src/panic.rs))
now routes every Rust panic — on any thread, from any site — through the
same diagnostic-and-abort path as user panics. It runs _before_
unwinding with the stack intact, so the backtrace is faithful and no
unwind ever reaches a C-ABI frame or poisons `SCHED`: the first panic
aborts immediately, which also kills the old worker-cascade failure
mode (one worker panicking, poisoning the lock, and the rest dying on
`SCHED.lock().unwrap()`). The hook is installed exactly once via a
`std::sync::Once` (`ensure_runtime_init`) at the head of `koja_rt_spawn`
and `koja_rt_main_done` — `koja_rt_spawn` is the first runtime call a
program makes, so the hook is live before any worker thread or lock
exists. The shared `abort_with_diagnostic(origin, msg)` takes a
`PanicOrigin` so runtime panics keep `koja_rt_*` / `koja_runtime::`
frames (the stack worth seeing) while user panics still surface only
user code. Targeted source fixes: `fill_random` retries `EINTR` instead
of asserting on the first hiccup, and `cstr_str` decodes with
`from_utf8_lossy` (no panic on malformed bytes). Remaining
`expect`/`debug_assert`/`Layout` sites stay as invariants — they now
abort cleanly with a diagnostic via the hook. Per-entry `catch_unwind`
shims were intentionally skipped: the global hook covers every site at
zero per-call cost, so the shims would add boilerplate without
additional safety.

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

## The big four (done)

The four highest-leverage fixes have all landed:

1. **RAII the resources** — `Drop` everywhere + `free_transport` defuse
   for the delivered-receive transfer. (Entry #1.) ✅
2. **One allocator for all Koja heap** — the `memory.rs` funnel; removes
   the cross-allocator UB hazard and unblocks phase 6. (Entry #2.) ✅
3. **A `ProcessTable` abstraction** — generational PIDs + ready queue +
   timer/deadline heaps + bounds-checked accessor. Removes unbounded
   growth, the O(N) scans, scattered `pid - 1` indexing, and de-risks the
   keyspace collision. (Entries #3, #7.) ✅
4. **ThreadSanitizer + transition guards** — every state change funnels
   through `ProcessTable::transition` with a `debug_assert!` edge check,
   and `just tsan` runs a fiber-annotated race soak. (Entry #4; `loom`
   remains as step 3.) ✅

## What's left for soft launch

- **Wire `Envelope.drop_glue` at the send site** — the recursive drop
  glue itself landed in the compiler (per-type `drop_T`, value-semantics
  RC; see `MEMORY-MODEL.md`), but codegen does not yet stamp the message
  type's glue pointer into the envelope, so `drop_glue` is always null
  ([wire.rs](koja/crates/koja-runtime/src/wire.rs)) and nested heap
  inside an undelivered/discarded payload leaks. This is the last
  message-lifecycle leak (§1).
- **#5 (reactor strands a worker)** and **#9 (messages don't wake
  `WaitingIo`)** — liveness / shutdown semantics.
- Deferred: **#7** (typed `EventKey`; de-risked by generational PIDs),
  **#8** (remaining `malloc` null checks), **#4 step 3** (`loom`).

**#6 (panics across the C-ABI)** is now done — see its section: a global
panic hook converts every panic into a clean diagnostic abort before any
unwind reaches a C frame.
