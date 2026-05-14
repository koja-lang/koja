# Lang-suite golden gaps

A triage of the `tests/lang/` fixtures that fail under
`expo alpha run --backend=llvm` as of 2026-05-13. Companion to
[V1-PARITY.md](V1-PARITY.md) — that doc tracks the closed parity items;
this doc enumerates what's still open and groups failures by root cause so
fixing one entry unblocks a known cluster of fixtures.

PASS count as of last run: `48 passed, 18 failed, 1 skipped` (script
[scripts/validate_alpha_lang.sh](../scripts/validate_alpha_lang.sh),
`process_lifecycle` skipped because it's signal-driven).

Two top-level buckets:

- **Real alpha gaps** — bugs / unimplemented features the alpha pipeline
  needs to absorb. Each root-cause cluster lists the fixtures it unblocks.
- **v1-permissive idioms** — fixtures that lean on v1 looseness alpha
  intentionally rejects. Rewrite the fixture (or defer the gap) rather
  than relaxing the type system.

---

## Real alpha gaps

### 1. `Process`-shaped PascalCase entries (4 fixtures)

`fn App.run(self)` style entries are rejected:

> alpha pipeline does not yet support PascalCase Process entry `App`;
> use a `fn main` entry for now

Blocks `kernel_exit/`, `process_argv/`, `process_entry/`, `process_exit/`
— all four use the `App` entry shape. Two viable fixes: (a) implement
the PascalCase entry resolver (lifts the body into the runtime's spawn
thunk like the `fn main` path); (b) rewrite the fixtures to use `fn
main` and a manually-spawned `App` ref. (a) preserves the surface area
v1 supported.

### 2. Recursive enum miscompile (1 fixture)

`generics/recursive_enum` — built binary exits 133 silently. The
fixture exercises a self-referential enum (a tree with `Node(Box<...>)`
shape). The companion fixtures `recursive_generic_list` and
`recursive_generic_map` previously failed under the same banner; both
now pass once the LLVM layout phase started defining variant complete +
outer bodies in dependency order. `recursive_enum` still fails, so the
remaining root cause is likely IR-side payload projection through the
recursive variant rather than the layout phase.

### 3. `Ref<M>` substitution with union message types (2 fixtures)

`io/process_union_msg` + `types/union_struct_field` both fail with:

> type parameter `M` of `Global.Ref` cannot be both `MsgA | MsgB`
> and `MsgA`

`Substitution::set` rejects a narrowing rebinding of `M` from the
union literal type to one of its members. Likely fix: when the
template slot is bound to a union and the new actual is a member of
that union, treat it as compatible (UnionWiden) rather than a
conflict.

### 4. `HTTP` not wired into `ALPHA_QUALIFIED` (2 fixtures)

`types/qualified_signature` and `types/qualified_static_call` both
reference `HTTP.Headers`:

> alpha typecheck does not recognize the type name `HTTP.Headers`

Per [V1-PARITY.md](V1-PARITY.md): "`Http` — language-feature parity ...
ready to wire into `ALPHA_QUALIFIED` once the source files clean-compile
end-to-end." This is the wiring step (and confirming the source files
clean-compile, which they presumably do now that field assignment and
dotted type names shipped).

### 5. Generic function-pointer field as callee (1 fixture)

`functions/fn_generic_arg` — IR lower asserts:

> alpha IR lower: instance method receiver resolved to non-Global type
> (Anonymous(Function { … }))

A generic struct field holding a function pointer is being dispatched
as a method receiver. Needs a callee shape for `wrapper.f()` indirect
dispatch where `f` is an anonymous-function-typed field on a generic
type.

### 6. Miscellaneous one-offs (2 fixtures)

- `functions/short_closures` — Wildcard closure parameter (`|_| ...`)
  not yet lowered: `alpha IR lower: closure param #0 (Wildcard { … })
  is not yet supported in lowering`.
- `types/nested_enum_eq` — `==` not implemented for `Option<Color>` or
  any enum-of-enum. Likely needs `Equality` derivation to recurse
  through generic enums.

---

## v1-permissive idioms (4 fixtures, fixture rewrites)

These fixtures lean on v1 looseness alpha intentionally rejects.
Rewriting the fixture is cheaper than relaxing the type system.

- **`generics/nested_generics`, `generics/recursive_generic`,
  `generics/recursive_struct`** — `Option.None` in generic position
  relies on backwards-flow inference. Alpha says:

  > alpha typecheck cannot infer type parameter `T` of `Global.Option`
  > from unit variant `None`

  Already documented under "Generic enum unit variants in top-level
  code" in [GAPS.md](GAPS.md). Rewrite the fixtures with explicit type
  annotations: `let x: Option<Foo> = Option.None` or pass through a
  typed return slot.

- **`ownership/ownership_clone`** — Calls `.clone()` on `Int` and on a
  plain `Point` struct (no heap fields). In alpha, `Clone` is a
  protocol — copy types don't (and shouldn't) implement it. Rewrite to
  drop the explicit `.clone()` calls on copy types.

---

## Closed: opaque `Debug` receivers for anonymous types

Latent at the time this doc was written; surfaced once the
remaining stdlib packages (`Net`, `Json`) were wired into
`ALPHA_QUALIFIED` and their `Process<…, Union<…>>` impls forced
mono to substitute `A → Union<…>` inside the parametric
`impl Debug for Pair<A, B>`. IR-lower's `receiver_struct_id`
panicked because the receiver was `Union(…)` instead of
`Named { Global }`.

Fixed by short-circuiting `lower_method_call` for
`Debug.{format, print, inspect}` on opaque receivers
(`Union` / `Anonymous(Function)`): emit the literal `"..."`
placeholder, matching the AST-layer rule
[`derive_debug::is_opaque_type`] already applies to opaque
struct/enum fields. Pinned by
`crates/expo-alpha-ir/tests/opaque_debug_receivers.rs`. See
[V1-PARITY §10](V1-PARITY.md#10-opaque-debug-receivers-for-anonymous-types--shipped-2026-05-13).

---

## Mixed: covered by an existing plan

- **`basics/int_coercion`** — Two v1-isms stacked, both addressed by the
  pending sized-int arithmetic + `IntLiteral` protocol work:
  1. `Counter.add` body uses `Int32 + Int32` arithmetic; alpha rejects
     sized-int arithmetic. Closed by Phase 1 of that plan
     (`binary_type` / `unary_type` generalization).
  2. `x: Int32 = identity(42)` widens an `Int` literal through a generic
     return; alpha won't unless `IntLiteral<T>` ships. Closed by Phase
     2 of that plan.

---

## Infrastructure (not an alpha bug)

- **`ffi/`** — `alpha LLVM object emit failed: failed to write object
  file: "Operation not permitted"`. Sandbox-only filesystem permission
  issue on the link temp dir. The fixture itself compiles. Move the
  temp-dir under a writable path or ignore this fixture in
  sandboxed runs.

---

## Priority order (cheapest unblock per fixture-count)

1. **PascalCase process entry** (gap #1) — 4 fixtures from one fix.
2. **Option.None inference rewrites** (bucket B) — 3 fixtures, zero
   compiler work.
3. **`IntLiteral` + sized arithmetic** (mixed) — 1 fixture but unblocks
   the broader narrow-int story across stdlib.
4. **`Ref<M>` union substitution** (gap #3) — 2 fixtures.
5. **Wire `HTTP` into `ALPHA_QUALIFIED`** (gap #4) — 2 fixtures.
6. **Recursive enum miscompile** (gap #2) — 1 fixture.
7. **One-offs** (gap #5, #6) — 3 fixtures each fixed in isolation.

After (1)–(6) the lang suite is at full alpha parity and `lang_suite.rs`
can flip its runner off v1.
