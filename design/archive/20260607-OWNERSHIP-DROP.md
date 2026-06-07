# Ownership & Drop Insertion

> **Superseded by `MEMORY-MODEL.md`** (value semantics + reference
> counting). The affine single-owner / `move` / drop-insertion model
> described below was abandoned in favor of value semantics made cheap
> by RC copy-on-write. Retained as a historical snapshot of the
> reasoning; nothing here reflects the current implementation.

Design for a sound, complete drop-insertion model in the Koja
compiler: every move-typed value gets freed exactly once, no value is
freed while still aliased, and no static/literal payload is ever
handed to `free`. Closes the heap leaks left by the current
conservative scheme (collection backing buffers, composite fields,
function-returned heap) **without** introducing the double-frees and
invalid-frees that a naive widening produces.

This is a destination doc, not a trajectory. Every claim reduces to a
concrete behavior in `koja-ir/src/lower/ownership.rs`,
`koja-ir/src/lower/ctx.rs` / `body.rs`, the codegen in
`koja-ir-llvm/src/emit/locals.rs` and the intrinsic emitters, or the
allocator in `koja-runtime/src/memory.rs`. Where the doc proposes a
change it names the call site it replaces. When this doc conflicts
with `design/archive/`, this doc wins; where it touches pipeline
shape, it defers to `COMPILER-NORTHSTAR.md`.

## Why

Koja is single-owner, move-by-default, no GC (`LANGUAGE.md` §Ownership
and Borrowing). The language contract (§Drop Insertion) promises:
"the compiler inserts deterministic cleanup at scope boundaries;
`List<T>` backing buffers and captured closure environments are freed
automatically." Today the implementation delivers far less than that
promise, and the gap cannot be closed by widening the existing
mechanism — two independent invariants break the moment we try.

The current mechanism is a coarse per-local ownership stamp computed
by `ownership_for_expr` (`koja-ir/src/lower/ownership.rs`): a local is
`Owned` iff its initializer is one of a hardcoded set of
expression shapes — `<>` concat, `<<>>` binary literal, closures,
`receive`, interpolated strings. `Owned` locals get a `DropLocal` at
scope exit (`emit_drop_local`, `koja-ir-llvm/src/emit/locals.rs`),
which frees the `payload - 8` block for leaf heap and the env block
for closures. Everything else is `Unowned` and never freed.

This whitelist is correct but deliberately tiny, and it leaks
everything outside it:

- **Function-returned heap leaks.** `s = some_fn()` where `some_fn`
  returns a fresh `String` is `Unowned` → never freed.
- **Collection buffers leak.** A `List` / `Map` / `Set` bound to a
  local is `Unowned`; its malloc'd backing buffer is never freed,
  contradicting the language doc.
- **Composite-owned heap leaks.** A `struct`/`enum` holding a `String`
  or a `List` field is never recursively dropped.

The obvious fix — stamp more expressions `Owned`, add recursive drop
glue for composites — is what the "recursive composite drop glue"
attempt did. It compiles and passes unit tests, then aborts the whole
language suite, including `42.print()`. Two root causes, both
confirmed with reduced repros and an `lldb` backtrace:

### Blocker 1 — literal payloads are static, not heap

String / Binary / Bits literals are emitted as **private constant
globals** in rodata: `emit_const_payload`
(`koja-ir-llvm/src/emit/constants.rs`) builds a `{ i64 bit_length,
[N x i8] }` global, marks it `constant`, `Private`, and returns a
const-GEP to the payload. The runtime `free` is an unguarded
passthrough to `libc::free` (`koja-runtime/src/memory.rs`). So any
drop path that frees a literal-backed payload calls `free` on rodata.

Today this never happens because a literal binding
(`s = "hi"`) is `Unowned`. But the instant a composite owns a
literal-backed field, recursive drop glue frees it:

```koja
struct Box
  raw: String
end
b = Box{raw: "literal"}   # drop glue → free(rodata) → SIGABRT
```

The value's *provenance* (heap vs static) is invisible at the point of
drop. The leaf-local whitelist sidesteps this by only ever dropping
values whose provenance is statically known to be a fresh malloc; a
composite field has no such guarantee. And literals are not the only
offender: `const` refs lower through `LoadConst` to the same rodata
global, and borrowed params are not heap-owned either — both reach
owned composite positions the same way. The fix therefore cannot be
literal-specific; see §Provenance.

### Blocker 2 — "owned" call results can alias a borrow

Several intrinsics and accessors return a value that the type system
calls owned but that physically aliases their (borrowed) receiver.
The sharpest example is `String.to_binary`:

```
fn emit_to_binary(...) {
    let payload = self_payload(...);   // self's payload pointer
    build_return(Some(&payload))       // returns the SAME pointer
}
```
(`koja-ir-llvm/src/intrinsics/string.rs`)

So `bin = data.to_binary()` produces a `Binary` whose buffer *is*
`data`'s buffer. Stamp that `Owned` and its `DropLocal` frees `data`'s
buffer — which `data`'s real owner frees again. This is exactly how
the widening broke `42.print()`:

```
Int.print → IO.puts → Fd.write:
  bin = data.to_binary()             # alias of borrowed `data`
  ... koja_fd_write(bin.ptr(), ...)  # bin dropped → frees data's buf
→ "pointer being freed was not allocated", SIGABRT in Fd.write
```

The same shape lurks in any borrow-returning accessor: `List.get`
returning an element view, a field getter `fn raw(self) -> String;
self.raw end`, `CPtr.to_string`, etc. The ownership lowering has **no
information about whether a call returns a freshly-owned value or a
view of its arguments**, so it cannot stamp call results at all.

### The common cause

Both blockers are the same missing abstraction: the IR ownership
lattice is a syntactic guess (`ownership_for_expr`) that carries
neither a **heap invariant** (is everything reachable from this owned
value actually freeable?) nor the typechecker's **move/borrow +
return-mode** facts (does this call hand back ownership, or a borrow of
its inputs?). Drop insertion needs the first established (by cloning
statics/borrows at ownership acquisition, §Provenance) and the second
known (§Ownership). Until it has them, the only safe `Owned` sources
are the five hand-verified "always a fresh malloc, never aliases
anything" shapes already on the whitelist.

A third, related gap motivates the same machinery:

### Blocker 3 — composite field-overwrite drop (the "struct field" case)

`obj.field = obj.field.append(x)` (and any in-place mutation idiom,
`LANGUAGE.md` §Mutating Fields) must free the *old* field value before
storing the new one — but only when the RHS did not move the old value
out (e.g. `append` takes `move self`, consuming the old buffer and
returning a grown one that may reuse it). The field-overwrite path in
`lower_field_assignment` (`koja-ir/src/lower/body.rs`) currently only
drops leaf heap (`is_heap_owned`) and has no move-consumption analysis,
so widening it risks freeing a buffer `append` already reused. This is
the deferred "Pass 2" of the drop-glue attempt and depends on the same
move-tracking facts as Blocker 2.

## What "correct" requires

Drop insertion is sound and complete when, for every move-typed SSA
value, the compiler knows three facts at the drop site:

1. **Provenance** — every payload reachable from an `Owned`, droppable
   value must be heap; only heap payloads may be freed. Rather than
   track heap-vs-static at runtime, the compiler *maintains* this as an
   invariant: an `Owned` binding is never initialized from an `Unowned`
   source (literal, `const`, borrow) without a clone. (Resolves
   Blocker 1.)
2. **Ownership** — does this binding own the value, or borrow it?
   Only owners drop. For a *call result*, this is the callee's
   declared return mode: owned (fresh / moved-out) vs borrowed (a view
   of an argument). (Resolves Blocker 2.)
3. **Liveness at the drop point** — has the value already been moved
   out (return, `move`-arg, struct/enum init, field store, control-flow
   arm tail)? A moved value must not be dropped. (Already modeled, in
   part, by `move_out_local_value` + the slot lattice in
   `lower/ctx.rs`; see §Liveness below.)

The current lattice approximates (3) and ignores (1) and (2).

## Design

### Provenance: keep statics static, clone on ownership acquisition

Literals stay zero-cost rodata globals, and so do `const`s (which
lower through `LoadConst` to the same `emit_const_payload` path). The
two alternatives that make heap-vs-static a runtime-visible fact —
heapifying every literal through `koja_alloc`, or tagging the
`bit_length` header with a sentinel `koja_free` checks — both pay a
permanent, program-wide cost (an allocation per literal occurrence, or
a header bit plus a branch on every free and allocator) to solve a
problem that only exists at one boundary. Both are rejected.

That boundary is *ownership acquisition*: a static or borrowed payload
can only reach `free` by being stored into an `Owned`, droppable
position — a struct/enum field, a collection element, or a `move`
parameter. Leaf bindings never trigger it (`s = "hi"` is `Unowned`,
never dropped, stays rodata). So the invariant drop glue needs —
**every payload reachable from an owned, droppable value is heap** — is
established structurally, at lowering, by one rule:

> When an `Owned` binding is initialized from an `Unowned` source
> (literal, `const`, or borrowed value), insert a heap clone of the
> source.

This subsumes literals, consts, and borrows with a single mechanism,
costs nothing for transient literals, and needs no runtime provenance
bit or allocator cooperation. It lowers to the value-semantics rc glue
(`IRInstruction::Clone`, emitted in `koja-ir-llvm/src/emit/clone.rs`):
leaf-heap acquisitions are an `rc_inc` on the shared immutable block,
and composite clones recurse the same shape drop glue does, inverted.

Heapifying literals would *not* let us skip this rule. `const` stays
static by design, and borrowed params are never heap-owned here, so
both still flow `Unowned` into owned positions — "const stays static"
plus "drop glue frees fields" together *force* clone-on-acquisition
regardless. Eager literal heapification would therefore be pure cost
with no drop-glue payoff, and a strict regression on today's
zero-cost transient literals (`print("hi")`, literal match arms).

### Ownership: consume the typechecker's return mode

The move/borrow facts already exist for parameters — `PassMode`
(`koja_ast`), `move self`, borrow-by-default — and the lowering reads
`PassMode` today for argument sinks. What's missing is a **return
mode** on every callable: does the function hand back a freshly-owned
value, or a borrow of one of its inputs?

- Most functions return owned (you cannot return a borrow without
  lifetimes, which Koja does not have). These results are safe to
  stamp `Owned`.
- The exceptions are intrinsics/accessors that return a view
  (`String.to_binary`, `List.get` element, field getters). These must
  either be reclassified as **borrow-returning** (so their results are
  `Unowned`, never dropped) **or** fixed to genuinely copy
  (`to_binary` should clone, matching `CPtr.to_binary`'s documented
  "copies `len` bytes" contract in `LANGUAGE.md`).

Concretely, return mode is a per-function `ReturnMode { Owned, Borrowed }`
**computed in typecheck** (after `resolve`, before `seal_ast`: a
memoized DFS over the resolved call graph where a function is `Owned`
iff every returned value is owned, intrinsics keyed off a hand-authored
catalog, cycles biased to `Borrowed`) and stamped onto the AST-layer
`FunctionSignature`. Computing it there — not in an IR pass — makes it
invariant across monomorphization, surfaces it to the LSP, and has it
in hand at lowering. The intrinsic catalog is the audit of every
pointer-shaped intrinsic: the genuine aliases (`String.to_binary`,
`Binary.ptr`/`to_bits`, `Bits.to_binary`, `CPtr.offset`, `List.get`/
`pop`, `Map.get`) are `Borrowed`; fresh/clone/slice results are `Owned`.

This is the load-bearing input that lets ownership widen past the
five-shape whitelist. It decomposes into two orthogonal axes:

- **Axis A — "is this value owned?"** The return mode feeds it.
  Consumed (in the deferred Phase 2) by `ownership_for_expr` stamping
  `Ownership::Owned` on call results, which in turn drives the
  `Drop*` instructions lowering already emits.
- **Axis B — "how do I recursively free a `T`?"** The composite drop
  glue itself, orthogonal to return mode. See §Drop glue.

#### What landed now vs. what consumes it later

Return mode is **computed and carried** but **not yet consumed**:

- Direct calls read it from the callee's `FunctionSignature` at
  lowering.
- Indirect calls (`CallClosure`, concrete callee unknown) can't reach a
  signature, so the mode also rides on the closure value's type:
  `IRType::Function` carries a distinct IR-layer `ReturnMode` (mirroring
  the `koja_ast::PassMode` → IR `Ownership` split). It is identity-erased
  metadata — `fn(T) -> U` is one structural type regardless of a given
  callee's mode, so it does not affect type equality / hashing. Lowering
  populates it wherever a function value wraps a known callee (the
  fn-as-value adapter from its signature); closures, type annotations,
  and bounded/protocol callees stay the conservative `Borrowed`.

Consuming it now would be unsafe: stamping `Owned` on call results
before the Phase 2 move-out sinks exist would double-free
(`x = f()` owned, then `g(move x)`). So this phase keeps the
conservative `Borrowed` default everywhere it's read, leaving behavior
unchanged (leak-not-double-free) until the machinery below lands
together.

### Liveness: generalize the existing move tracking

The slot lattice in `FnLowerCtx` (`lower/ctx.rs`) and
`move_out_local_value` (`lower/body.rs`) already track per-slot
`Owned`/`Unowned` + `moved`, and merge across control flow. To support
composite owned locals, extend the move-out sinks to: rebind RHS
(`x = y`), field-assignment RHS (`obj.f = y`), and control-flow arm
tails — every site where an owned local escapes into another owner.
The branch merge must treat `moved` as **OR across arms** (a value
moved on any path is not unconditionally droppable at the single
scope-exit drop point), biasing to leak-not-double-free until per-path
drop flags exist.

(These three move-out sinks and the OR-merge were prototyped in the
shelved attempt and are correct in isolation; they are safe to land
once provenance + return-mode unblock the `Owned` widening they
support.)

### Drop glue, once unblocked (Axis B)

With the heap invariant (clone-on-acquisition) and return-mode in
place, recursive composite drop glue is sound. The destination shape is
an **`elaborate` IR sub-pass**, not a backend fallback: it
whole-program-discovers which composite types are actually dropped
(mirroring the closure/process discovery passes), synthesizes one
per-type drop function (`FunctionKind::DropGlue { ty }`) into the
`IRProgram`, and rewrites every composite `DropLocal`/`DropValue` into a
`Call @drop_T`. The synthesized body recurses structurally: a struct
drops each owning field, an enum tag-dispatches to its variant payload,
and `List`/`Map`/`Set` walk their elements then free their always-heap
backing buffers. Leaf heap (`String`/`Binary`/`Bits`) keeps its direct
`DropLocal`.

Materializing the glue as real `IRFunction`s — discovered and lowered
before seal, like every other function — keeps the backend a pure
translator and removes its current panic-on-composite fallback,
satisfying the northstar's "no lazy backfill in codegen" rule. It is
orthogonal to return mode (Axis A): Axis A decides *whether* a value is
dropped, Axis B decides *how* to free a `T` once that decision is made.

### Field-overwrite (Blocker 3)

Once the lattice tracks whether the RHS of `obj.f = obj.f.method(...)`
consumed the old field (via the same move-out facts), the
field-overwrite drop in `lower_field_assignment` can conditionally
drop the old value: drop iff the old field was *not* moved out by the
RHS. This is a direct consumer of the move-tracking above and should
land after it.

## Phasing

1. **Return mode + intrinsic audit (landed).** Compute `ReturnMode` in
   typecheck onto `FunctionSignature`; port the intrinsic audit into a
   typecheck catalog; carry the IR-layer mode on `IRType::Function` for
   indirect calls. Computed and carried, not consumed — `ownership_for_expr`
   keeps the whitelist but now *can* trust call results (direct via the
   signature, indirect via the closure value's type). Unblocks Blocker 2.
2. **Widen ownership + move-out sinks + OR-merge + clone-on-acquisition.**
   Stamp call results, constructors, and collection literals `Owned`
   using the facts from (1); add the rebind / field-RHS / arm-tail
   move-out sinks; add the clone-on-acquisition guard so any `Unowned`
   source feeding an `Owned` binding (aggregate field, collection
   element, `move` param) is cloned to heap. This is where Blocker 1's
   resolution lands — statics stay static, ownership boundaries clone.
3. **Recursive drop glue.** Land the composite glue against the now-safe
   value flow. RSS leak fixtures for struct / enum / `List` / `Map` /
   `Set` + nested; eval parity; `just doit` + `just tsan`.
4. **Field-overwrite drop.** Conditional old-value drop on field
   reassignment, gated by move-consumption.

Each phase is independently green; only (2) onward changes observed
free behavior.

## Mechanical checks

- No `free` reaches a pointer that did not originate from `koja_alloc`
  (verify with a debug allocator / ASan over the lang suite once
  provenance lands).
- Every intrinsic returning a pointer-shaped value is classified
  owned-fresh or borrow-of-arg; no intrinsic returns an aliasing value
  stamped owned.
- A value moved on any control-flow path is never unconditionally
  dropped at scope exit (OR-merge invariant).
- `just doit` + `just tsan` green, plus RSS-stability fixtures that
  bound steady-state memory for long-lived collection/composite churn.

## Downstream consumers

- **Message discard.** `design/archive/20260529-MESSAGE-LIFECYCLE.md`
  phase 6 (`Envelope.drop_glue`) is a direct consumer: once recursive
  drop glue exists (phase 3 here), the `send` site can stamp the
  message type's glue pointer into the envelope so a discarded
  mailbox reclaims nested payload heap, not just the transport buffer.
  Until then `Envelope.drop_glue` stays `null` (transport reclaimed,
  nested payload leaked).

## Non-goals

- Per-path conditional drop flags (drop-on-some-branches). The OR-merge
  bias leaks rather than tracks these; revisit only if the leaks prove
  material.
- Runtime heap-vs-static provenance — a `bit_length` tag bit, or
  heapifying every literal through `koja_alloc`. The
  clone-on-acquisition invariant keeps statics out of drop paths
  structurally, so no runtime tag, allocator cooperation, or
  per-literal allocation is needed.
- Reference counting or any shared-ownership story. Koja stays
  single-owner.
- Cyclic-data reclamation beyond what `Indirect` boxing + move
  semantics already bound.
