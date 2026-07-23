# Performance Strategy

This is a standing strategy for Koja runtime and codegen performance. It is not
a backlog or release plan. It records the current cost model, measured gaps,
and optimization directions that preserve the language contract.

Concrete commitments belong in [ROADMAP.md](ROADMAP.md). Correctness problems
belong in [RUNTIME-GAPS.md](RUNTIME-GAPS.md). Representation and scheduling
constraints live in [MEMORY-MODEL.md](MEMORY-MODEL.md) and
[SCHEDULER-PROTOCOL.md](SCHEDULER-PROTOCOL.md).

## Discipline

Performance work starts with a representative measurement and ends with the
same measurement. A technique remains an idea until it improves throughput,
latency, memory, or tail behavior without weakening semantics.

Microbenchmarks identify costs. They do not establish application performance.
Long-running services, process churn, and real package workloads decide whether
an optimization matters.

Correctness comes first. The native panic and forced-kill paths can currently
leave managed allocations referenced only by discarded frames. Performance
work must not hide that reclamation gap or interpret crash-loop RSS growth as
ordinary allocation overhead.

## Current cost model

The LLVM backend is the production performance target.

- Primitive values copy as bits.
- `String`, `Binary`, `Bits`, and closure environments use non-atomic
  reference-counted blocks within a process.
- Native collection copies allocate and copy their backing buffers.
- User composites recursively copy or acquire their fields.
- Native process boundaries physically deep-copy managed payloads.
- The cooperative interpreter uses host values and `Rc` storage. Its
  allocation profile is not evidence for native performance.
- Scheduling is cooperatively preemptive through compiler-inserted yield
  checks.

There is no general in-place-when-unique optimization, reference-count
elision pass, process-boundary move inference, or cross-process shared
immutable block today. The one targeted exception is the tail-call rewrite,
which transfers argument ownership across the loop back edge instead of
paying a clone and drop on every iteration.

## Measured baseline

The baseline snapshot below was measured on July 22, 2026, on one Apple Silicon
Darwin machine with `just bench`. Koja uses `build --release` and LLVM `-O3`.
The comparison uses Erlang/OTP BeamAsm. Internal timing excludes startup and
compilation. Lower values are better.

| Workload                        | Koja median | BEAM median | Koja / BEAM time |
| ------------------------------- | ----------: | ----------: | ---------------: |
| Tight loop, 200M iterations     |      210 ms |      324 ms |            0.65x |
| `fib(35)`, 29.9M non-tail calls |       65 ms |       52 ms |            1.25x |
| Tail scan, 200M iterations      |      432 ms |      457 ms |            0.95x |
| Message round trip, 1M          |      616 ms |      479 ms |            1.29x |
| Spawn and reply, 100k           |      326 ms |      102 ms |            3.20x |
| Process storm, 10k processes    |      221 ms |      138 ms |            1.60x |

These are diagnostic microbenchmarks, not release gates. They cover only the
compiled backend. Koja `Ref.call` also performs timeout and reply-correlation
work that the raw Erlang send and receive comparison omits.

The useful signal is structural. Native compute is competitive. Message round
trips and process storms are within roughly 1.3x to 1.6x of the BEAM in this
snapshot. Spawn and reply remains the largest measured gap at 3.2x.

## Near-term levers

### Reduction checks

Every `YieldCheck` decrements `koja_reductions_left`. On Darwin, thread-local
access may retain a TLV thunk even on the inline path. Call-heavy recursion
therefore pays a meaningful scheduling tax.

Candidate optimizations include:

- pinning the active reduction counter in a reserved register through a
  supported calling convention
- assigning static reduction costs to straight-line regions and decrementing
  once per region
- reducing redundant checks only where the same fairness bound is preserved

The current checks cover loop back-edges, tail calls, and entries of
call-containing functions. Removing one requires evidence that the remaining
sites still bound cooperative execution.

### Native collection copies

Copying `List`, `Map`, or `Set` is proportional to the collection size because
native clone glue allocates a fresh buffer and acquires every managed element.
This is a larger cost than the phrase "cheap value copy" suggests.

Potential improvements include:

- eliding a collection copy when a compiler-proven owned temporary transfers
  directly into its destination
- adding reference-counted collection buffers with copy-before-write
  semantics
- specializing copies for trivial element types
- coalescing nested element acquisitions when the full buffer is immediately
  consumed

Reference-counted collection buffers would change the current representation.
They require a clear uniqueness test, matching drop behavior, and process
deep-copy handling before they become the default.

### Native message copies

Native sends, replies, timers, and spawn configs deep-copy managed payloads so
process-local reference counts remain non-atomic. The cost is proportional to
the reachable payload.

Two optimizations cover different cases.

**Last-use transfer** can hand a native allocation to the receiver when the
compiler proves the sender retains no reachable reference and every transferred
block has a unique owner. This is not a general analysis Koja already has.
Existing owned-temporary lowering is a useful precedent, but process-boundary
transfer needs its own escape and uniqueness proof.

**Shared immutable leaves** can give sufficiently large immutable payloads an
atomic reference-counted representation selected at allocation time. This is
similar to BEAM reference-counted binaries. It should remain an internal
representation unless a user-visible sharing type proves necessary.

Small messages should retain the current non-atomic deep-copy path unless
measurement shows that an atomic representation is cheaper.

### Reference-count and glue optimization

Koja emits explicit clone and drop operations before LLVM optimization.
Opportunities include:

- cancelling provably balanced acquire and release pairs
- coalescing repeated operations across straight-line regions
- specializing composite glue for trivial fields
- measuring whether biased reference counting helps any internal
  cross-thread immutable representation

This work must preserve normal-path cleanup and cannot compensate for missing
unwind cleanup. The panic and kill reclamation gap needs a correctness design,
not an optimizer workaround.

### Long foreign calls

A CPU-bound `@extern "C"` call occupies its native worker until the call
returns. Cooperative reduction checks cannot run inside foreign code.

A dirty-scheduler equivalent could route explicitly marked long calls to a
dedicated pool. The design must define argument ownership, result delivery,
process death while a call is active, and whether the foreign function may
call back into Koja.

## AOT optimization

### LTO, PGO, and post-link layout

Project and dependency package code already enters one whole-program LLVM
module. The remaining opportunities are profile-guided optimization, post-link
layout, and possible LTO across the generated module and supporting native
libraries. These techniques are broad but not free. Profile collection,
reproducibility, build time, and stale profile behavior are part of the
feature.

### Static-call optimization

Protocol dispatch is already static through monomorphization. There is no
dynamic protocol dispatch to devirtualize.

The remaining opportunity is whole-program inlining, constant propagation, and
specialization across static call boundaries. Measure LLVM's current result
before adding a Koja-specific pass.

### Value representation

Pointer tagging or another compact representation could reduce allocation and
copy cost for selected values. Any change couples directly to [ABI.md](ABI.md),
debug information, FFI boundaries, and both backends. It requires a workload
showing that representation size or boxing is a dominant cost.

### Binary and bit operations

LLVM auto-vectorization and focused SIMD intrinsics may improve `Binary` and
`Bits` operations. Prefer patterns LLVM can already optimize before adding
target-specific intrinsics.

## Asynchronous preemption research

Signal-based asynchronous preemption is an open question, neither committed
nor ruled out. The cooperative yield-check contract stands unless the
measurements below justify revisiting it.

Koja already checks loop back-edges, tail calls, and call-containing function
entries. Asynchronous preemption would primarily remove check overhead and
cover compiled native regions that cannot reach a check promptly. It would not
make an arbitrary foreign call safely suspendible, and it would not replace the
cooperative mechanism in eval or a future single-threaded target.

A viable design needs:

- compiler-registered safe suspension points
- signal-safe handoff to the scheduler
- correct register and stack-map recovery
- interaction with runtime locks and foreign calls
- architecture support for every native tier
- a portable cooperative fallback

Native checks should not be removed until the asynchronous path demonstrates
equivalent fairness, correct resumption, and a meaningful measured win.

## Additional research

- NUMA-aware queue placement and steal heuristics at high core counts
- better batching for scheduler wakes and message delivery
- allocation-site profiles tied to source locations
- package-level code-size and compile-time budgets

## Suggested order

1. Establish repeatable application benchmarks and regression tracking.
2. Reduce Darwin reduction-counter overhead.
3. Measure and optimize native collection copies and redundant glue.
4. Add PGO, post-link layout, or native-library LTO only with reproducible
   measurements.
5. Prototype last-use transfer for fresh process payloads.
6. Evaluate shared immutable leaves for large retained payloads.
7. Investigate asynchronous preemption only if yield checks remain a material
   cost.

Every optimization must publish its benchmark, workload, compiler revision,
and correctness checks. Remove ideas from this document when evidence makes
them irrelevant.
