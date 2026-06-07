# Runtime gaps & architecture smells

A triage of structural problems in `koja-runtime`. This is a standing
audit document, not a plan: each entry records a smell, where it lives,
the bug class it produces, and the shape of the fix. Completed gaps are
deleted, so everything below is still open. Pull individual entries into
their own plans as they get tackled.

## The throughline

Almost every runtime bug we've chased — the message-envelope leaks
(`archive/20260529-MESSAGE-LIFECYCLE.md`), the `on_cpu` scheduler race,
the nondeterministic SIGBUS in tight `call` loops — traces to one root:

> **The runtime manages raw memory and process state by hand, with
> ownership and ordering rules encoded in comments instead of enforced
> by types.**

That is the smell to attack. The highest-leverage fixes have already
landed — RAII (`Drop` + a `free_transport` defuse for the
delivered-receive transfer), a single allocator (`memory.rs`), a
generational `ProcessTable` (bounded growth, ready queue, timer/deadline
heaps, bounds-checked access), envelope `drop_glue` wired at the send
site (undelivered payloads reclaim their nested heap), the
close-while-blocked reactor wake, and a global panic hook plus
ThreadSanitizer with transition guards. Each converted a class of
"correct by careful review" into "correct by construction" or "caught by
CI." The entries below are what remains.

---

## Open gaps

### 1. Owned heap temporaries passed as arguments (and construction results) are never dropped

**Severity: high. Bug class: leaks (unbounded for long-running programs).**

Value semantics make every parameter an independent value: a callee
clones (rc++) its heap-backed parameters on entry. But codegen does not
treat the *caller's* owned heap temporary as something to drop after the
call, and IR lowering does not mark construction results (`Struct{...}`,
enum constructors) as `owned`. Two concrete leaks fall out:

- **Owned temp as argument.** `sink(a <> b)` — the `a <> b`
  concatenation is a fresh owned `String`. It is passed to `sink`, which
  clones it on entry; the caller never drops its temporary, so the
  original allocation leaks once per call.
- **Construction result.** `m = Msg{text: base}` — the constructed `Msg`
  is not flagged `owned`, so `materialize_owned` *clones* it into `m`
  instead of moving it, and the un-owned construction temporary (and its
  `String` field) is never dropped.

Memory-safe — no use-after-free or double-free, because the leak is a
missing `rc--`, not an extra one — but it is **unbounded growth** on
extremely common patterns (`f(a <> b)`, `f(Struct{...})`), so a
long-running server bleeds memory.

**Where it lives.** This is a koja-ir/codegen ownership-model gap, not a
`koja-runtime` smell — tracked here because it is the remaining
unbounded-memory leak, surfaced right after the message-payload
reclamation work landed. It lives in `lower/calls.rs` (`emit_call`
doesn't drop owned heap argument temporaries after a regular call) and
the value-ownership tracking that decides whether a
`StructInit`/constructor result is `owned` (`lower/ownership.rs`).
Surfaced via LLVM IR inspection of minimal repros (`f(a <> b)`,
`m = Struct{...}`, `r.cast(Struct{...})`).

**Fix.** A core ownership-model change in lowering: (1) mark
construction results (`StructInit`, enum constructors) `owned` so
`materialize_owned` moves rather than clones them; (2) in `emit_call`,
drop owned heap-backed argument temporaries after the call returns (the
callee took its own clone); (3) ensure the message-send intrinsics
(`Ref.cast`/`call`/`send_after`, `ReplyTo.send`) **opt out** of that
post-call drop — they already `materialize_owned` the payload and
transfer the owned copy to the transport, so dropping it after the call
would double-free.

**Status: open (deferred).** Found while wiring message-payload
reclamation; intentionally scoped out of the Tier-1 message-lifecycle
work to keep that change reviewable. This is the next memory-lifecycle
item to land before a wide launch.

### 2. No exhaustive interleaving coverage of the context switch

**Severity: medium. Bug class: nondeterministic crashes / hangs.**

Two scheduler invariants are correct-by-comment only: the `on_cpu` flag
+ "publish `Blocked` before the context switch saves `sp`" dance
(`Process` doc in `scheduler.rs`), and `io_block` setting `WaitingIo`
_before_ `register` (`reactor.rs`). Both are now guarded at runtime —
every `ProcessState` write funnels through `ProcessTable::transition`
with a `debug_assert!` edge check, and `just tsan` runs a
fiber-annotated, multi-worker ping-pong soak (`scheduler_stress.rs`)
that reports no data races over ~32k cross-worker handoffs.

What's missing is *exhaustive* coverage. The TSan harness only exercises
spawn/send/receive — not `kill`, timers, or I/O readiness — and TSan
cannot follow the hand-written asm stack swap itself
(`koja_context_switch` faults its shadow stack), only the Rust state
around it.

**Fix.** `loom` for exhaustive interleaving tests of the switch/handoff,
covering the paths the TSan soak doesn't.

### 3. Two keyspaces multiplexed into one integer

**Severity: low (was medium; de-risked by generational PIDs). Bug class: misrouted I/O events.**

The reactor distinguishes I/O-block events (key = `pid`) from watch
events (key = `fd + WATCH_KEY_OFFSET`, offset `1_000_000`) by arithmetic
(`reactor.rs`). Multiplexing two keyspaces into one integer by offset is
fragile by construction.

**De-risked.** Now that PIDs are generational (`(generation << 32) |
index`), live packed PIDs are `>= 2^32` — far above `WATCH_KEY_OFFSET` —
so the collision is practically unreachable. This is a robustness
cleanup, not an active bug.

**Fix.** Fold both into a typed `enum EventKey { Process(Pid),
Watch(Fd) }` resolved through a table, rather than an integer offset.

### 4. Messages to an I/O-blocked process don't wake it

**Severity: low. Bug class: delivery latency / missed signals.**

`koja_rt_send`, `send_lifecycle_to`, and the timer-fire path only
promote `state == Blocked`, never `WaitingIo`. A lifecycle message
(e.g. `SIGTERM`) sent to a process parked in `accept()` isn't seen until
its I/O happens to complete.

**Fix.** Decide the intended semantics (does a message interrupt an I/O
wait?). If yes, promote `WaitingIo → Runnable` on message arrival and
have the resumed process re-check its mailbox before re-blocking on I/O.

### 5. `malloc` results unchecked in several places

**Severity: low. Bug class: null-deref on OOM.**

`intrinsics.rs` checks `malloc` and aborts on null; `socket.rs`
(`recv_from`, `resolve`) and `util.rs` (`build_argv`) write straight
through without a check. Inconsistent.

**Fix.** A single `xmalloc`-style helper that aborts on null, used
everywhere — `memory.rs` is the single allocation funnel now, so that's
its natural home.

---

## Launch priority

Only **#1** (owned heap temporaries / construction results never
dropped) is a launch blocker: it's the one open memory-safety-adjacent
leak that grows unbounded on everyday code. The rest are
robustness/coverage cleanups — **#2** (`loom`), **#3** (typed
`EventKey`), **#4** (wake `WaitingIo` on message), **#5** (`malloc` null
checks) — and can land after a soft launch.
