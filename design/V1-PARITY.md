# V1 → Alpha Parity

What the alpha pipeline (`expo-parser` → `expo-alpha-typecheck` →
`expo-alpha-ir` → `expo-alpha-ir-llvm` / `expo-alpha-ir-eval`) still
needs to absorb the surface area v1 supports, so we can retire the
v1 pipeline. Replaces the stdlib-scoped
[`archive/20260511-ALPHA-ROADMAP.md`](archive/20260511-ALPHA-ROADMAP.md),
which served its purpose: every file under `lib/global/src/` plus
`Crypto` now compiles end-to-end through alpha.

This doc focuses on **what's left** — the language features v1 has
that alpha doesn't, and the qualified stdlib packages still gated on
those features. It is a parity ledger, not a sequencing plan; the
sequencing block at the bottom is a recommendation, not a contract.

---

## Status snapshot

- **Autoimport bundle (`lib/global/src/`)**: parity. 14 files, 2356
  LOC, all compile and run through alpha — bitwise, cptr, cstring,
  debug, fd, io, kernel, list, map, process, set, string, system,
  time. Plus alpha-only synthesis: `alpha_clone`,
  `alpha_debug_containers`.

- **Qualified packages (`lib/<pkg>/src/`)**:
  - `Crypto` — parity. Wired into `ALPHA_QUALIFIED` and exercised by
    `alias Crypto.Sha256` in driver tests.
  - `Json` — language-feature parity. String interpolation (shipped 20260513) was the last gap; ready to wire into `ALPHA_QUALIFIED`
    once the source files clean-compile end-to-end.
  - `Http` — language-feature parity. `String.clone()` (shipped
    20260512), field assignment (shipped 20260513), dotted type
    names (shipped 20260513), and string interpolation (shipped 20260513) all landed; ready to wire into `ALPHA_QUALIFIED` once
    the source files clean-compile end-to-end.
  - `Net` — language-feature parity. Type unions (shipped 20260513) was the last gap; ready to wire into
    `ALPHA_QUALIFIED` once the source files clean-compile end-to-
    end and `spawn` lights up (see "End-to-end concurrency
    execution" below).

- **End-to-end concurrency execution** — shipped 2026-05-13. See the
  "Verified parity" section below for the full breakdown. Alpha now
  spawns the user body as PID 1, runs `expo_rt_main_done()`, and
  implements `Ref.call` / `Ref.cast` / `Ref.send_after` against the
  runtime's `Pair<M, Option<ReplyTo<R>>>` envelope — the three
  `tests/lang/io/` goldens compile end-to-end. Authoring the
  goldens as runnable `.exps` scripts (no `fn main`, no
  `expo.toml`) is the lighter-weight shape going forward,
  especially for concurrency snippets where the v1 project +
  manifest overhead was the bulk of the file.

- **Project mode in `expo alpha`** — shipped 2026-05-13. `expo alpha
{check,build,run}` now read `expo.toml`, walk the manifest's `src/`
  tree (including dependencies), parse + bundle through
  `bundle_many_with_autoimport`, run alpha typecheck, then lower via
  `expo_alpha_ir::lower_program` and link via the same `pipeline::link`
  v1 uses. Single-file `.expo` programs share the project pipeline
  through a derived single-file package and an auto-resolved `main`
  entry. The legacy stub-error arms are gone. Required for any
  multi-file alpha test and for flipping `lang_suite.rs` off v1 once
  the remaining alpha gaps land.

---

## Audit method

For every `.expo` source under `tests/lang/` (the golden suite v1
ships against), grep for the language constructs it uses, then
cross-reference against the `"alpha (typecheck|IR|LLVM) does not
yet ..."` diagnostics in `crates/expo-alpha-*/src/`. Anything a
golden test reaches for that alpha diagnoses (or silently
mishandles) is a parity blocker.

The full list of "not yet" diagnostics lives in:

- `expo-alpha-typecheck/src/pipeline/collect.rs`
- `expo-alpha-typecheck/src/pipeline/lift_signatures/{functions,types,constants}.rs`
- `expo-alpha-typecheck/src/pipeline/resolve/{expr,statements,calls/mod,patterns/mod,literals/binary,closures,match_expr}.rs`
- `expo-alpha-typecheck/src/pipeline/seal/{expressions,patterns}.rs`
- `expo-alpha-ir/src/lower/{expr,ops,structs,enums,package,calls,closures}.rs`
- `expo-alpha-ir-llvm/src/{emit/mod,emit/instruction,main_wrapper}.rs`

---

## Gaps gating golden tests

After the 2026-05-13 concurrency + FFI + project-mode milestone, the
2026-05-13 `String.escape_debug()` fix below, and the 2026-05-13
match-arm-literal + LLVM-layout dependency-order fix below, no
language-feature blocker remains tracked here. Validating
`tests/lang/` against `expo alpha run --backend=llvm` now reports
48/66 PASS (script `expo/scripts/validate_alpha_lang.sh`); the
remaining 18 failures are individual alpha-IR / runtime bugs to
triage one-by-one (see [GOLDEN-GAPS.md](GOLDEN-GAPS.md)), not a
single regression class, and are out of scope for this ledger until
they coalesce into a pattern.

---

## Feature-gap diagnostics with no golden coverage

These all surface "alpha … does not yet support …" diagnostics, but
nothing in `tests/lang/` exercises them, so they're **not** on the
parity critical path. Listed for completeness — useful when the gap
inventory needs an audit pass.

| Gap                                                                                                                            | Diagnostic                                                                |
| ------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------- |
| Generic protocol methods (`fn m<U>(…)` inside `protocol`)                                                                      | `collect.rs:580`                                                          |
| `type` aliases inside `impl` blocks                                                                                            | `collect.rs:541`                                                          |
| Default parameter values                                                                                                       | `lift_signatures/functions.rs:409`                                        |
| Default field values                                                                                                           | `collect.rs:480`                                                          |
| Named arguments                                                                                                                | `calls/mod.rs:701`                                                        |
| Pattern destructuring assignment (`[a, b] = …`)                                                                                | `statements.rs:246`                                                       |
| Destructured closure parameters                                                                                                | `closures.rs:194`                                                         |
| Dynamic-width binary segments (`<<x::n>>` runtime `n`)                                                                         | `literals/binary.rs:132`                                                  |
| Binary patterns in `match`                                                                                                     | `patterns/mod.rs:154` (Phase 7 of `archive/20260511-ALPHA-MATCH-PLAN.md`) |
| List patterns in `match`                                                                                                       | `patterns/mod.rs:162`                                                     |
| Annotations on protocols, protocol methods, struct items, enum items, constants (other than `@doc` / `@intrinsic` / `@extern`) | `collect.rs:394,463,500,560,593`                                          |

---

## Verified parity (false positives I checked while writing this)

These look like they might be missing from a quick read of the
alpha source tree, but golden coverage confirms them shipped:

- **`for ... in` (statement position)** — desugared in
  `synthesize/for_desugar.rs`; pinned by `process_argv`.
- **`cond` and ternary `cond ? a : b`** — `cond_type_mismatch`
  exercises both arms.
- **Generic protocols** (`protocol Greeter<T>`) — `default_impl`.
- **Default protocol method bodies** — `default_impl` (`FancyGreeter`
  overrides; `HelloGreeter` inherits).
- **Field-as-callable dispatch** (`self.work()` against a fn-typed
  field) — `process` is in `ALPHA_AUTOIMPORT`; `Task<R>.run` lowers.
- **Aliases** (`alias Pkg.Type as X`) — `alias_dep`,
  `package_collision`.
- **`Clone` protocol** for heap primitives — shipped 2026-05-12;
  pinned by `crates/expo-alpha-typecheck/tests/clone.rs`.
- **Field assignment** (`p.x = 10`, depth-N, `self.f = v` under `move
self`, `p.x += 1`) — shipped 2026-05-13. Resolver walks the
  segment chain through nested struct definitions with type-arg
  substitution; IR adds `FieldSet` (SSA-pure rebuild) and
  `DropValue` (heap-leaf overwrite); both backends implement.
  Pinned by `crates/expo-alpha-typecheck/tests/field_assignment.rs`,
  `crates/expo-alpha-ir/tests/lower_field_assignment.rs`,
  `crates/expo-alpha-ir-eval/tests/field_assignment.rs`, and
  `crates/expo-alpha-ir-llvm/tests/field_assignment.rs`.
- **Dotted type names** (`Crypto.SHA256` and `HTTP.Headers` in
  signatures and as static-method receivers, without an `alias`
  line) — shipped 2026-05-13. `resolve_path_to_global` walks the
  full path through alias-rewrite → same-package → head-as-package
  → `Global.*` lookup; `classify_receiver` collapses the parser's
  `EnumConstruction Unit` shape, bare `Ident`, and `FieldAccess`
  chain shapes onto the same dotted-path lookup and rewrites the
  receiver to a synthetic `Ident` so IR lowering's existing static
  path picks it up. Pinned by the `dotted_*` tests in
  `crates/expo-alpha-typecheck/tests/aliases.rs` and the
  `dotted_type_in_signature_lifts_to_qualified_global` test in
  `crates/expo-alpha-typecheck/tests/lift_function_types.rs`.
- **String interpolation** (`"hello #{x}"` for any Debug-conforming
  `x`, no manual `.format()`) — shipped 2026-05-13. `resolve_string`
  resolves each interpolation, and any inner expression that isn't
  already `String`-typed is rewritten in place to
  `<original>.format()` and dispatched through the normal method-call
  resolver (mirroring the literal-carrier swap in
  `resolve/literals/carrier.rs`). `String`-typed interpolations are
  left bare to preserve the user's no-quote rendering (since
  `String.format` is the Debug repr `"\"" <> self.escape_debug() <>
"\""`). IR lowering, eval, and LLVM all see only `String`-typed
  parts and need zero changes. Pinned by the `string_interpolation_*`
  tests in `crates/expo-alpha-typecheck/tests/resolve_strings.rs`.
- **Type unions** (`A | B`, `type X = A | B`, typed-binding patterns
  `p: Post -> ...`) — shipped 2026-05-13. The lifter accepts
  `TypeExpr::Union` in every type slot (params, return slots, fields,
  let bindings, generic arguments) and canonicalizes to a sorted /
  deduplicated / flattened `ResolvedType::Union`. `type X = ...`
  registers a `GlobalKind::TypeAlias` so the alias name round-trips
  through diagnostics; equivalence peels through aliases so
  `Pet ≡ Cat | Dog | Fish`. `check_compatible` widens member types
  into union slots by stamping a `Coercion::UnionWiden(target)` on
  the source `Expr` (see the `Coercion` enum in
  `expo-ast/src/coercion.rs` for the 1:1 IR mapping). Match arms
  add `Pattern::TypedBinding` (`p: Member -> body`) which narrows
  the bound local to the member type and contributes to the
  union-aware exhaustiveness check; bare `FieldAccess` /
  `MethodCall` against a union receiver surface a precise
  diagnostic that points the user at `match`. IR adds
  `IRType::Union { mangled, members }` plus a per-program
  `IRUnionDecl` registry sized for the largest member; lowering
  emits `IRInstruction::UnionWrap` (the boxing op),
  `IRInstruction::UnionTagGet`, and `IRInstruction::UnionPayloadGet`
  in 1:1 correspondence with the typecheck-side `Coercion` /
  `TypedBinding` shapes. The LLVM backend lays each union out as
  `{ i8 tag, [N x i8] payload }` (declared in a phase that runs
  before struct / enum bodies so a struct can carry a union-typed
  field); the eval interpreter materializes a `Value::Union {
symbol, tag, payload }` and dispatches `UnionWrap` / `TagGet` /
  `PayloadGet` against it. Pinned by
  `crates/expo-alpha-typecheck/tests/resolve_unions.rs` and
  `tests/resolve_type_aliases.rs`,
  `crates/expo-alpha-ir/tests/lower_unions.rs`,
  `crates/expo-alpha-ir-eval/tests/unions.rs`, and
  `crates/expo-alpha-ir-llvm/tests/unions.rs`.
- **Tail-call optimization** — shipped 2026-05-13. A new
  `IRTerminator::TailCall { callee, args }` variant carries the
  control-flow signal explicitly; the post-merge
  `expo-alpha-ir/src/tail_calls.rs` rewrite walks every regular
  function and replaces any `Call + Return` shape (with arbitrary
  trailing exit drops) into a `TailCall` whenever the callee is
  the enclosing function and the returned value matches the call's
  dest. Cross-function tail calls aren't admitted yet (the
  rewrite's self-callee gate is one line to relax once a backend
  musttail emit lands). The seal pass asserts `TailCall.callee`
  matches the enclosing function's symbol and `args.len()` matches
  the param arity. The LLVM backend installs a per-function
  `tco_loop` block when any block in the function carries a
  `TailCall`: param promotion stays in the LLVM entry block and
  branches to `tco_loop`, the body emits into `tco_loop`, and each
  `TailCall` lowers to a `store-args + br tco_loop` against the
  matching parameter slot's `alloca` (no per-iteration stack
  growth — `EmitContext::build_entry_alloca` already pulls the
  slot to the LLVM entry). The eval interpreter wraps
  `execute_function` in a trampoline that surfaces a `TailCall`
  out of `execute_blocks` as a `BlockOutcome::TailRestart(args)`,
  rebuilds the frame with the new args, and re-walks the body.
  Both backends drive 100 000-deep recursion (the v1 golden) in
  constant host stack. Pinned by the `tail_calls::tests` lib unit
  tests in `expo-alpha-ir`,
  `crates/expo-alpha-ir-eval/tests/tail_calls.rs`, and
  `crates/expo-alpha-ir-llvm/tests/tail_calls.rs`.
- **FFI surface (`@extern "C"` + `CPtr<T>`)** — shipped 2026-05-13.
  See §7 for the full breakdown. Pinned by the `extern_*` snapshots
  in `crates/expo-alpha-ir-llvm/tests/extern.rs` and the `@extern
"C"` + `self` rejection in
  `crates/expo-alpha-typecheck/tests/extern_c.rs`.

- **End-to-end concurrency execution** — shipped 2026-05-13. See §8
  for the full breakdown (spawn-driven `main` trampoline, `Ref.call`
  - `Pair<M, Option<ReplyTo<R>>>` envelope, protocol default-method
    type-param substitution). Pinned by the `ref_*` snapshots in
    `crates/expo-alpha-ir-llvm/tests/process.rs`.

- **Infinite `loop` + `break`** — shipped 2026-05-13. `Resolver`
  threads a `loop_depth` counter and a `loop_break_seen: Vec<bool>`
  stack; `resolve_loop` types the loop as `Never` when no targeted
  `break` was seen (the body is divergent — only `return` / `panic`
  paths) and `Unit` otherwise. `resolve_while` bumps the same
  fields so its body's `break` is gated, but keeps its own `Unit`
  return type (the cond can fall through). The walker gates
  `Statement::Break` on `loop_depth > 0` with `"break outside of
loop"`. Closure boundaries save/restore both fields, so an inner
  `break` can never bleed up to an outer-function loop. IR lowering
  threads a `loop_exit: Vec<IRBlockId>` stack on `FnLowerCtx`;
  `lower_loop` emits `loop_body` / `loop_exit` blocks with a self
  back-edge, and `lower_break_stmt` terminates the open block with
  a `Branch` to the innermost exit. The for-desugar's `__idx = __len`
  exit hack was replaced with a real `Statement::Break`. Pinned by
  the `loop_*` / `break_*` tests in
  `crates/expo-alpha-typecheck/tests/resolve_loops.rs`,
  `crates/expo-alpha-ir/tests/lower_loops.rs`,
  `crates/expo-alpha-ir-eval/tests/loops.rs`, and
  `crates/expo-alpha-ir-llvm/tests/loops.rs`.

- **Match-arm literal / partial / nested struct + enum patterns** —
  shipped 2026-05-13. Phase 4 of `archive/20260511-ALPHA-MATCH-PLAN.md`
  had restricted struct-field and enum-payload positions to wildcards
  and bindings while the container-level machinery shipped. The
  resolver gates (`is_admitted_field_element` /
  `is_admitted_tuple_element`) are gone; pattern lowering now emits a
  chained `BindStep` extraction (`EnumPayloadField` / `StructField` /
  `UnionPayload`) followed by the inner pattern's own check against
  the projected value, wired through `match_and_field` blocks under
  `ChainMode::And`. Same time, the LLVM layout phase started defining
  enum variant complete + outer bodies in dependency order — a
  stdlib enum like `Option<TestApp.TokenKind>` previously sized its
  outer to `[1 x i8]` when `TokenKind` was in a later package, since
  `get_abi_size` on an opaque inner returned 0. The new
  `layout/enum_order.rs` topo-sort guarantees every transitive
  variant payload reference is bodied before its outer chunk is
  computed. Pinned by the new `match_nested_*` /
  `nested_enum_*` tests in
  `crates/expo-alpha-typecheck/tests/resolve_match.rs`,
  `crates/expo-alpha-ir/tests/lower_match.rs`,
  `crates/expo-alpha-ir-eval/tests/match_nested.rs`,
  `crates/expo-alpha-ir-llvm/tests/match_nested.rs`, and the
  `types/struct_pattern_*` + `types/nested_enum_pattern_literal`
  goldens. The layout-ordering fix also unblocks
  `collections/json_value`, `generics/recursive_generic_list`,
  `generics/recursive_generic_map`, `io/multi_process`,
  `io/spawn_process`, and `types/cast_loop` as a bonus — every
  fixture that depended on a cross-package generic enum sized
  correctly.

- **`String.print()` / `String.escape_debug()` runtime crash** —
  shipped 2026-05-13. The SIGABRT was a cross-arm slot-state leak
  in `lower_assignment`: `FnLowerCtx::locals` was function-flat, so
  lowering a `match` whose arms each `result = result <> "..."`
  inherited the prior arm's `Owned` stamp and synthesized a
  `DropLocal` against the slot's still-Unowned literal at the
  arm's body block. Fixed by snapshotting the per-slot state at
  every control-flow construct boundary (`match` / `cond` / `if` /
  `unless` / ternary), restoring before each arm, and merging
  per-arm post-states with a conservative join: a slot's
  `ownership` adopts the per-arm stamp only when every branch
  agreed, else falls back to `Unowned`; `moved` is the AND across
  branches. Same time, the auto-print scaffolding (
  `Compiler::emit_print_call`, `__expo_alpha_print_{i64,bool,f32,
f64,binary,bits}`, `Interpreter::format_via_debug`'s driver
  call) was retired: scripts and programs now always exit `0` on
  normal completion and user code is responsible for its own
  output via `IO.puts` / `.print()`. The `__expo_user_main`
  spawn thunk caps with `ret void` and the `@main` trampoline
  caps with `ret i64 0`. Pinned by
  `crates/expo-alpha-ir/tests/lower_drops.rs`
  (`match_arms_writing_owned_emit_no_droplocal_when_pre_state_unowned`,
  `cond_arms_writing_owned_emit_no_stale_droplocal`,
  `match_arms_writing_owned_merge_to_owned_when_every_arm_agrees`),
  `crates/expo-alpha-ir-eval/tests/escape_debug.rs`,
  `crates/expo-alpha-ir-llvm/tests/escape_debug.rs`, and the new
  `tests/lang/basics/string_match_escape.expo` golden.

---

## Recommended sequencing

Roughly cheapest → most expensive, weighted by what each step
unblocks. Each step lands behind seal-asserted output and standalone
tests, per northstar.

### 1. Field assignment (`p.x = 10`) — shipped 2026-05-13

Single statement-resolve gap; multi-segment `LValue` lifted via a
new resolver chain walker plus a `FieldSet` IR instruction (SSA-
pure rebuild) and a value-keyed `DropValue` for heap-leaf overwrite.
Unblocked `structs.expo` and removed every alpha gap from the
`Http` package except dotted type names (#2).

### 2. Dotted type names in expr + type position — shipped 2026-05-13

`Foo.Bar` in type annotations and `Foo.Bar.method()` as a static
receiver, both without an `alias` line. Resolver gate widened in
`resolve_path_to_global` (alias → same-package → head-as-package →
`Global` precedence) and `classify_receiver` (collapses the
parser's three receiver shapes onto a unified dotted-path lookup,
then rewrites the receiver to a synthetic `Ident` for downstream
lowering). Unblocked the `qualified_*` golden tests and removed
every alpha-side language gap from the `Http` package.

### 3. String interpolation (`"hello #{x}"`) — shipped 2026-05-13

Single resolver patch in `resolve_string`: each `StringPart::Interpolation`
gets its inner expression resolved, and anything that isn't already
`String`-typed is wrapped in a synthetic `<original>.format()`
MethodCall and dispatched through the normal method-call resolver.
`String`-typed interps are left bare so `"hello #{name}"` renders
`hello world`, not `hello "world"` (since `String.format` is the
Debug repr that adds quotes). IR/eval/LLVM see only `String`-typed
parts; the existing `lower_string` `Concat::String` chain handles
the rest with zero backend changes. Unblocked the ~25 `tests/lang/`
files that interpolate without manual `.format()` and removed the
last alpha-side language gap from the `Json` package.

### 4. Infinite `loop` + `break` — shipped 2026-05-13

`ExprKind::Loop` resolves through `resolve_loop`, typing as `Never`
when the body has no targeted `break` and `Unit` when at least one
`break` fires (a syntactic check via the new `loop_break_seen`
stack on `Resolver`). `break` is gated on `loop_depth > 0` with
`"break outside of loop"`; closure boundaries reset both fields so
an inner `break` can't reach an outer-function loop. IR lowering
threads a `loop_exit` stack on `FnLowerCtx`; `lower_loop` emits
`loop_body` / `loop_exit` blocks with a self back-edge, and
`lower_break_stmt` terminates the open block with a `Branch` to
the innermost exit. Backends needed zero changes. The for-desugar's
`__idx = __len` exit hack was retired in favor of a real
`Statement::Break`. `continue` is **not** in v1 either, so it
isn't a parity gap — left as a future language extension.

### 5. Type unions (`A | B`, `type X = A | B`, typed-binding patterns) — shipped 2026-05-13

Lifted `ResolvedType::Union(Vec<ResolvedType>)` with canonical
member ordering / dedupe / flattening, added a
`GlobalKind::TypeAlias` registry slot for `type X = ...` so
diagnostics keep the alias name, threaded
`Compatible::UnionWiden { target }` through `check_compatible` at
every type-equality site (call args, struct fields, return slot,
let bindings, enum tuple payloads), added
`Pattern::TypedBinding` resolution + union-aware exhaustiveness
in `match`, and surfaced bare-receiver diagnostics on field
access / method calls so the user is steered at `match`. IR
introduced `IRType::Union { mangled, members }` plus a
program-level `IRUnionDecl` registry sized for the largest
member and the matching `IRInstruction::UnionWrap` /
`UnionTagGet` / `UnionPayloadGet` ops; the LLVM backend lays
unions out as `{ i8 tag, [N x i8] payload }` (declared before
structs / enums so struct fields can carry union types) and the
eval interpreter materializes `Value::Union { symbol, tag,
payload }`. Coercions are stamped through a single
`pub coercion: Option<Coercion>` field on `Expr` whose variants
map 1:1 to `IRInstruction` ops, per the northstar coercion
contract. Unblocks the `Net` package and the
`union_types` / `union_named` / `union_typed_binding` /
`union_struct_field` / `process_union_msg` golden tests.

### 6. Tail-call optimization — shipped 2026-05-13

A new `IRTerminator::TailCall { callee, args }` variant carries
the control-flow signal explicitly, populated by a post-merge
`expo-alpha-ir/src/tail_calls.rs` rewrite that walks every
regular function and replaces self-recursive `Call + Return`
shapes (with arbitrary trailing exit drops) into `TailCall` —
the callee match keeps the rewrite scoped to self-recursion for
now, and the seal pass enforces both that and the arg-arity
match against `function.params`. The LLVM backend installs a
per-function `tco_loop` block when any block in the function
carries a `TailCall`: param promotion stays in the LLVM entry
block and branches to `tco_loop`, the body emits into
`tco_loop`, and each `TailCall` lowers to a `store-args + br
tco_loop` against the matching param slot's existing
entry-block `alloca` (no per-iteration stack growth). The eval
interpreter wraps `execute_function` in a trampoline that
surfaces a `TailCall` out of `execute_blocks` as
`BlockOutcome::TailRestart(args)`, rebuilds the frame, and
re-walks the body. Both backends drive 100 000-deep recursion
(`tail_call.expo` / `tail_call_unit.expo`) in constant host
stack. The IR-level vocabulary is backend-agnostic — extending
to cross-function tail calls is a one-line relaxation of the
self-callee gate plus a backend musttail emit.

### 7. FFI surface (`@extern "C"` + `CPtr<T>` marshaling) — shipped 2026-05-13

Verified end-to-end through alpha. `@extern "C"` declarations parse
through the existing annotation pipeline, lift into IR as
`@extern`-flagged `IRFunction`s with no body, and lower to LLVM
`declare`s with the C-ABI `i32` / `i64` / `ptr` mapping (no Expo
prelude wrapping). `CPtr<T>` parameters and arguments lower to
`ptr` directly with no marshaling shim — the call site forwards the
pointer value verbatim, matching v1. Pinned by the new
`extern_with_cptr_arg_declares_pointer_param` and
`call_to_extern_passes_cptr_as_ptr_with_no_marshaling` snapshots in
`crates/expo-alpha-ir-llvm/tests/extern.rs` (alongside the existing
`extern_c_with_link_lib_declares_external_function` pin), plus the
`@extern "C"` + `self` rejection in
`crates/expo-alpha-typecheck/tests/extern_c.rs`. The
`tests/lang/ffi/` golden compiles and links cleanly through
`expo alpha build --backend=llvm` once `libffi_helper.a` is in
`LIBRARY_PATH`.

### 8. End-to-end concurrency execution — shipped 2026-05-13

Three intertwined changes:

1. **Spawn-driven `main` trampoline**
   (`expo-alpha-ir-llvm/src/main_wrapper.rs`). The user body now
   lives in a `void __expo_user_main(i8*)` thunk, and the host
   `i64 main()` is a one-call trampoline:
   `expo_rt_spawn(__expo_user_main, null, 0)` registers the body as
   PID 1, `expo_rt_main_done()` boots the I/O reactor + scheduler
   and runs until PID 1 dies, then `ret i64 0`. Required for
   `expo_rt_self()`, `Ref.call`, `Ref.cast`, and every other
   primitive that reads `CURRENT_PID` — pre-refactor the body ran
   inline in `main()` with `CURRENT_PID == -1`, which crashed any
   concurrency code that reached `expo_rt_self()` in the runtime's
   process table. Pinned indirectly by the spawn-from-`main`
   integration tests in `crates/expo-alpha-ir-llvm/tests/process.rs`.
2. **`Ref.call` + `Pair<M, Option<ReplyTo<R>>>` envelope**
   (`expo-alpha-ir-llvm/src/intrinsics/process.rs`). `Ref.cast`
   and `Ref.send_after` now build the same `Pair` envelope the
   runtime's `expo_rt_receive_*` paths and the `Receive`
   instruction emit expect, with `Option::None` for the reply
   slot (`[2 x i64] [i64 1, i64 0]` after constant-folding).
   `Ref.call` allocates the same `Pair`, fills the reply slot
   with `Option::Some(ReplyTo { id: expo_rt_self() })`, sends via
   `expo_rt_send`, then `expo_rt_receive_timeout(timeout_ms)` to
   wait for a `Result<R, CallError>` reply, branching to either a
   deserialized `Result.Ok(R)` or the appropriate
   `CallError.Timeout` / `CallError.ProcessDown` arm. Pinned by
   the `ref_cast_emits_pair_envelope_with_none_reply_to_…` /
   `ref_send_after_emits_pair_envelope_…` /
   `ref_call_emits_pair_envelope_with_some_reply_to_and_receive_loop`
   snapshots in `crates/expo-alpha-ir-llvm/tests/process.rs`. The
   `IRSymbol → IRStructDecl/IREnumDecl` lookup table was extended
   with a per-enum `IRVariantPayload` registry
   (`expo-alpha-ir-llvm/src/layout.rs` + `layout/enums.rs`) so
   `Ref.call` can pull the `R` type out of `Result<R, CallError>`
   without re-walking mangled names.
3. **Protocol default-method type-param substitution**
   (`expo-alpha-typecheck/src/pipeline/lift_signatures/impls.rs`).
   When `synthesize_default_method` clones a default body off a
   protocol into an impl (e.g. `Process<C, M, R>.run` → `impl
Process<WorkerConfig, WorkerMsg, String>`), it now walks the
   body's `TypeExpr` references and rewrites the protocol's
   declared type-params (`M`, `R`, …) to the impl's pinned
   arguments. Without this, the default `run` body's
   `pair: Pair<M, Option<ReplyTo<R>>>` typed-binding pattern would
   still mention bare `M` / `R` after lift, and the resolver would
   reject them as unrecognized type names (no protocol-type-param
   scope outside the original `protocol P<…>` declaration).
   Substitution covers param signatures, return types, and every
   `TypeExpr` inside the body (match arms, receive arms, closures,
   `let` annotations, generic argument lists, function types,
   union members, struct/enum patterns). Mirrors the v1 collector's
   `substitute_named_in_*` shape.

These three together unblock the `tests/lang/io/{spawn_process,
multi_process, process_union_msg}` fixtures in alpha.

### 9. `String.print()` / `String.escape_debug()` runtime crash —

shipped 2026-05-13. See the "Verified parity" section below for
the full breakdown.

### Future direction: prefer `.exps` for new goldens

Alpha runs `.exps` scripts directly (no `fn main`, no `expo.toml`),
v1 doesn't. Going forward, new goldens for `tests/lang/` should
prefer the `.exps` shape: `tests/lang/<feature>.exps` +
`tests/lang/<feature>.stdout`, collapsing the per-fixture overhead
from a project directory to two files. Existing `.expo` goldens
stay until they're touched for other reasons.

After (1)–(9) the alpha pipeline is at v1 surface parity for
`tests/lang/` plus all qualified stdlib packages, and the v1
toolchain (`expo-typecheck`, `expo-codegen`) can be removed.
