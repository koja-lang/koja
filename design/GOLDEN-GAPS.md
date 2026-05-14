# Lang-suite golden gaps

A triage of the `tests/lang/` fixtures that fail under
`expo alpha run --backend=llvm` as of 2026-05-13. Companion to
[V1-PARITY.md](V1-PARITY.md) — that doc tracks the closed parity items;
this doc enumerates what's still open and groups failures by root cause so
fixing one entry unblocks a known cluster of fixtures.

PASS count as of last run: `54 passed, 12 failed, 1 skipped` (script
[scripts/validate_alpha_lang.sh](../scripts/validate_alpha_lang.sh),
`process_lifecycle` skipped because it's signal-driven; the `ffi`
fixture's `libffi_helper.a` must be built ahead of time — see
["Closed: project-root FFI library search"](#closed-project-root-ffi-library-search)).

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

### 2. Recursive payload miscompile (2 fixtures)

`generics/recursive_enum` — built binary exits 133 silently. The
fixture exercises a self-referential enum (a tree with `Node(Box<...>)`
shape). The companion fixtures `recursive_generic_list` and
`recursive_generic_map` previously failed under the same banner; both
now pass once the LLVM layout phase started defining variant complete +
outer bodies in dependency order. `recursive_enum` still fails, so the
remaining root cause is likely IR-side payload projection through the
recursive variant rather than the layout phase.

`generics/recursive_struct` now sits in the same bucket once its
`Option.None` typecheck rewrite (see ["Closed: v1-permissive
Option.None inference"](#closed-v1-permissive-optionnone-inference))
landed: alpha typecheck and the interpreter backend both produce the
expected `1\n2\n3`, but the LLVM backend prints `1\n2\n0` — the
innermost recursive-struct field read returns zero. Same
payload-projection shape as the enum case (Node holds
`Option<Node>`, the inner `match n.next { Option.Some(n2) -> ... }`
arm reads a stale/empty payload). Fixing the recursive enum root
cause should close this one too.

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

## Closed: v1-permissive `Option.None` inference

Three fixtures relied on v1's backwards-flow inference for
`Option.None` in a generic position — alpha rejects with:

> alpha typecheck cannot infer type parameter `T` of `Global.Option`
> from unit variant `None`

Rewrite (landed 2026-05-13): bind `Option.None` to a typed local
on its first declaration, then read it back as the field value.
The struct constructor infers its own type parameter from the
field type instead of from `None` directly.

| Fixture                       | Annotation                       |
| ----------------------------- | -------------------------------- |
| `generics/nested_generics`    | `no_detail: Option<Int>`         |
| `generics/recursive_generic`  | `no_next: Option<GNode<Int>>`    |
| `generics/recursive_struct`   | `no_next: Option<Node>`          |

All three typecheck cleanly under alpha and produce the recorded
stdouts under v1 + alpha-interpreter. `recursive_struct` still
miscompiles under alpha LLVM (innermost field reads as `0`); see
["§2 Recursive payload miscompile"](#2-recursive-payload-miscompile-2-fixtures)
— the typecheck rewrite is correct; the LLVM miscompile is the
remaining gap.

## Closed: v1-permissive `.clone()` on copy types

`ownership/ownership_clone` called `.clone()` on `Int` (a copy
type) and on a plain `Point{ x: Int, y: Int }` struct (a move
type per the spec, but alpha hadn't synthesized `Clone` for it).

Rewrite (landed 2026-05-13):

- Dropped `m = n.clone()` for `Int` — `m = n` works in both v1
  and alpha since `Int` is a copy type.
- Added a manual `impl Point { fn clone(self) -> Point }` to the
  fixture, keeping the `q = p.clone()` shape working in both
  toolchains.

The fixture still demonstrates the original ownership shape
(consume `q`, then read `p.x` to prove the clone kept `p` alive).

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

## Closed: project-root FFI library search

The `ffi/` fixture's `@link "ffi_helper"` annotation expands to a
`cc -lffi_helper` flag, but the linker had no `-L` for the project
directory — so `libffi_helper.a` (sitting next to `expo.toml`) was
not discoverable unless the caller exported `LIBRARY_PATH=<dir>` by
hand. The `lang_suite.rs` `lang_ffi` harness already did this, which
masked the gap for the test runner; manual `expo run` / `expo alpha
run` from inside the project dir failed identically under both
pipelines.

Fixed by threading `extra_lib_search_paths` through
[`pipeline::link`](../crates/expo-driver/src/pipeline.rs) so
project-mode callers (`build_project`, `cmd_alpha_run_project`,
`cmd_alpha_build_project`, the `expo test` harness) pass the
directory holding `expo.toml`. The linker emits one extra `-L<root>`
after the embedded-runtime `-L`, so a sibling `libfoo.a` resolves
without any env juggling. Single-file `.expo` / `.exps` builds pass
`&[]` and keep their existing behavior.

`@link` still requires the user to build the static archive (`cc -c`
+ `ar rcs` for now); the compiler doesn't ship a build-script
phase. The fixture itself still relies on the harness or a manual
build step to produce `libffi_helper.a`.

---

## Priority order (cheapest unblock per fixture-count)

1. **PascalCase process entry** (gap #1) — 4 fixtures from one fix.
2. **`IntLiteral` + sized arithmetic** (mixed) — 1 fixture but unblocks
   the broader narrow-int story across stdlib.
3. **`Ref<M>` union substitution** (gap #3) — 2 fixtures.
4. **Wire `HTTP` into `ALPHA_QUALIFIED`** (gap #4) — 2 fixtures.
5. **Recursive payload miscompile** (gap #2) — 2 fixtures
   (`recursive_enum` + `recursive_struct`, same root cause).
6. **One-offs** (gap #5, #6) — 3 fixtures each fixed in isolation.

`Option.None` inference and `.clone()` on copy types are closed —
see the "Closed:" sections above. After (1)–(5) the lang suite is at
full alpha parity and `lang_suite.rs` can flip its runner off v1.
