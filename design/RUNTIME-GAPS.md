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
close-while-blocked reactor wake, the owned-temporary / construction
drop discipline in IR lowering (callers release heap temps they pass to
a clone-on-entry callee; construction results are `owned` and moved, not
cloned), deep-copy at every process boundary (`IRInstruction::DeepCopy`
+ `deep_copy_T` glue; payloads never alias sender heap, so intra-process
rc stays non-atomic), a unified `OwnedPayload` RAII owner across
envelopes / timers / spawn configs, the two-queue mailbox with a tokened
one-shot reply slot (stale replies are discarded by correlation, not
delivered to the next call), the kill-tombstone policy owned by
`ProcessTable` (`try_park` / `try_park_io` atomically refuse when a
cross-worker kill already marked the process `Dead`, and
`mark_dead_if_alive` makes the death mark idempotent — a new park site
cannot reintroduce the park-over-tombstone race), and a global panic
hook plus ThreadSanitizer with transition guards.
Each converted a class of "correct by careful review" into "correct by
construction" or "caught by CI." The `tests/lang/memory/` fixtures pin
the payload-reclaim behavior with `koja_rt_live_blocks` steady-state
checks. The entries below are what remains.

---

## Open gaps

### 1. No exhaustive interleaving coverage of the context switch

**Severity: medium. Bug class: nondeterministic crashes / hangs.**

Two scheduler invariants are correct-by-comment only: the `on_cpu` flag
+ "publish `Blocked` before the context switch saves `sp`" dance
(`Process` doc in `scheduler.rs`), and `io_block` setting `WaitingIo`
_before_ `register` (`reactor.rs`). Both are now guarded at runtime —
every `ProcessState` write funnels through `ProcessTable::transition`
with a `debug_assert!` edge check, and `just tsan` runs a
fiber-annotated, multi-worker ping-pong soak (`scheduler_stress.rs`)
that reports no data races over ~32k cross-worker handoffs.

The runtime is also self-reporting now: `ProcessTable` keeps invariant
counters (`ScheduleCounters`) and a lifecycle event ring, bumped at the
policy chokepoints while the lock is already held. Illegal edges are
*counted* in every build — not just debug-asserted — and exposed via
`koja_rt_sched_violations`, so the `tests/lang/memory/kill_park_race`
fixture asserts race-correctness on the real release runtime (asm
switch included) in every CI run. `koja_rt_parks_refused` gives the
fixture's storms positive coverage evidence (the kill-vs-park window —
the interleaving that actually shipped — is hit dozens of times per
run, visibly refused), and `KOJA_SCHED_TRACE=1` dumps the event ring at
shutdown so a failing run's interleaving can be read directly.

What's missing is *exhaustive* coverage. TSan only exercises
spawn/send/receive — not `kill`, timers, or I/O readiness — and cannot
follow the hand-written asm stack swap itself (`koja_context_switch`
faults its shadow stack); the counters detect a bad interleaving only
when a run happens to produce one.

**Fix.** Seeded deterministic scheduling: drive `claim_next` pickup and
preemption decisions from a `KOJA_SCHED_SEED` PRNG so interleaving
soaks are replayable by seed (the counters above become the oracle).
Alternatively `loom` models of the `ProcessTable` protocols for true
exhaustiveness over a small state space.

### 2. Two keyspaces multiplexed into one integer

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

### 3. Messages to an I/O-blocked process don't wake it

**Severity: low. Bug class: delivery latency / missed signals.**

`koja_rt_send`, `send_lifecycle_to`, and the timer-fire path only
promote `state == Blocked`, never `WaitingIo`. A lifecycle message
(e.g. `SIGTERM`) sent to a process parked in `accept()` isn't seen until
its I/O happens to complete.

**Fix.** Decide the intended semantics (does a message interrupt an I/O
wait?). If yes, promote `WaitingIo → Runnable` on message arrival and
have the resumed process re-check its mailbox before re-blocking on I/O.

### 4. `malloc` results unchecked in several places

**Severity: low. Bug class: null-deref on OOM.**

`intrinsics.rs` checks `malloc` and aborts on null; `socket.rs`
(`recv_from`, `resolve`) and `util.rs` (`build_argv`) write straight
through without a check. Inconsistent.

**Fix.** A single `xmalloc`-style helper that aborts on null, used
everywhere — `memory.rs` is the single allocation funnel now, so that's
its natural home.

---

## Launch priority

No open entry is a launch blocker. With the owned-temporary /
construction leak now fixed, the unbounded-memory class is closed, and
everything that remains is a robustness/coverage cleanup — **#1**
(`loom`), **#2** (typed `EventKey`), **#3** (wake `WaitingIo` on
message), **#4** (`malloc` null checks) — that can land after a soft
launch.
