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

- `deep_copy_T` glue; payloads never alias sender heap, so intra-process
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

- "publish `Blocked` before the context switch saves `sp`" dance
  (`Process` doc in `scheduler.rs`), and `io_block` setting `WaitingIo`
  _before_ `register` (`reactor.rs`). Both are now guarded at runtime —
  every `ProcessState` write funnels through `ProcessTable::transition`
  with a `debug_assert!` edge check, and `just tsan` runs a
  fiber-annotated, multi-worker ping-pong soak (`scheduler_stress.rs`)
  that reports no data races over ~32k cross-worker handoffs.

The runtime is also self-reporting now: `ProcessTable` keeps invariant
counters (`ScheduleCounters`) and a lifecycle event ring, bumped at the
policy chokepoints while the lock is already held. Illegal edges are
_counted_ in every build — not just debug-asserted — and exposed via
`koja_rt_sched_violations`, so the `tests/lang/memory/kill_park_race`
fixture asserts race-correctness on the real release runtime (asm
switch included) in every CI run. `koja_rt_parks_refused` gives the
fixture's storms positive coverage evidence (the kill-vs-park window —
the interleaving that actually shipped — is hit dozens of times per
run, visibly refused), and `KOJA_SCHED_TRACE=1` dumps the event ring at
shutdown so a failing run's interleaving can be read directly.

What's missing is _exhaustive_ coverage. TSan only exercises
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

### 3. Preemption points cover only loops and tail calls

**Severity: medium. Bug class: scheduler starvation / unfair preemption.**

`YieldCheck` is inserted at exactly two sites: loop back-edges and
immediately before a `TailCall` (`yield_checks.rs::insert_in_body`).
Nothing sits at function entry or ordinary call sites, so a process
that burns CPU without crossing either site never yields. The unguarded
shapes are **deep non-tail recursion** (each frame is a plain `Call`, no
back-edge) and **long straight-line blocks** (no branch, so no
dominator analysis and no check). A `fib(35)` ran to completion with
zero yield-checks — it didn't preempt, it monopolized the worker for its
whole run, and on the single-threaded cooperative interpreter that shape
can starve a peer indefinitely. Both backends are affected.

In practice most long-running work is loops or tail recursion, which are
covered — hence medium, not high — but the hole is real and is a
correctness/fairness gap, not just a perf one.

**Fix.** Add a preemption point that doesn't depend on control-flow
shape: a check on function entry (and/or a bounded-instruction-count
trip for pathological straight-line blocks). That multiplies how often
`YieldCheck` runs, so pair it with an **inline fast-path lowering** —
decrement a per-process reduction counter inline and call
`koja_rt_yield_check` only on exhaustion, instead of an unconditional
runtime call per check (the standard BEAM reduction trick). The inline
path is the enabler that makes denser placement cheap; doing it for
placement-completeness rather than to win a microbenchmark is the right
ordering. Couples to where the reduction counter lives, so it wants to
land after the scheduler/reduction model settles.

---

## Launch priority

No open entry is a launch blocker. With the owned-temporary /
construction leak now fixed, the unbounded-memory class is closed, and
everything that remains is a robustness/coverage/fairness cleanup —
**#1** (`loom`), **#2** (typed `EventKey`), and **#3** (preemption-point
completeness) — that can land after a soft launch. **#3** is the one
genuine correctness gap of the three (a CPU-bound non-tail-recursive
process can starve peers), but it's bounded to code shapes that avoid
loops and tail calls, so it doesn't block a soft launch.
