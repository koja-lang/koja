# Runtime Gaps and Architecture Smells

A standing audit of structural problems across `koja-runtime-core` and its
adapters. Each entry records a smell, where it lives, the bug class it
produces, and the shape of the fix. Completed gaps are deleted, so everything
below is still open. Pull individual entries into their own plans as they are
tackled.

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

### 2. Mailbox depth is not observable

**Severity: medium. Bug class: invisible overload and latency collapse.**

Each process mailbox stores system and business traffic in unbounded
`VecDeque`s. `cast`, timers, and I/O delivery keep accepting work while a
receiver is slow or blocked. This is intentional. Like the BEAM, Koja does not
apply automatic sender-side backpressure or impose a process mailbox capacity.
Application protocols, calls, pools, and demand-driven libraries own admission
control.

This is not an ownership leak: draining or killing the process reclaims
the envelopes correctly. It is demand-driven retention, so the existing
steady-state live-block tests do not catch it. A producer that outruns a
consumer can still grow RSS until the OS kills the program.

**Fix.** Implement the `Runtime` observability API tracked in
[ROADMAP.md](ROADMAP.md). Global and per-process snapshots must expose
mailbox depth without changing delivery semantics. At minimum, distinguish
system and business queue depth so operators can identify a slow consumer
without inspecting payloads.

Add a slow-consumer fixture that verifies depth snapshots and eventual
reclamation on both adapters.

### 3. Per-process mmap caps density and slows spawn

**Severity: medium. Bug class: scaling ceiling, slow spawn.**

Every spawn does an `mmap` plus an `mprotect`, and every death an
`munmap` (`allocate_process_stack` in `scheduler.rs`). This is the
correct-first design, but it carries two costs:

- **Spawn latency.** Two syscalls per spawn put the spawn benchmark at
  roughly 2.8x BEAM. The BEAM allocates process memory from large
  shared carriers with no syscall on the spawn path.
- **Density.** Each guarded stack is two VMAs (the `PROT_NONE` guard
  and the usable region). Linux caps VMAs at `vm.max_map_count`,
  65530 by default, so about 30k live processes before tuning. The
  BEAM runs millions of processes per node because thousands of
  process blocks share one carrier mapping. Matching that is part of
  the pitch to the Elixir audience.

The virtual reservation itself is not the constraint. 2M processes at
576 KiB reserve about 1.1 TB, which a 64-bit address space absorbs.
The VMA count is the binding limit.

**Fix.** In two steps, both compatible with fixed non-relocatable
stacks:

1. **Stack pooling.** Death returns the mapping to a free list, spawn
   pops one. Removes the syscalls from the hot path and most of the
   spawn gap. No change to the guard or fault handler.
2. **Carrier stacks with a prologue limit check.** Carve stacks from
   large shared mappings with no guard pages, and have the compiler
   emit a stack-limit compare at function entry, the Go model. The
   check can ride the existing `YieldCheck` entry sequence, and an
   overflow becomes a clean runtime kill with the same
   `** (stack overflow)` diagnostic. A software check cannot protect
   unprobed C frames, so FFI-heavy processes may keep guarded `mmap`
   stacks while plain processes use carrier stacks.

   The check also upgrades overflow from program-fatal to
   process-contained. A hardware guard fault cannot unwind (no stack
   to run cleanup on, and it may interrupt a lock-holding runtime
   call), so today the whole program dies and monitors never fire.
   A prologue check detects exhaustion synchronously with headroom
   left, so it can reuse the panic unwind path: the process dies with
   a stack overflow `StopReason` and monitors are notified like any
   other crash. The guard stays as the program-fatal backstop for
   unprobed C frames.

### 4. Collection buffers have no refcount, so unfused mutators copy

**Severity: medium. Bug class: throughput collapse on hot paths.**

`List`, `Map`, and `Set` buffers are deep-ownership with no reference
count, so every copy is O(n). The consume-fusion pass
(`koja-ir/src/elaborate/consume.rs`) makes `append` / `put` / `insert`
reuse the receiver's buffer when the receiver value provably dies at
the call site, which covers `x = x.append(y)` rebind loops and
discarded owned temps. Every mutator call outside those shapes still
clones the whole backing buffer. The fusion is the tactical fix for
the hottest pattern, not the memory-model endpoint.

**Fix.** Swift-style refcounted collection buffers with a uniqueness
check at every mutator. That needs a shared-buffer frontier scheme so
views with different lengths stay correct, and it generalizes later
with Perceus-style reuse analysis. The `Indirect` box refcount entry
in GAPS.md is the same "give it a refcount" move for recursive
structures. The two land together as the 0.18.0 memory-model
milestone that makes MEMORY-MODEL.md's "copied lazily only on
mutation" promise true everywhere.

---

## Launch priority

No open entry blocks an experimental soft launch. The ownership-leak
class is closed. **#1** (`loom`) is a robustness and coverage improvement.
**#2** requires the 0.16 observability surface, while unbounded delivery
remains the documented contract. **#3** step 1 (stack pooling) is the
next performance item, and step 2 gates the million-process story, not
the launch. **#4** is the 0.18.0 memory-model milestone. The one-time fairness gap (preemption
points covering only loops and tail calls, letting deep non-tail
recursion monopolize a worker) is now closed: a `YieldCheck` sits at the
entry of every call-containing function, lowered to an inline reduction
decrement.
