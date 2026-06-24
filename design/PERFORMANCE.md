# Performance strategy

A standing strategy document for Koja's runtime and codegen performance —
not a plan and not a backlog. It records _where Koja structurally stands
relative to the BEAM_, the throughline that should guide tuning, and a
tiered menu of levers with their prior art. Pull individual levers into
their own plans as they get tackled; prune entries here once they land or
stop being true.

See also: [ROADMAP.md](ROADMAP.md) (milestones), [RUNTIME-GAPS.md](RUNTIME-GAPS.md)
(correctness/structural smells), [MEMORY-MODEL.md](MEMORY-MODEL.md) (RC + COW),
[SCHEDULER-PROTOCOL.md](SCHEDULER-PROTOCOL.md) (executor/reactor/driver).

## The throughline

> The BEAM spent thirty years working _around_ being a bytecode
> interpreter with immutable data and per-process tracing GC. Koja starts
> where that climb was headed: **ahead-of-time native code through LLVM.**

So the strategy is not "copy the BEAM." It is two-pronged:

1. **Pay the taxes the BEAM also pays, but more cheaply** — reduction
   counting, message copying, allocation, RC bookkeeping. These are the
   places Koja is currently _behind_, because the BEAM has tuned them for
   decades and Koja's first cut is naive.
2. **Exploit what a bytecode VM structurally cannot do** — whole-program
   optimization, profile-guided optimization, post-link layout,
   devirtualization. These are Koja's moat; the BEAM cannot follow.

Every lever below is tagged with which prong it serves. When a microbenchmark
regresses (e.g. `fib` after function-entry preemption landed), the question to
ask first is "is this a tax we're paying naively?" — prong 1 — before reaching
for anything exotic.

## Measured baseline

Microbenchmarks, one machine (Apple Silicon / darwin), Koja `build --release`
(LLVM -O3) vs Erlang/OTP BeamAsm. Internal timing excludes VM startup and
compile. Captured via `just bench`; lower is better.

| Workload                         |    Koja |   BEAM | Koja ÷ BEAM throughput |
| -------------------------------- | ------: | -----: | ---------------------: |
| Tight loop (200M iters)          |  223 ms | 330 ms |      **1.48× (ahead)** |
| `fib(35)` (29.9M non-tail calls) |   80 ms |  54 ms |                  0.68× |
| Msg round-trip (1M)              | 2069 ms | 494 ms |                  0.24× |
| Spawn + reply (100k)             |  393 ms | 106 ms |                  0.27× |
| 10k process storm                | 1428 ms | 168 ms |                  0.12× |

Read these honestly: they are degenerate microbenchmarks (an empty 200M-iter
loop and naive `fib` are not real workloads), they cover only the compiled
backend, and the messaging comparison is conservative for Koja (its `.call`
does timeout + reply-correlation work the raw Erlang `!`/`receive` baseline
skips). The shape of the gap is what matters: **compute is a real fight,
fine-grained concurrency is where the BEAM still leads.**

The `fib` line is the canonical prong-1 story: it was ~1.9× _ahead_ before
function-entry preemption landed, because it had zero yield-checks. It now
runs a reduction decrement on every one of ~30M calls — a tax the BEAM has
always paid (and folds into dispatch cheaply) that Koja only just adopted.
Tier 1 below is largely about making that tax cheap.

---

## Tier 1 — make the runtime taxes cheap (prong 1, highest near-term leverage)

### Reduction-counting overhead

**Leverage: high. Effort: medium. Prior art: BEAM register pinning, BEAM per-op reduction cost.**

The per-`YieldCheck` decrement is an inline `load / sub / store / icmp / branch`
on the `koja_reductions_left` thread-local (`reductions.c`, lowered in
`emit_yield_check`). On darwin the TLS access does not relax to local-exec —
it goes through a TLV thunk per check — so the "inline" path still pays a
thunk on every check. Two proven moves:

- **Register-pin the counter.** The BEAM keeps core VM state in reserved
  machine registers. Pinning the current process's reduction count (and likely
  the current-process pointer) in a reserved register via an LLVM global
  register variable / a reserved-register calling convention turns each check
  into register arithmetic and deletes the TLV thunk. This alone likely
  recovers most of the `fib` regression.
- **Amortize the decrement.** The BEAM assigns each operation a static
  reduction cost and decrements in bulk, not 1-per-call. Have the compiler
  compute a per-region weight and decrement once per straight-line region
  instead of once per call/back-edge. Fewer hot-path ops at equal fairness.

### Message-passing copy cost

**Leverage: high (closes most of msg/spawn gap). Effort: medium-high. Prior art: BEAM refc binaries. Gated on: the `Shared` ARC-style type.**

Koja deep-copies every payload at the process boundary (`DeepCopy` +
`deep_copy_T` glue) so intra-process RC stays non-atomic. The BEAM copies
small terms but **reference-counts binaries above ~64 bytes off-heap and
shares them**. Koja's COW + RC model is most of the way to the same thing:
for large immutable payloads, transfer a shared-immutable RC'd block instead
of copying — O(1) instead of O(size). The cost is that cross-process sharing
forces those blocks' RC to be atomic; scope it to large payloads so the common
small-message path keeps the non-atomic fast path.

The clean vehicle for that atomic boundary is the **`Shared` ARC-style type**
— the deeply-immutable, atomically-reference-counted value from the original
EXPOIR design ([ROADMAP.md](ROADMAP.md): the Phase 5 "atomic-refcount sharing
for deeply-immutable values" optimization question, currently deferrable; also
[archive/20260427-EXPOIR.md](archive/20260427-EXPOIR.md)). A `Shared<T>` makes
"this value may be referenced across processes, account for it atomically" a
*typed, opt-in* property rather than a runtime guess, which keeps the
non-`Shared` fast path provably non-atomic. **This lever is effectively gated
on that type landing** — and the roadmap flags it as defer-if-complex, so treat
zero-copy messaging as downstream of that decision, not independent of it. If
`Shared` is deferred, the fallback stays the existing copy machinery (and
`shared_map`'s copy-in/copy-out), which is correct but O(size). Couples to
[MEMORY-MODEL.md](MEMORY-MODEL.md).

### Allocation + reference-counting bookkeeping

**Leverage: medium-high (dominant in real workloads). Effort: medium. Prior art: RC research the BEAM never needed.**

Koja chose RC, so mine the RC literature the BEAM's tracing GC let it ignore:

- **Coalesced / deferred RC** (Levanoni–Petrank): batch and cancel
  retain/release pairs rather than touching counts eagerly.
- **Biased RC** (Swift; _Biased Reference Counting_, PACT'18): a non-atomic
  fast path for the owning thread, atomic slow path only on cross-thread access.
  This is the _general_ form of what a `Shared` type does _selectively_; if the
  `Shared` ARC-style type lands, prefer leaning on that typed boundary over a
  blanket biased-RC scheme, and keep biased RC in reserve for the case where
  untyped cross-thread sharing turns out to be common.
- **An ARC-optimizer-style LLVM pass** to eliminate redundant `Clone`/`DropValue`
  pairs at codegen (Koja already emits acquire/drop glue; a peephole over it is
  low-risk).
- **Arena / bump allocation for process-local short-lived data**, bulk-reclaimed
  at process death — the BEAM's cheapest trick (per-process heaps freed
  wholesale).

---

## Tier 2 — exploit the AOT edge (prong 2, medium-term moat)

### Profile-guided + whole-program optimization

**Leverage: high, broad. Effort: medium (mostly build-pipeline work). Prior art: nothing in the BEAM — structurally impossible there.**

LTO across packages, **PGO** (collect branch/reduction profiles, feed them
back), and post-link layout (**BOLT**). A JIT approximates PGO at runtime; an
AOT compiler bakes it in for free. This is the single biggest thing the BEAM
cannot answer.

### Protocol-dispatch devirtualization

**Leverage: medium. Effort: medium. Prior art: standard for AOT trait/interface langs.**

Turn dynamic protocol dispatch into static / inlined calls where the call
graph permits (monomorphization is already done for generics in `koja-ir`;
extend the analysis to protocol receivers). Removes an indirect call and
unlocks inlining on hot dispatch sites.

### Move inference at the process boundary

**Leverage: high (eats most real sends; partially decouples the concurrency win from `Shared`). Effort: medium-high. Prior art: Pony `iso`/`consume`, Rust move + `Send`.**

Value semantics already make copy-vs-move-vs-share an *implementation detail*:
the spec says every binding is an independent value, so the backend is free to
pick the cheapest sound lowering for a boundary `DeepCopy`, with the copy as the
always-correct fallback. That licenses a pure optimization — no behavior change.

The high-value, locally-provable case is **move-on-last-use**: if the sender
provably never touches a sent value again *and* its heap subgraph isn't
reachable from anything the sender keeps live, the boundary copy becomes a
**move** — hand the block to the receiver, zero copy, and crucially *no atomic
RC*, because ownership transfers wholesale and there's still exactly one owner.
This is the strongest outcome on every axis and covers the dominant idioms
(`spawn Foo.start(config)` with a locally-built `config`; a `send` of a
freshly-constructed message). It is the same analysis Koja already runs
*intra-process* — the owned-temporary discipline where construction results are
moved, not cloned (see the throughline in [RUNTIME-GAPS.md](RUNTIME-GAPS.md)) —
generalized from the call boundary to the send boundary.

Why this is move inference and **not** auto-sharing: promoting a *kept* or
aliased value to a shared block runs into representation coherence — RC
atomicity is a property of the heap block, fixed at allocation, and one count
field can't be non-atomic for the sender's references and atomic for the
receiver's. That global rewrite is exactly what the typed `Shared` boundary
exists to avoid. So the division of labor is: **the compiler infers moves
automatically; `Shared` stays the explicit type for the "kept and shared" case
a local proof can't reach.** Move inference is the partial-decoupling that takes
schedule pressure off `Shared` — it captures the common send-and-forget traffic
without waiting on that type to land. (COW also means the share case never has a
mutation hazard — any write forks — so the only true blocker for sharing is the
representation, not aliased mutation.) Conservative is fine: fire only where
non-escape is cheaply provable and copy otherwise. Couples to
[MEMORY-MODEL.md](MEMORY-MODEL.md).

### Dirty-scheduler equivalent for long native calls

**Leverage: medium (fairness + throughput under FFI). Effort: medium. Prior art: BEAM dirty schedulers (OTP ~17/20).**

A CPU-bound `@extern` / FFI call currently occupies a worker with no yield
point. A dedicated pool for long native calls (or yield-around-FFI hooks)
keeps the main workers responsive. Couples to [FFI.md](FFI.md) and the
reactor model in [SCHEDULER-PROTOCOL.md](SCHEDULER-PROTOCOL.md).

---

## Tier 3 — the frontier (prong 2, long-term architecture bets)

### Signal-based asynchronous preemption — the marquee bet

**Leverage: high (removes yield-checks from the hot path entirely). Effort: high. Prior art: Go 1.14 (2020).**

This is the natural successor to the function-entry preemption that landed in
Gap #3, and the most important long-term idea in this document.

Go used to preempt cooperatively at **function prologues** — the _exact_
approach Koja ships today — and hit the _exact_ two problems Koja now has:
tight loops with no calls can't be preempted, and the prologue checks cost. In
1.14 Go moved to **signal-based async preemption**: the scheduler signals a
worker, the handler parks the process at a compiler-registered safepoint, and
the hot path carries _no checks at all._

Koja built the "Go 1.13" version. The "Go 1.14" upgrade would:

- keep compiler-inserted `YieldCheck`s as the portable fallback (and the only
  mechanism on the cooperative interpreter and WASM),
- add signal-based preemption on platforms that support it, and
- **delete the checks from the native hot path** — reclaiming both `fib` and
  the per-iteration tight-loop overhead, while _strengthening_ fairness (even
  check-free loops become preemptible).

It makes the entire reduction-counting-tax question in Tier 1 mostly disappear
on the native backend. The Tier 1 reduction-counter fixes are the cheap interim
win; this is the structural endgame.

### Smaller frontier levers

- **NUMA-aware run queues** and smarter steal heuristics (victim selection,
  steal batching) as core counts grow.
- **Value representation**: pointer tagging / NaN-boxing to avoid boxing small
  scalars; couples to [ABI.md](ABI.md) and [TYPES.md](TYPES.md).
- **SIMD bit/binary ops**: the BEAM interprets its bit syntax; LLVM
  auto-vectorization (or hand-written intrinsics) can make `Binary`/`Bits`
  operations a Koja strength rather than parity.

---

## Sequencing & discipline

Suggested order, cheapest-validating-first:

1. **Tier 1 reduction-counter fixes** — recovers today's `fib` regression,
   small, and exercises the measurement loop.
2. **PGO / LTO** — broad win, the structural moat.
3. **Move inference at the process boundary** — `Shared`-independent, captures
   the common send-and-forget traffic, and takes schedule pressure off the
   `Shared` type.
4. **Message zero-copy for kept/aliased payloads** — closes the rest of the
   concurrency gap; gated on the `Shared` ARC-style type.
5. **Signal-based async preemption** — the architecture bet that retires the
   yield-check tax for good.

Discipline: you cannot tune what you do not measure. `just bench` and the
runtime's self-reporting counters (`ScheduleCounters`, the lifecycle ring,
`koja_rt_sched_violations`) are the seed; the standing goal is per-release
regression tracking against _representative_ workloads, not just the degenerate
microbenchmarks above. Every lever in this document should be justified by a
before/after number, not a hunch.
