# Ownership & Drop Insertion

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
composite field has no such guarantee.

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
neither **provenance** (is this payload heap or static?) nor the
typechecker's **move/borrow + return-mode** facts (does this call
hand back ownership, or a borrow of its inputs?). Drop insertion needs
both. Until it has them, the only safe `Owned` sources are the five
hand-verified "always a fresh malloc, never aliases anything" shapes
already on the whitelist.

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

1. **Provenance** — does this value's payload live on the heap
   (`koja_alloc`) or in a static global? Only heap payloads may be
   freed. (Resolves Blocker 1.)
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

### Provenance: make heap-or-static a runtime-visible fact

Two viable shapes; recommend the first.

- **(A) Heapify literals.** Materialize String / Binary / Bits
  literals through `koja_alloc` instead of a constant global
  (replace the rodata path in `emit_const_payload`). Every payload is
  then uniformly freeable, drop glue needs no provenance bit, and the
  C-ABI passthrough invariant in `memory.rs` is preserved. Cost: a
  per-literal allocation at first use (amortizable by caching a heap
  copy per literal global behind a one-time init), and literals stop
  being shareable rodata.
- **(B) Tagged free.** Keep literals static but reserve a sentinel in
  the `i64 bit_length` header (e.g. a high bit, or a `-1` refcount
  word) that `koja_free` checks and no-ops. Cheaper at materialization,
  but spends a header bit and adds a branch to every free, and every
  hand-written runtime allocator must honor the sentinel.

Either makes Blocker 1 disappear: drop glue can free any leaf payload
it reaches.

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

Concretely: add a return-mode bit to the IR function signature
(owned vs borrows-arg-N), populated by typecheck, and have
`ownership_for_expr` stamp `Call`/`MethodCall` results from it instead
of guessing. Audit every intrinsic that returns a pointer-shaped value
for accidental aliasing; either make it copy or mark it borrowing.
This is the load-bearing change — it is what lets ownership widen past
the five-shape whitelist at all.

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

### Drop glue, once unblocked

With provenance and return-mode in place, recursive composite drop
glue (`FunctionKind::DropGlue { ty }`, registered pre-seal, bodies
synthesized in `koja-ir-llvm/src/intrinsics/drop_glue.rs`) is sound:
a struct drops each owning field, an enum tag-dispatches to its
variant payload, and `List`/`Map`/`Set` walk their elements then free
their always-heap backing buffers. The glue infrastructure from the
shelved attempt is reusable as-is; it was only ever unsafe because the
values flowing into it had unknown provenance and ownership.

### Field-overwrite (Blocker 3)

Once the lattice tracks whether the RHS of `obj.f = obj.f.method(...)`
consumed the old field (via the same move-out facts), the
field-overwrite drop in `lower_field_assignment` can conditionally
drop the old value: drop iff the old field was *not* moved out by the
RHS. This is a direct consumer of the move-tracking above and should
land after it.

## Phasing

1. **Provenance.** Heapify literals (or tagged-free). Self-contained;
   unblocks Blocker 1; no behavior change until drops widen.
2. **Return mode + intrinsic audit.** Add the IR return-mode bit,
   populate from typecheck, fix/mark aliasing intrinsics. Unblocks
   Blocker 2. Still no widening yet — `ownership_for_expr` keeps the
   whitelist but now *can* trust call results.
3. **Widen ownership + move-out sinks + OR-merge.** Stamp call
   results, constructors, and collection literals `Owned` using the
   facts from (1)+(2); add the rebind / field-RHS / arm-tail sinks.
4. **Recursive drop glue.** Land the composite glue against the now-safe
   value flow. RSS leak fixtures for struct / enum / `List` / `Map` /
   `Set` + nested; eval parity; `just doit` + `just tsan`.
5. **Field-overwrite drop.** Conditional old-value drop on field
   reassignment, gated by move-consumption.

Each phase is independently green; only (3) onward changes observed
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
  drop glue exists (phase 4 here), the `send` site can stamp the
  message type's glue pointer into the envelope so a discarded
  mailbox reclaims nested payload heap, not just the transport buffer.
  Until then `Envelope.drop_glue` stays `null` (transport reclaimed,
  nested payload leaked).

## Non-goals

- Per-path conditional drop flags (drop-on-some-branches). The OR-merge
  bias leaks rather than tracks these; revisit only if the leaks prove
  material.
- Reference counting or any shared-ownership story. Koja stays
  single-owner.
- Cyclic-data reclamation beyond what `Indirect` boxing + move
  semantics already bound.
