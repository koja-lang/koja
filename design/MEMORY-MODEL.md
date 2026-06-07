# Memory Model

The destination memory model for Koja: **value semantics**, made
cheap by reference-counted copy-on-write, opportunistic in-place
mutation, and explicit arena regions. Every value behaves as if it
were an independently-owned copy; the runtime makes that affordable
instead of literal.

This is a destination doc, not a trajectory. It records decisions
reached deliberately, with the reasoning that motivated each. It
**supersedes** the affine single-owner / borrow / drop-glue direction
in `OWNERSHIP-DROP.md` (see [Supersedes](#supersedes-and-open-questions)).
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

**Supersedes.** This model retires the affine drop-insertion
direction: `OWNERSHIP-DROP.md`, the shelved `elaborate.rs` drop-glue
pass, `FunctionKind::DropGlue`, and the synthesized struct/enum/
collection drop machinery. The phase-1 **return-mode inference** in
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
3. Message-boundary policy: deep copy vs atomic-rc handoff per type
   (large binaries especially).
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

## Implementation tracker (drop-glue / RC rollout)

Compiler-internal `clone_T` / `drop_T` glue (via the `elaborate`
sub-pass), retiring the user-facing `Clone` protocol. Strategy:
reference-counting for heap leaves; synthesized glue for composites
(`List`, `Map`, `Set`, struct, enum, union). User-facing value
semantics: every binding owns an independent value; acquire at
boundaries (`Clone` / `clone_T`), release at scope exit (`Drop` /
`drop_T`). Last-use move elision is a future optimizer — the naive
baseline always acquires at boundaries for now.

### Completed

- **Phase 0 — scaffold.** `is_heap_managed`, `FunctionKind::CloneGlue` /
  `DropGlue`, `elaborate` wiring, glue symbol mangling, seal + backend
  match updates.
- **Phase 1 — struct / enum / union glue.** `elaborate` synthesizes
  aggregate clone/drop IR bodies; LLVM emits them; seal validation +
  unit tests.
- **Phase 2a — IR wiring.** `elaborate` rewrite pass (composite
  `Clone` / `Drop` → glue `Call`); eval short-circuits
  `CloneGlue` / `DropGlue`; LLVM no-glue aggregate arms (rebind /
  no-op).
- **Phase 2b — collection glue bodies.** LLVM `clone_T` / `drop_T` for
  `List`, `Map`, `Set`.
- **Phase 2c — COW correctness.** Shared element acquire/release
  helpers; list insert/append COW; hashtable clone/insert/resize;
  `emit_map_get` must call `acquire_value` on hand-out (fixed).

### In progress — Phase 2d

Flip lowering from `is_heap_leaf` to `is_heap_managed`; end-to-end
value-semantics tests.

**Done in this slice:**

- `materialize_owned`, `emit_slot_drops`, `drop_discarded_temp` use
  `is_heap_managed`.
- `heap_leaf_slots` → `heap_managed_slots`.
- TCO promotion prefix scan is structural (not heap-leaf-specific).
- `IRType::Indirect` is transparent in `elaborate` (no separate glue;
  inner type's glue applies).
- **Loop body scoping fix** (`lower/loops.rs`): bindings declared
  inside a loop body are dropped at the back-edge and excluded from
  function-exit drops (fixes zero-trip loop + uninitialized slot drop,
  e.g. `Headers.set` on empty list).
- `lower_process` tests updated for acquire-before-return on
  heap-managed values.

**Done in a later slice:**

- **`List.pop()` empty-branch clone** (`koja-ir-llvm` `intrinsics/list.rs`):
  the empty branch returned the borrowed `self` buffer directly as the
  pair's `.second`, so `empty_pair.second` aliased the caller's
  receiver slot and both freed the same buffer at scope exit. Now
  clones into a fresh (empty) buffer, mirroring the nonempty branch's
  `copy_buffer`. Fixes the `list_test` "pop until empty" teardown
  crash (was the global-stdlib exit-1).
- **Tail-call composite-arg acquire** (`tail_calls.rs`): the
  self-tail-call rewrite acquired only _heap-leaf_ args before the
  trailing exit drops; composite args (`List`, struct, etc.) were
  rebound from the just-dropped slot → use-after-free on the next
  iteration. Now acquires every `is_heap_managed` arg (the inserted
  `Clone` is elaborated into `clone_T` for composites). Fixes the
  `http` `headers_test` "get_all" crash (recursive `collect_all`).
- **Shared heap predicate**: hoisted `is_heap_managed` to an
  `IRType` method; `lower::ownership` and `tail_calls` now share it.
- All of `just doit` green (lint + stdlib + `test-rust` + `test-lang`).

**Remaining for Phase 2d:**

- **Call-boundary acquire** (`lower/calls.rs`) is _not_ needed for
  correctness under the current "callee acquires on promotion" +
  "intrinsics return freshly-owned heap" convention — the two crashes
  above were the intrinsic empty-branch and the tail-call gap, not a
  missing caller-side clone. Revisit only if a future aliasing case
  surfaces; blanket caller-side cloning would leak against intrinsics
  that borrow `self`.
- Optional: regression test for zero-trip loop with body-scoped
  heap binding. (`pop()`-on-empty and tail-recursive composite args
  are now covered by stdlib `list_test` / http `headers_test` plus
  `tail_calls` unit tests.)

### Done — Phase 3

Closures are now first-class heap-managed values: `is_heap_managed`
returns `true` for `IRType::Function`, so `materialize_owned` /
`emit_slot_drops` / `drop_discarded_temp` treat a closure exactly like
a `String` or a `List`. Captures are acquired into the env at
`MakeClosure` and released transitively when the env's refcount hits
zero — no more leaked env blocks and no use-after-free when a captured
heap value's outer binding is dropped first.

**Env ABI.** A (non-null) env block gains a 16-byte header mirroring
the heap-leaf shape: `[i64 rc][ptr drop_fn]`, captures following.
`drop_fn` is the address of the closure body's capture-release glue
(or null when no capture is heap-managed). Both the backend
(`CLOSURE_ENV_HEADER_FIELDS`) and the runtime (the `LENGTH_OFFSET`
header note) agree on the layout; capture `i` lives at field
`2 + i`. The env base pointer doubles as the rc word, so the existing
`koja_rc_inc` operates on it unchanged.

**Clone / drop.** `Clone` of a `Function` is an `rc++` on the env
(aliasing the same `{fn_ptr, env_ptr}` fat pointer — the env is shared
like an immutable leaf). `Drop` calls the new `koja_closure_rc_dec`,
which null/immortal-checks, decrements, and at zero runs `drop_fn`
(when present) before `free`ing the block. Both the slot-keyed
(`DropLocal`) and value-keyed (`DropValue`) closure drop paths funnel
through `emit_drop_closure_value`.

**Capture-release glue.** A closure body that owns ≥1 heap-managed
capture gets a sibling `FunctionKind::DropClosureGlue`
(`<body>.$drop_env$`) minted during lowering: closure-shaped (implicit
`env_ptr`, env-first ABI), it `LoadCapture`s each heap-managed capture
and `DropValue`s it, returning `Unit`. Born as real IR so `elaborate`
discovers any composite capture's `drop_T` and rewrites the composite
`DropValue`s into glue calls, exactly as for a `Regular` body. Seal
admits it alongside `Closure` (it's the second `LoadCapture`-bearing
kind); eval never invokes it (host GC reclaims closures).

**Retain cycles: not reachable.** RC here only _shares_ immutable
values; a closure cannot capture a still-mutable binding to itself
(captures are by value, taken at `MakeClosure`, and there are no
reference types). A closure's env can only contain values that
existed before the env, so the ownership graph stays a DAG — no cycle
can form, and the naive `rc--`-frees-at-zero scheme is sound without a
cycle collector.

### Done — Phase 4

The user-facing `Clone` protocol is gone — value semantics makes
explicit duplication meaningless (every value is already independent,
and rc copy-on-write makes assignment cheap). Removed:

- `lib/global/src/clone.koja` (protocol decl + every primitive /
  heap-leaf impl) and its `Global.clone` autoimport entry.
- The `List` / `Map` / `Set` / `CPtr` `Clone` impls in their
  respective `.koja` files.
- `derive_clone.rs` (the auto-`impl Clone for T` synthesizer) and its
  pre-collect wiring in `program.rs` / `synthesize/mod.rs`.
- `Clone` from `UNIVERSAL_PROTOCOLS`; the universal fallback in
  `bounded.rs` now augments bare type-param bounds with `Debug` /
  `Equality` only.
- All `.clone()` call sites in `lib/http` (`client.koja`,
  `parser.koja`) — plain assignment / argument passing replaces them.
- The protocol-clone test files (`koja-typecheck/tests/clone.rs`,
  `koja-ir/tests/lower_clone.rs`, `koja-ir-llvm/tests/clone.rs`,
  `koja-ir-eval/tests/clone.rs`); the `ownership_clone*` lang tests
  were repurposed as `ownership_assign*` (same independence assertion,
  via plain assignment instead of `.clone()`).

This was safe because the value-semantics rc glue already covers every
duplication path: collection copy-on-write
(`hashtable::clone_table_buffers` → `acquire_in_slot`) and element
acquisition run on the internal `$clone$` glue (`clone_glue_symbol` /
`IRInstruction::Clone`), never the protocol. The protocol-clone
backend chain was reachable _only_ through user-facing `.clone()`.

**Backend dead-code cleanup (done):** the now-unreachable
protocol-clone backend chain was excised end to end alongside the
frontend removal — the `*Method::Clone` intrinsic variants and their
`"clone"` mappings in `koja-ir/src/intrinsic_id.rs`, the per-method
dispatch arms (`string.rs` / `binary.rs` / `map.rs` / `set.rs` in both
backends), `emit_table_clone` + `resolve_clone_fn` /
`clone_receiver_symbol` / `call_clone` (hashtable `lifecycle.rs` /
`util.rs`), and the eval `deep_clone_value` / `deep_clone_payload`
helpers. The deep-copy backbone (renamed `heap_clone.rs` →
`heap_payload.rs`) stays for the conversions that genuinely mint a new
block (`Binary.to_string` / `CPtr.to_string`, which add a libc NUL),
and as the seed for future "copy on process boundary" deep copies. The
same-layout reinterprets (`Binary.to_bits`, `String.to_binary`,
`Bits.to_binary`) were migrated off the deep copy to an `rc_inc` +
pointer reinterpret (`heap_payload::share_heap_payload`) — Koja blocks
are immutable, so sharing is invisible and matches the heap-leaf
`Clone` path.

### Pending — Phase 5

Refresh doc comments + this doc's "Supersedes" paragraph (elaborate
glue is landed, not shelved); `just lint` + `just test`; leak audit
(HTTP suite passes without explicit clones).

### Known bugs discovered (session notes)

| Symptom                                                                          | Root cause                                                                                               | Fix status                        |
| -------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- | --------------------------------- |
| `qualified_signature.koja` SIGBUS (flaky → deterministic with empty list + loop) | Loop-body local dropped at function exit uninitialized when loop runs 0 times                            | Fixed (loop scoping)              |
| `Map.get` double-free                                                            | Hand-out without `acquire_value`                                                                         | Fixed                             |
| `List.pop()` on empty list, teardown SIGBUS                                      | Empty branch returned the borrowed `self` buffer as `Pair.second`; caller slot + pair drop the same list | Fixed (empty-branch clone)        |
| Global stdlib exits 1 after ~84 green dots                                       | Same as pop-on-empty (crashes before harness summary)                                                    | Fixed                             |
| `http` `headers_test` "get_all" SIGSEGV in `clone_Header`                        | Tail-call rewrite acquired only heap-_leaf_ args; composite args rebound from just-dropped slot          | Fixed (acquire heap-managed args) |
| `lower_process` spawn/receive tests                                              | Expected raw `ValueId`; return now acquires heap-managed values                                          | Fixed (structural assert)         |
