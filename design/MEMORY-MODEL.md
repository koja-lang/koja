# Memory Model

The destination memory model for Koja: **value semantics**, made
cheap by reference-counted copy-on-write, opportunistic in-place
mutation, and explicit arena regions. Every value behaves as if it
were an independently-owned copy; the runtime makes that affordable
instead of literal.

This is a destination doc, not a trajectory. It records decisions
reached deliberately, with the reasoning that motivated each. It
**supersedes** the affine single-owner / borrow / drop-glue direction
in `archive/20260607-OWNERSHIP-DROP.md` (see
[Supersedes](#supersedes-and-open-questions)).
Where it touches pipeline shape it defers to `COMPILER-NORTHSTAR.md`.

## Why the pivot

The affine model (single owner, borrow-by-default, `move` to consume,
compiler-inserted drops) kept spawning design forks that never
resolved cleanly: per-function return modes, struct-field ownership,
collection drop glue, and escaping borrows. Every one traced back to
the same root — **borrows with no lifetime or escape analysis**. Drop
insertion fundamentally needs to answer "does this value's ownership
escape, and if so, who frees it?", which is the lifetime question;
the language had deliberately removed the mechanism that answers it,
then asked the backend to answer it anyway. The hand-maintained
return-mode catalog was the tell: ownership wasn't derivable from
structure, so it had to be enumerated.

Rather than reintroduce lifetimes (Rust's complexity, against Koja's
no-annotations goal), we invert the model: **everything is a value.**
Ownership stops being a correctness obligation the programmer reasons
about and becomes a runtime/optimizer concern. Memory safety becomes
automatic; performance becomes an optimization problem that never
changes program semantics.

## Core model

1. **Value semantics.** Every binding, parameter, return, and
   field-store is semantically an independent value. There are no
   borrows, no aliasing, no observable sharing.
2. **Cheap by implementation, not by literal copy.** Heap values
   carry a refcount. "Clone" is lazy (copy-on-write); "drop" is a
   decrement, freeing at zero. Scalars stay inline and trivially
   copied.
3. **Optimizations that recover performance** (all semantics-
   preserving — they fall back to a copy when unsure, so they are
   sound by construction):
   - **Last-use move elision.** A clone whose source is dead
     afterward degrades to a move (free). Intra-procedural liveness,
     far easier than escape analysis.
   - **Opportunistic in-place mutation.** When a heap value's refcount
     is 1, mutate in place instead of copying. Collapses the threaded-
     state idiom (`acc = acc.append(x)` in a loop) back to amortized
     O(1)/step.
   - **Refcount elision.** Cancel balanced inc/dec pairs the compiler
     can prove redundant.
4. **The copy/move type split dissolves.** `LANGUAGE.md`'s "copy types
   vs move types" becomes an implementation distinction (inline vs
   heap), not a semantic one the programmer learns.
5. **Cycles are avoided by construction.** Immutable values built
   bottom-up cannot form reference cycles, so pure refcounting is
   viable with no cycle collector. Immutability is the linchpin that
   makes RC sound here.

## Concurrency and shared state

There are **two distinct refcounts**, and they must not be conflated:

- **Intra-process rc (non-atomic).** The default. Cheap precisely
  because it is non-atomic, which is sound only because a process-
  local value is never reachable from two threads at once.
- **Cross-process rc (atomic — ARC).** Lives inside an explicit
  `Shared<T>` boundary type for state genuinely shared across
  concurrent processes (e.g. concurrent caches). It carries atomic
  refcounting and internal synchronization.

Process isolation is what makes the non-atomic default safe. Messages
are moved between processes; values are copied (or handed to atomic
rc) at the process boundary, Erlang-style. The `Shared<T>` type is the
synchronization seam: reads out of it produce ordinary value-semantics
results that drop back into the cheap non-atomic world; nothing
aliases out of it. The dangerous, aliased, atomically-managed thing is
the one you have to name — good.

Invariant: a non-atomically-refcounted value must never be
simultaneously reachable from two processes. The type system marks
which world a value is in.

## Arenas

`arena { … }` blocks provide a bump-allocated region, freed wholesale
at block exit:

- **rc-free inside.** Values that live and die with the region need no
  refcount — the arena owns them.
- **Bump allocation + bulk free.** Fastest possible allocation; no
  per-object teardown; cache-friendly contiguous layout.
- **Locally mutable.** The region is uniquely owned and transient, so
  in-place construction of large intermediates is allowed.
- **Cycles are a non-issue** within a region (no rc, freed wholesale).

**Escape safety — copy-out on escape.** A value leaving an `arena`
block is cloned into the normal rc heap at the boundary; rc-free
inside, rc'd outside, with the compiler inserting the conversion.
This is "clone on acquisition" applied at the region edge — the same
grain as the rest of the model.

**Ergonomics — implicit arena context.** Callees inherit the active
allocator from an ambient context (Odin-style), so dropping into an
arena does not require viral allocator parameters on every signature.
This is what makes the arena-heavy style pleasant rather than a tax.

**Role.** Arenas are the _targeted_ fast path for profiled,
allocation-heavy or transient hot work (query execution, parsing,
planning, per-iteration temporaries, transient graphs) — **not** the
everyday default. If ordinary code needs arenas to be fast, that
signals an under-powered rc-elision/in-place optimizer, and the fix is
the optimizer. Arenas are also **not** for long-lived mutable shared
state (that is `Shared<T>` or FFI).

**Tiered model**, safe by default, control on demand:

| Tier        | Mechanism                                          | Use                           |
| ----------- | -------------------------------------------------- | ----------------------------- |
| Default     | immutable + rc/COW                                 | the 90% of code               |
| `arena { }` | bump, rc-free, locally mutable, copy-out on escape | profiled hot/transient work   |
| `Shared<T>` | atomic ARC + synchronization                       | cross-process mutable state   |
| `CPtr`/FFI  | manual                                             | truly external / extreme core |

## Surface language changes

- **`move` is removed entirely** — from parameters, `self`, closure
  params, and function types. It annotated a borrow/consume
  distinction that no longer exists. Consumption is automatic via
  last-use elision.
- **Implicit receiver.** Instance methods drop `self` from the
  parameter list. `self` remains a keyword usable as a value (to pass,
  return, or compare the whole receiver). Removing `move` is what
  makes this clean — `self`'s only remaining job was carrying the
  mode.
- **Field access stays explicit: `self.field`.** No implicit
  field access in method bodies (avoids field/local ambiguity and
  preserves readability, per `build.mdc`).
- **`-> Self` stays** unchanged.
- **Associated (receiver-less) functions use `fn Self.foo()`** —
  a Ruby-style namespaced definition that mirrors the `Type.foo()`
  call site. The bare-vs-dotted contrast (`fn greet()` instance vs
  `fn Self.new()` associated) replaces the need for a `static`
  keyword. Applies uniformly to structs, enums, and protocol decls
  (e.g. `fn Self.default() -> Self`). (`type fn` is the fallback if
  the dotted form proves awkward to parse or read.)

Example:

```koja
impl Greeter for Cat
  fn Self.new(name: String) -> Self
    Self{name: name}
  end

  fn greet() -> String
    "meow, I'm #{self.name}"
  end

  fn rename(name: String) -> Self
    Self{name: name}
  end
end
```

## Performance positioning (honest)

The closest shipping sibling is **Swift** (ARC + COW value types);
**Roc** is the aspirational sibling (leans hardest on uniqueness/in-
place to approach systems speed). Claims worth standing behind:

- **Faster than Elixir** for sequential/compute work (native vs BEAM
  VM). Parity at Elixir's own game (massive supervised concurrency,
  fault tolerance) is unproven.
- **Competitive with Go, not categorically faster.** Wins on latency
  (no GC pauses), memory footprint, and deterministic cleanup; loses
  on raw allocation throughput. "Faster than Go" is overreach — do not
  claim it.
- **Near-systems performance inside arena hot paths.**

Caveat: these are _achievable targets contingent on a mature
optimizer_ (rc elision, in-place fast path, uniqueness analysis).
Naive rc/COW out of the box is noticeably slower than Go. Treat
external perf claims as aspirational until benchmarked.

**vs tracing GC:** RC wins on deterministic reclamation (RAII-style
resource cleanup), latency predictability (no stop-the-world), tight
footprint, and FFI/runtime simplicity (stable pointers, no moving
collector, no safepoints). GC wins on raw allocation throughput and
arbitrary cyclic graphs — and Koja sidesteps the cycle weakness by
immutability. For Koja's profile this is a genuine fit, not a
consolation. One-line identity: **Elixir's concurrency model with
Swift's memory model.**

## Suitability

- **Infrastructure / control-plane services** (APIs, network services,
  brokers, coordination, gateways, soft-realtime): strong fit; the
  no-pause latency story can beat Go.
- **Data systems:** service/coordination layers — yes. Query/compute
  data-plane — plausible in idiomatic-plus-arena Koja (arenas match
  how real query engines allocate). Bleeding-edge persistent storage
  core (buffer pool, page cache, WAL) — via `Shared<T>`/FFI for the
  mutable-shared parts; that is a lifetime-sharing problem arenas do
  not solve.

## Supersedes and open questions

**Supersedes.** This model retired the affine drop-insertion
direction in `archive/20260607-OWNERSHIP-DROP.md`. What that doc framed
as future drop-insertion machinery is now the landed RC implementation:
the `elaborate` drop-glue pass, `FunctionKind::CloneGlue` / `DropGlue`,
and the synthesized struct/enum/collection glue all ship today (see the
RC-rollout history archive). The phase-1 **return-mode inference** in
typecheck (`pipeline/return_mode/`) was built to feed drop insertion;
under value semantics drop no longer needs it, so it is a candidate
for removal — flag for review before deleting (it may retain LSP
value, or none).

**Open questions** to resolve before implementation:

1. ~~Refcount header layout for heap values (and interaction with the
   existing `payload - 8` length-prefix convention).~~ **Resolved.**
   Heap-leaf blocks (`String` / `Binary` / `Bits`) are
   `[i64 rc][i64 bit_length][payload…][NUL?]`, a 16-byte header. The
   SSA pointer still addresses the first payload byte, so the existing
   `bit_length` lives at `payload - 8` unchanged; the new `i64 rc` word
   sits at `payload - 16` (the block base, the pointer handed to
   `free` / `koja_rc_inc` / `koja_rc_dec`). `Clone` is `rc++`, `Drop`
   is `rc--` (free at zero); statically-allocated rodata literals carry
   a negative sentinel rc (`i64::MIN`) so inc/dec are no-ops and they
   never reach `free`. The layout constants are mirrored, by contract,
   in `koja-runtime` (`util::{BLOCK_HEADER_SIZE, LENGTH_OFFSET}`),
   `koja-ir-llvm` (`emit::heap_layout`), and `koja-ir-eval`'s raw-ABI
   readers rather than shared through a common crate.
2. COW retrofit on the current runtime: `List` flat buffer,
   `String`/`Binary`/`Bits`, the `Map`/`Set` hashtables.
3. ~~Message-boundary policy: deep copy vs atomic-rc handoff per
   type (large binaries especially).~~ **Resolved: deep copy at the
   boundary (BEAM-style), keeping intra-process rc non-atomic.**
   Send (`Ref.cast` / `Ref.call` / `Ref.send_after` / `ReplyTo.send`)
   and spawn-config sites lower to `IRInstruction::DeepCopy`, which
   produces a physically independent payload — no heap storage shared
   with the sender. Leaves deep-copy via `koja_heap_deep_copy`;
   composites via synthesized `deep_copy_T` glue
   (`FunctionKind::DeepCopyGlue`, the process-boundary analog of
   `clone_T`); closures via a third env-header word (`copy_fn`,
   stamped by `MakeClosure` with the body's
   `FunctionKind::CopyClosureGlue`). The transferred copy is owned by
   the runtime transport and reclaimed through the envelope drop glue
   on discard, or handed whole to the receiver on delivery. An
   atomic-rc handoff for large binaries remains a possible future
   optimization, layered behind the same `DeepCopy` seam.
4. Arena mechanism: ambient-context representation, escape analysis /
   copy-out insertion, interaction with closures that capture arena
   values and with message sends from inside a region.
5. Optimizer maturity: rc-elision and in-place-when-unique are what
   make the perf claims real; scope this work explicitly.
6. Final pick for the associated-fn form (`fn Self.foo()` vs
   `type fn`), and whether to add a mutable-binding sugar over the
   explicit `x = x.f()` rebind.
7. Whether "acyclic by construction" needs enforcement or is
   guaranteed by immutability alone.

## Implementation status

The drop-glue / RC rollout (Phases 0-5) has landed: heap-leaf
reference counting, synthesized composite `clone_T` / `drop_T` glue via
the `elaborate` sub-pass, closure-env RC, and removal of the
user-facing `Clone` protocol. See
[`archive/20260607-MEMORY-MODEL-RC-ROLLOUT.md`](archive/20260607-MEMORY-MODEL-RC-ROLLOUT.md)
for the full implementation history and the bugs fixed along the way.

The process-boundary deep-copy rollout has also landed (open question
3 above): `IRInstruction::DeepCopy` + the `deep_copy_T` glue family,
deep-copy lowering at every send / spawn site, runtime ownership of
the transferred payload (unified `OwnedPayload` RAII across envelopes,
timers, and spawn configs), the two-queue mailbox with a tokened
one-shot reply slot, and zero-init / drop-then-zero receive-slot
discipline. The `tests/lang/memory/` fixtures pin the reclaim
behavior with `koja_rt_live_blocks` steady-state checks.
