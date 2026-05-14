# Lang-suite golden gaps

A triage of the `tests/lang/` fixtures that fail under
`expo alpha run --backend=llvm`. Companion to
[V1-PARITY.md](V1-PARITY.md) — that doc tracks the closed parity items;
this doc enumerates what's still open and groups failures by root cause so
fixing one entry unblocks a known cluster of fixtures.

PASS count as of last run (2026-05-14): `58 passed, 8 failed, 1 skipped`
via [scripts/validate_alpha_lang.sh](../scripts/validate_alpha_lang.sh).
`process_lifecycle` is the skipped fixture (signal-driven, intentionally
excluded). The `ffi` fixture only links when
[`just build-ffi-fixture`](../justfile) has run first — without that
prerequisite step the script counts it as a failure even though every
language-level concern is closed (see
["Closed: project-root FFI library search"](#closed-project-root-ffi-library-search)).
That leaves **eight** real open failures, clustering into the four
root causes below. The previous count of nine dropped to eight when
binary pattern matching shipped (typecheck + IR + LLVM) — see
["Closed: binary pattern matching"](#closed-binary-pattern-matching)
for the parity item.

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

### 2. Recursive payload miscompile under LLVM (2 fixtures)

`generics/recursive_enum` — built binary exits 133 silently. The
fixture exercises a self-referential enum (a tree with `Node(Box<...>)`
shape). The companion fixtures `recursive_generic_list` and
`recursive_generic_map` previously failed under the same banner; both
now pass once the LLVM layout phase started defining variant complete +
outer bodies in dependency order. `recursive_enum` still fails, so the
remaining root cause is likely IR-side payload projection through the
recursive variant rather than the layout phase.

`generics/recursive_struct` sits in the same bucket once its
`Option.None` typecheck rewrite (see ["Closed: v1-permissive
Option.None inference"](#closed-v1-permissive-optionnone-inference))
landed: alpha typecheck and the interpreter backend both produce the
expected `1\n2\n3`, but the LLVM backend prints `1\n2\n0` — the
innermost recursive-struct field read returns zero. Same
payload-projection shape as the enum case (Node holds
`Option<Node>`, the inner `match n.next { Option.Some(n2) -> ... }`
arm reads a stale/empty payload). Fixing the recursive-enum root
cause should close this one too.

### 3. Sized-int arithmetic + `IntLiteral` widening (1 fixture)

`basics/int_coercion` stacks two v1-isms that the pending sized-int
arithmetic + `IntLiteral` protocol work absorbs:

1. `Counter.add` body uses `Int32 + Int32` arithmetic; alpha rejects
   sized-int arithmetic. Closes once `binary_type` / `unary_type`
   generalize (Phase 1 of that plan).
2. `x: Int32 = identity(42)` widens an `Int` literal through a generic
   return; alpha won't unless `IntLiteral<T>` ships (Phase 2).

One fixture but the broader narrow-int story across stdlib rides on
the same plan, so this entry is high-leverage even though the
fixture count is small.

### 4. `Equality` not synthesized for nested enum payloads (1 fixture)

`types/nested_enum_eq` exercises `Option<Color> == Option<Color>` and
similar enum-of-enum equality. Alpha rejects with:

> `==` requires matching Bool, Float, Int, or String operands; got
> `Option<Color>` and `Option<Color>`

`Equality` synthesis bails when a generic enum's variant payload is
itself a (different) enum, so the recursive equality call never gets
generated. Likely fix: thread the synthesis pass through generic enum
payloads so `Option<E>.eq` recurses into `E.eq` whenever `E:
Equality`. Same one-fix-unblocks-everything shape as the existing
`derive_debug` recursion — that pass already handles this for the
Debug side.

---

## Closed since this doc was last rewritten

The four entries below were open gaps in earlier passes of this
document. Each is now a closed parity item with a runnable fixture
or regression test pinning it.

### Dotted type names in expr + type position (2 fixtures)

`types/qualified_signature` + `types/qualified_static_call` were
blocked on `HTTP.Headers` (and similar) not parsing without an
`alias` line. Resolver gate widened in `resolve_path_to_global`
(alias → same-package → head-as-package → `Global` precedence) and
`classify_receiver` collapsed the parser's three receiver shapes
onto a unified dotted-path lookup. Closed alongside the broader
`Http` package wiring — see [V1-PARITY
§2](V1-PARITY.md#2-dotted-type-names-in-expr--type-position--shipped-2026-05-13).

### Generic function-pointer field as callee (1 fixture)

`functions/fn_generic_arg` exercised `wrapper.f()` where `f` is an
anonymous-function-typed field on a generic struct. IR lower
asserted "instance method receiver resolved to non-Global type
(Anonymous(Function { … }))"; closed by the
call-shape work that taught lower to dispatch
function-pointer-typed receivers as indirect calls rather than
method calls.

### Wildcard closure parameters (1 fixture)

`functions/short_closures` panicked in IR lower:

> alpha IR lower: closure param #0 (Wildcard { … }) is not yet
> supported in lowering

Fixed (commit a4e49be, 2026-05-14) by stamping a unique `LocalId`
on every `ClosureParam::Wildcard` in typecheck (new
`LocalScope::declare_anonymous`) so the lowerer can route it
through the same path as named params.

### `Ref<M>` narrowing for union message types (2 fixtures)

Two fixtures (`io/process_union_msg`, `types/union_struct_field`)
exercise the `Process<C, M, R>` shape with `M = MsgA | MsgB`.
`spawn Parent.start(...)` produces a `Ref<MsgA | MsgB, _>` (the
receiver scope pre-binds `M → MsgA | MsgB`); the follow-up
`ref.call(MsgA.Ping(...), 5000)` then drives the method-arg
unifier to bind `M → MsgA`. `Substitution::set` flagged the
second bind as a conflict and emitted

> type parameter `M` of `Global.Ref` cannot be both `MsgA | MsgB`
> and `MsgA`

Fix (landed 2026-05-14): extend the slot re-fill check in
[`pipeline/unify::Substitution::set`](../crates/expo-alpha-typecheck/src/pipeline/unify.rs)
with a one-direction `union_contains` helper — if the slot already
holds a `ResolvedType::Union` and the incoming actual is
[`types_equivalent`] to one of its members, accept the re-fill and
keep the wider slot intact. The reverse direction (widening a
narrower slot to a later union arrival) belongs to
`fill_from_expected`, not the per-arg path, so it stays
unchanged.

Pinned by
[`tests/process.rs::ref_call_accepts_union_member_arg`](../crates/expo-alpha-typecheck/tests/process.rs)
plus a negative companion that keeps the "cannot be both"
diagnostic firing when an arg sits outside the declared union.

### Binary pattern matching

`<<segments>>` in `match` arms — previously stubbed as
"alpha typecheck does not yet support binary patterns" in the
resolver. Now flows end-to-end:

- **Typecheck** (`expo-alpha-typecheck`): new
  [`pipeline/resolve/patterns/binary.rs`](../crates/expo-alpha-typecheck/src/pipeline/resolve/patterns/binary.rs)
  validates segments (literal-only, sized-int bindings with
  `signed` / `unsigned` / `big` / `little` modifiers, typed
  bindings via `Int8`..`UInt64`, string-literal segments, greedy
  `: Binary` / `: Bits` tails, `_::N` discards). Stamps
  `Resolution::Local` onto binding `Expr`s and registers their
  types in the arm scope so seal sees a fully-resolved AST.
- **IR** (`expo-alpha-ir`): new `LoweredBinaryPattern` /
  `LoweredBinaryMatchLayout` IR types and an
  `IRInstruction::BinaryMatch` instruction. Lower in
  [`lower/binary_match.rs`](../crates/expo-alpha-ir/src/lower/binary_match.rs)
  re-classifies each segment, computes its `bit_offset`, and
  declares the binding's `LocalDecl` in the entry block.
- **LLVM** (`expo-alpha-ir-llvm`): new
  [`emit/binary_match.rs`](../crates/expo-alpha-ir-llvm/src/emit/binary_match.rs)
  emits the length check (`EQ` exact / `UGE` with greedy tail),
  the byte-shift extract loop, the `signed` sign-extend (`shl` +
  arithmetic `ashr` — fixes v1's latent bug where the modifier
  was ignored), and the greedy-tail `malloc` + `memcpy` block.
  String-literal segments route through `memcmp`.

Pinned by
[`tests/resolve_binary_pattern.rs`](../crates/expo-alpha-typecheck/tests/resolve_binary_pattern.rs)
(13 typecheck cases including the negative paths for dynamic
widths, byte units, float extracts, and bit-misaligned tails)
and
[`tests/binary_match.rs`](../crates/expo-alpha-ir-llvm/tests/binary_match.rs)
(6 IR-text snapshot cases including the sign-extend pinning).

Out of scope (rejected with diagnostics, deferred): dynamic
sizes (`x::n`), `::N byte` / `::N size` units, float-extract
bindings (`<<x: Float32>>`), and bit-misaligned `: Binary`
greedy tails.

### `lib/global` Task / Ref<Unit, R> instantiations

`lib/global/test/task_test.expo` and the wider `Task<R>` /
`Ref<Unit, R>` shapes failed under the LLVM backend with a chain
of monomorphization + Unit-as-value gaps:

1. **Static generic method calls not enqueued** — `Task.async(...)`
   never lowered the receiver expression, so the
   `(method_id, receiver_args)` instantiation never reached the
   worklist and `seal_program_calls` panicked with
   > function `Global.TaskTest.test_async_await` calls
   > `Global.Task_$Int64$.async`, but that function is not
   > registered in the IRProgram
2. **`receive`-arm typed-binding patterns kept raw `TypeParam`
   leaves** — `pair: Pair<(), Option<ReplyTo<R>>>` retained an
   unsubstituted `R` after `Task.run`'s monomorphization,
   panicking downstream in `resolved_type_to_ir_type`.
3. **Synthesized spawn wrappers stranded** — mono'd bodies that
   minted closures / spawn wrappers via `lower_function_inner`
   landed in `output.synthesized_functions`, but the per-package
   drain ran once before mono, so subsequent wrappers were
   silently dropped (next seal pass complained "no spawn wrapper
   with that symbol is registered").
4. **Eager `Debug`-impl monomorphization on Unit-pinned generics
   ** — `enqueue_member_methods` greedily mono'd every method
   reachable from `Pair`, including the `Debug.format` impl that
   recurses into `A.format()` without an `A: Debug` bound,
   exploding on a `Pair<Unit, …>` instance.
5. **`IRType::Unit` in value position** — LLVM signatures, local
   allocas, struct fields, and the `Ref<M, R>` intrinsic
   emitters all rejected `Unit` outright, so even with the
   monomorphization fixes the Unit-pinned `Ref<(), R>.cast`
   couldn't lay out.

Fixes (landed 2026-05-14):

- [`lower/calls.rs`](../crates/expo-alpha-ir/src/lower/calls.rs)
  pushes a method-targeted `Instantiation` whenever the call
  carries receiver or method type-args. Mirrors what
  `resolved_type_to_ir_type` already does for instance dispatch.
- [`generics/substitute.rs`](../crates/expo-alpha-ir/src/generics/substitute.rs)
  adds `substitute_in_pattern` and threads it through the
  `ExprKind::Receive` walk so typed-binding payload types get
  rewritten.
- [`generics/mod.rs`](../crates/expo-alpha-ir/src/generics/mod.rs)
  drains `output.synthesized_functions` after every
  `monomorphize` step and routes each to the matching
  `IRPackage` by symbol prefix.
- [`generics/monomorphize.rs`](../crates/expo-alpha-ir/src/generics/monomorphize.rs)
  filters protocol-impl method names out of
  `enqueue_member_methods`. Protocol methods stay on-demand via
  `lower_method_call`'s push.
- [`types.rs::value_basic_type`](../crates/expo-alpha-ir-llvm/src/types.rs)
  maps `IRType::Unit` to an `i8` placeholder; routed through
  `function_signature` (params), `emit_local_decl` /
  `emit_local_read` (locals), `define_struct_body` (fields),
  and the `Ref<M, R>` envelope emitters in
  [`intrinsics/process.rs`](../crates/expo-alpha-ir-llvm/src/intrinsics/process.rs).
  `emit_const_instruction` binds Unit constants to `i8 0` so
  call-site lookups resolve cleanly.

Pinned by
[`tests/generics.rs`](../crates/expo-alpha-ir/tests/generics.rs)
(`static_call_on_generic_struct_registers_mono_method` and
`receive_arm_typed_binding_substitutes_payload_type_during_mono`)
and
[`tests/process.rs::ref_cast_with_unit_message_uses_i8_placeholder_in_envelope`](../crates/expo-alpha-ir-llvm/tests/process.rs).
All 163 `lib/global` tests now pass.

### Shortest-round-trip float `Debug.format` (1 fixture)

`protocols/debug_format` previously diverged: v1 printed
`"3.140000"` (`snprintf("%f")`'s legacy 6-digit fixed precision),
alpha printed `"3.14"` (Rust's `{:?}` — shortest round-trip via
Grisu/Ryu). Alpha's form is the right default: it round-trips
exactly (`parse(format(x)) == x` for every finite `f64`), it
doesn't fake precision with meaningless trailing zeros, and it
doesn't silently round away digits past the 6th decimal — the same
choice Rust, Go, modern JS, Python `repr`, Swift, etc. all make.

Fix (landed 2026-05-14): route v1's `Debug.format` for `Float` /
`Float32` through the same `expo_format_f64` / `expo_format_f32`
runtime helpers alpha uses, instead of inlining `snprintf("%f")`.
One source of truth for both backends, and the `.stdout` golden
updated to the round-trip form (`"3.14"`).

---

## Closed: v1-permissive idioms (rewrites, not compiler fixes)

The five fixtures below relied on v1 looseness alpha intentionally
rejects. Each was rewritten to a shape both pipelines accept — the
alpha-side surface area is unchanged.

### `Option.None` inference (3 fixtures)

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
miscompiles under alpha LLVM (innermost field reads as `0`) — see
[§2 Recursive payload miscompile](#2-recursive-payload-miscompile-under-llvm-2-fixtures);
the typecheck rewrite is correct, the LLVM miscompile is the
remaining gap.

### `.clone()` on copy types (1 fixture)

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

### Opaque `Debug` receivers for anonymous types

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
phase. Run [`just build-ffi-fixture`](../justfile) before the
validator script (or any manual `expo alpha run` inside
`tests/lang/ffi/`) so `libffi_helper.a` is present when the linker
goes looking. `cargo test ... lang_ffi` still cleans the archive
up after itself, so the manual step is needed any time the script
is the first thing to touch the fixture.

---

## Priority order (cheapest unblock per fixture-count)

1. **PascalCase process entry** (gap #1) — 4 fixtures from one fix.
2. **Recursive payload miscompile** (gap #2) — 2 fixtures
   (`recursive_enum` + `recursive_struct`, same root cause).
3. **`IntLiteral` + sized arithmetic** (gap #3) — 1 fixture but
   unblocks the broader narrow-int story across stdlib.
4. **`Equality` synthesis for nested enums** (gap #4) — 1 fixture,
   small targeted recursion fix in the synthesis pass.

After (1)–(4) the lang suite is at full alpha parity and
`lang_suite.rs` can flip its runner off v1.
