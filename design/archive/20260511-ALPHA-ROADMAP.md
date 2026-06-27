# Alpha Roadmap

Sequencing for getting `expo/lib/global/src/` compiling end-to-end through
the alpha pipeline (`koja-alpha-typecheck` → `koja-alpha-ir` →
`koja-alpha-ir-llvm` / `koja-alpha-ir-eval`).

The goal is **stdlib parity, not v1 parity**: alpha needs the surface area
that `lib/global/src/*.koja` exercises. Anything outside the auto-imported
package (Net, HTTP, JSON, …) is explicitly deferred — the stdlib stays
enum-first and structurally as-is; this doc just enumerates which compiler
features have to land for it to type-check and lower.

For pipeline shape and seal contracts, see
[COMPILER-NORTHSTAR.md](COMPILER-NORTHSTAR.md). For non-alpha v1 gaps, see
[GAPS.md](GAPS.md).

---

## Goal: compile the stdlib

Concrete success criterion: `koja alpha check` and `koja alpha run` (where
applicable — concurrency primitives stay stubbed) succeed on every file
under `expo/lib/global/src/`:

```
bitwise.koja  cptr.koja    cstring.koja  debug.koja  fd.koja
io.koja       kernel.koja  list.koja     map.koja    process.koja
set.koja      string.koja  system.koja   time.koja
2356 LOC total
```

Today they don't, by a lot. The audit below enumerates why.

---

## Audit method

For each `.koja` source under `lib/global/src/`, grep for the language
constructs it uses (`match`, `for`, closure types, `<>` concat, ternary,
`@extern`, generic impls, …), then cross-reference against the
`"alpha (typecheck|IR|LLVM) does not yet ..."` diagnostics in
`crates/koja-alpha-*/src/`. Anything the stdlib reaches for that alpha
diagnoses (or silently mishandles) is a blocker.

The full list of "not yet" diagnostics lives in:

- `koja-alpha-typecheck/src/pipeline/collect.rs`
- `koja-alpha-typecheck/src/pipeline/lift_signatures/{functions,types,constants}.rs`
- `koja-alpha-typecheck/src/pipeline/resolve/{expr,ops,statements,strings}.rs`
  (note: `resolve/calls/` is now its own submodule — see
  `mod.rs`, `methods.rs`, `bounded.rs`)
- `koja-alpha-ir/src/lower/{expr,ops,body,structs,enums,package,calls,closures}.rs`
- `koja-alpha-ir-llvm/src/{emit/mod,emit/instruction,main_wrapper}.rs`
  (note: `emit/` is now broken out into per-instruction-family
  siblings — `emit/{closures,enums,structs,locals,calls,concat,constants}.rs`)

---

## Stdlib feature inventory

Cataloguing what the stdlib actually reaches for. Counts are
approximate — the point is which files use a given construct.

| Construct                               | Files / examples                                                                                                   |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `struct` / `enum` / `protocol`          | every file                                                                                                         |
| Inherent `impl Type`                    | every file                                                                                                         |
| Trait `impl Protocol for Type`          | `kernel` (Equality, Hash for primitives), `debug`, `string`, `set`, `map`, `list`                                  |
| Generic types (`<T>`, `<K,V>`, `<U>`)   | `kernel`, `list`, `map`, `set`, `process` (shipped end-to-end)                                                     |
| Generic impl target (`impl Pair<A,B>`)  | `kernel` (Pair, Option, Result), `list`, `map`, `set`, `process` (shipped end-to-end)                              |
| `Self` type in signatures               | `bitwise`, `debug`, `set`, `map`, `list`                                                                           |
| `move` parameter mode                   | `process` (8x), `list` (8x), `map` (5x), `cptr`, `cstring`, `set`, `string` (already shipped)                      |
| `priv` visibility                       | every FFI-wrapping file                                                                                            |
| `@intrinsic` / `@extern "C"` / `@doc`   | every file (we shipped `@extern`/`@link`/`AnnotationKind`)                                                         |
| Closure parameter types (`fn (T) -> U`) | `kernel` (Option/Result map/then), `list` (filter/find/all?/map/reduce), `process` (already shipped)               |
| Closure expressions (block & short)     | none in stdlib bodies — shipped for user code; named fns auto-wrap as closure values when needed (already shipped) |
| `match` expression                      | `kernel` (12x — Option/Result), `string` (8x), `process` (7x), `list`, `io`, `fd`                                  |
| OR pattern (`"a" \| "b" \| ...`)        | `string` (alpha?, digit?, escape_debug, trim\_\*, whitespace?, upcase, downcase)                                   |
| Tuple pattern (`Some(val)`, `Ok(v)`)    | every match-using file                                                                                             |
| Wildcard pattern (`_`)                  | every match-using file                                                                                             |
| **`for` loop**                          | `string` (codepoints, digit?, alpha?, …), `list` (filter/find/all?/any?/map/reduce)                                |
| **`while` loop**                        | `string` (10+), `list` (reverse)                                                                                   |
| `return` statement                      | `string`, `list`                                                                                                   |
| `break` (in `loop`)                     | none in stdlib                                                                                                     |
| `unless`                                | `list` (`all?`)                                                                                                    |
| `if`/`else` value-producing             | `system`, `fd`, `cptr`, `string` (already shipped)                                                                 |
| `cond`                                  | `string` (alpha?, contains?, ends_with?, starts_with?) (already shipped)                                           |
| Ternary `cond ? a : b`                  | `fd` (5+), `system` (already shipped)                                                                              |
| String concat `<>`                      | `string` (heavy — upcase/downcase/escape_debug), `debug` (`format` for `String`) (already shipped)                 |
| Compound assign (`+=`, `-=`)            | `string` (15+), `list` (`reverse`) (already shipped)                                                               |
| String literal in IR                    | `kernel` (`Kernel.panic("…")`), `string`, `io`, `fd`, `system` — pervasive (already shipped)                       |
| String interpolation `#{…}`             | none — stdlib avoids it intentionally                                                                              |
| **`const` declarations**                | `io` (STDIN, STDOUT, STDERR — struct-literal initializers)                                                         |
| Numeric coercion (`Int` → `Int32` etc.) | `io` (`Fd{descriptor: 2}`), `fd` (CPtr ops with mixed widths), `time`                                              |
| `spawn`                                 | `process` (`Task.async`)                                                                                           |
| `receive` / `receive ... after`         | `process` (`run` loops)                                                                                            |
| List / Map / Set literal syntax         | none in stdlib bodies (collections are constructed via `.new()` + `.append`/`.put`)                                |
| Bitstring / `<<…>>` literals            | none in stdlib (alpha ships them anyway — gates Phase 7 of the match plan) (already shipped)                       |
| Nested `MyApp.Config` types             | none in stdlib                                                                                                     |
| Type unions (`A \| B`)                  | none in stdlib                                                                                                     |
| `type` aliases inside `impl`            | none in stdlib                                                                                                     |
| Free functions (no `impl`)              | none — every fn lives in an `impl`                                                                                 |
| Default field values                    | none in stdlib                                                                                                     |

The "none in stdlib" rows are useful negative space: those features are
**not** alpha blockers for the stdlib effort, no matter how much they
matter elsewhere.

---

## Gap classification

Every gap below maps to one or more diagnostics already wired into the
alpha crates.

### Blockers — without these the stdlib does not type-check

_None outstanding._

### Significant — required for non-trivial stdlib pieces

- ~~**`spawn` and `receive` in IR.**~~ **Shipped through LLVM emit.**
  See the new "Concurrency primitives" entry under "Already supported"
  for the full feature description. The remaining stdlib gap is _not_
  the primitives themselves but `Task<R>.run` in `process.koja`
  calling `self.work()` against a function-typed struct field —
  alpha-typecheck only dispatches `recv.method(...)` through method
  lookup today. Adding field-as-callable fallback unblocks
  `Global.process` for `AUTOIMPORT`; until then the primitives
  ship to user code via inline `Process` / `Ref` / `ReplyTo` decls
  and the `koja_rt_*` runtime ABI.

- **End-to-end concurrency execution.** Spawning works at the LLVM
  level (`koja_rt_spawn` returns a real PID, the wrapper is
  ABI-correct), but the alpha `main_wrapper.rs` does not call
  `koja_rt_main_done()` after the body runs, so spawned processes
  never actually execute. v1 codegen always emits the `main_done`
  call (`koja-codegen/src/compiler.rs:1257`); alpha needs the same
  one-line addition before the trailing `ret i64 0`. Pair this
  with the field-as-callable fix above to land the
  `task_async` / `counter_call` / `receive_lifecycle` end-to-end
  driver tests originally scoped to this slice.

### Already supported — common false positives

- **Closures — full surface, end-to-end.** Closure parameter types
  (`fn (T) -> U`) lift as `ResolvedType::Anonymous(AnonymousKind::Function)`
  with `FnParam` carrying name + `ResolvedType` per slot; closure
  expressions (both block `(x: T) -> { … }` and short `x -> body`
  forms) lower to a synthesized `IRFunctionKind::Closure` body
  named `<enclosing>__closure<N>`. Capture analysis walks the body
  AST to deduplicate free locals (encounter order); heap-typed
  captures `MoveOutLocal` into the closure's env at the
  construction site, stack-typed captures copy. The IR vocabulary
  is `IRType::Function`, `IRInstruction::{MakeClosure, CallClosure,
LoadCapture}`, and `FunctionKind::Closure { env_layout }` for
  synthesized bodies. Named top-level functions used in a
  closure-typed context auto-wrap as `<fn>__as_closure` adapters
  (captureless, env*ptr is null). Closure-typed local variables
  used as callees dispatch through `CallClosure` rather than the
  symbol-keyed `Call` form. LLVM ABI: closure values are
  `{fn_ptr, env_ptr}` fat pointers; closure bodies declare an
  extra `i8*`env parameter at LLVM position 0; env blocks
  heap-allocate via`malloc`and free at scope exit through
  `DropLocal { ty: Function }`. Backend symmetry: eval and LLVM
  produce identical results for higher-order calls, captureless
  closures, single/double/heap captures, and fn-as-value adapters.
  Pinned by `crates/koja-alpha-typecheck/tests/resolve\*closures.rs`,
  `crates/koja-alpha-ir/tests/lower_closures.rs`,
  `crates/koja-alpha-ir-eval/tests/closures.rs`,
  `crates/koja-alpha-ir-llvm/tests/closures.rs`, and the
  `\*\_closure\*\*`/`**higher_order**`/`\__fn_as_value_\*`driver
  tests in`crates/koja-driver/tests/alpha_two_plus_two.rs`.
  **Out**: recursive drops of nested heap captures inside the
  env block (the env itself frees, but heap-typed captures inside
  it leak today — alpha milestone trade-off for a simpler ABI; a
  per-body drop function synthesized alongside the closure body
  closes this).

- **Generics — full feature, end-to-end.** Generic types
  (`<T>`, `<K,V>`), generic functions, generic impls (concrete
  `impl Bag<Int>` and trait `impl Show for List<T>`), bounds
  (`<T: Eq>`, `<T: Eq & Hash>`), protocol conformance recording on
  the target's `StructDefinition` / `EnumDefinition`, bounded-method
  dispatch (`t.method()` against the type-param's bounds), trait-impl
  domain check (`Bag<Int>::greet` vs `Bag<String>::greet`), dual-scope
  inference (receiver scope + method scope) at every call site.
  Substitution drives end-to-end through monomorphization
  (`koja-alpha-ir/src/generics/`) and name-mangling
  (`Bag_$Int64$.greet`). Pinned by
  `concrete_impl_specialization.rs`, `bounded_dispatch.rs`,
  `generic_method_inference.rs`, `trait_impl_domain.rs`, and
  `substitution.rs` across `koja-alpha-typecheck`, `koja-alpha-ir`,
  and `koja-alpha-ir-llvm`. **Out**: generic protocol methods
  (i.e. `protocol P { fn m<U>(...) }`) — separate slice; no stdlib
  use.

- **`match` expression — full v1-parity surface.** Patterns:
  literal, wildcard, binding, enum-unit, enum-tuple, enum-struct
  (named-field destructure), struct destructure, or, and constructor
  shorthand (`Some(x)`). Guards (`pattern when expr -> body`).
  Exhaustiveness checking on enum subjects with `Bool`
  specialization. Reachability / redundancy diagnostics (dead
  arms, duplicates, overlaps) as warnings. Pinned by
  `crates/koja-alpha-typecheck/tests/resolve_match.rs`,
  `crates/koja-alpha-ir/tests/lower_match.rs`,
  `crates/koja-alpha-ir-eval/tests/interpreter.rs`,
  `crates/koja-alpha-ir-llvm/tests/control_flow.rs`. **Out**:
  `Pattern::Binary` (gated on binary literals; tracked as Phase 7
  of [20260511-ALPHA-MATCH-PLAN.md](20260511-ALPHA-MATCH-PLAN.md)).

- **`String` / `Binary` / `Bits` — full surface, end-to-end.**
  String literals lower to `[i64 bit_length][payload bytes][\0]`
  heap globals; `Binary` and `Bits` share the same `[i64
bit_length][payload]` layout (no NUL, ceil-rounded bytes for
  `Bits`). The polymorphic `<>` concat operator typechecks against
  any of the three (same-type operands, same-type result), with
  `String`/`Binary` lowering to inline `malloc` + `memcpy` and
  `Bits` deferring to `__koja_alpha_concat_bits` for sub-byte
  alignment. `<<segments>>` literal syntax handles integer
  (signed/unsigned, big/little, literal `::N` widths including
  sub-byte), float (`: Float32` / `: Float64`), string segments,
  and the type-annotation form (`: Int16` ≡ `::16 signed big`);
  byte-aligned segments pack inline, sub-byte segments call
  `__koja_alpha_pack_bits`. Total bits `% 8 == 0` → `Binary`,
  else `Bits`. Move/drop integration: heap-typed slots are
  `Owned` at write sites and emit `DropLocal` at scope exit (one
  shape across all three types — `free(payload - 8)`). Auto-print
  routes `String` → `__koja_alpha_print_string`, `Binary` →
  `__koja_alpha_print_binary`, `Bits` → `__koja_alpha_print_bits`,
  with byte-identical output between LLVM and the eval interpreter.
  Pinned by `crates/koja-alpha-typecheck/tests/resolve_binary_literal.rs`,
  `crates/koja-alpha-ir/tests/lower_binary_literal.rs`,
  `crates/koja-alpha-ir-eval/tests/binary_literal.rs`,
  `crates/koja-alpha-ir-llvm/tests/program.rs`, and the e2e
  driver tests in `crates/koja-driver/tests/alpha_two_plus_two.rs`
  (`*_string_concat_*`, `*_binary_literal_*`, `*_bits_literal_*`).
  Alpha **leapfrogs v1 here** for `Bits` — v1's `resolve_binary_segments`
  rejects sub-byte segments and forces `Primitive::Binary`; alpha
  packs them via runtime helpers cleanly. **Out**: stdlib
  conversion methods (`String.to_binary`, `Binary.to_string`,
  `Bits.to_binary`, `String.to_cstring`, `CPtr.to_binary`),
  `Equality` / `Hash` on `Binary` / `Bits`, `Pattern::Binary` in
  `match` (Phase 7 of [20260511-ALPHA-MATCH-PLAN.md](20260511-ALPHA-MATCH-PLAN.md),
  unblocked by this slice), `<<x::n>>` with runtime-int widths,
  and `Debug.format` rendering (gated on `Debug` protocol synthesis,
  not on the alpha critical path).

- **Const with struct literals.** `const STDOUT: Fd = Fd{descriptor: 1}`
  (used in `io.koja`) is wired end-to-end:
  `lift_signatures/constants.rs::struct_construction_type` validates
  non-generic struct literals;
  `lower/constants.rs::lower_constant_value` pools them as
  `IRConstantValue::Struct`. The `io.koja` STDIN / STDOUT / STDERR
  constants are blocked _only_ by the numeric-coercion blocker (the
  `1` literal is `Int`, `Fd.descriptor: Int32`, strict-equality
  rejects). The diagnostic at `lift_signatures/constants.rs:261` is
  for **generic** struct literals in `const`, which the stdlib doesn't
  use.

- **Compound assignment (`+=`, `-=`).** Wired through both typecheck
  (`resolve/statements.rs::Statement::CompoundAssign`) and IR
  (`lower/body.rs:126`). `string.koja` exercises `i += 1` / `i -= 1`
  in 9 places, all already supported. (`<>=` isn't a token in the
  language — every string-concat in stdlib is the long form
  `result = result <> rhs`.)

### Minor — can be deferred

- **List/Map literal syntax** — stdlib never writes `[1, 2, 3]` or
  `["k": v]` in its own bodies. Required once user code does, but not
  a stdlib blocker.

- **String interpolation** — stdlib avoids it. User code lights up
  `resolve/strings.rs:27` ("does not yet support string interpolation")
  the moment someone writes `"hello #{name}"`.

- **Default parameter values** — `lift_signatures/functions.rs:339`.
  Stdlib doesn't use them.

- **Annotations on enum/struct items**
  (`lower/{enums,structs}.rs:248,306`) — stdlib only uses `@doc` on
  items, which is already accepted. The diagnostic fires on any other
  annotation; not a stdlib blocker.

- **Type unions, `type` aliases inside `impl`, named arguments,
  generic protocol methods, dotted type names, default field
  values** — none of these appear in `lib/global/src/`. Tracked in
  v1 [GAPS.md](GAPS.md) where applicable.

---

## Recommended sequencing

Order is chosen to maximize what each step _unblocks_, not by
implementation cost in isolation. Each step lands behind seal-asserted
output and standalone tests, per northstar.

### ~~Phase 1 — Loops~~ — **Shipped**

- `while` lowers to a three-block CFG (`while_header` / `while_body`
  / `while_exit`) using the existing alloca-based local-variable
  model for loop-carried state.
- `for` desugars in the typecheck `synthesize` sub-pass (not in IR
  lowering) to `while` + `match` over `Option<T>`, per the northstar
  "typecheck owns AST mutation" rule. The desugar emits real method
  calls to the iterable's `length` / `get`; the structural
  `Enumeration<T>` contract is checked nominally against
  `Global.Option<T>`.
- `return` from inside loops works through the existing `Never`
  divergence path.
- `break` / `continue` remain post-stdlib (no `lib/global/` use).

### ~~Phase 2 — Closures~~ — **Shipped**

- Closure parameter types lift as
  `ResolvedType::Anonymous(AnonymousKind::Function)` with
  `FnParam { name, ty }`. The `Anonymous` umbrella leaves room for
  future structural variants (tuples, dictionaries) without
  reshaping `ResolvedType` again.
- Closure expressions (block + short form) are stamped with a
  `LocalId` per parameter at typecheck-resolve time, capture
  analysis runs at lower time (free-local walk that deduplicates by
  encounter order), and heap-typed captures `MoveOutLocal` into a
  per-instance env block.
- The IR-level move story is "closures never borrow" — every heap
  capture is `Owned` in the env, the closure value itself is
  `Owned` (heap env), and the existing `DropLocal` machinery frees
  the env at scope exit. (Recursive drop of heap captures inside
  the env is a known follow-up; see "Closures — full surface" above.)
- Both backends ship: the eval interpreter carries a
  `Value::Closure { body, captures }` runtime shape; LLVM uses a
  `{fn_ptr, env_ptr}` fat pointer with the env_ptr threaded as
  param 0 of every closure body.
- Fn-as-value adapters (named function used in a closure-typed
  context) and closure-typed local calls landed in the same slice,
  so the surface is "first-class functions" rather than just
  closure expressions.

### Phase 3 — Mechanical glue

Small; can land in any order or batched into one PR each.

- ~~**Numeric coercion at struct-literal sites (narrow-int widths).**~~
  Shipped. The literal-fit slice landed: a span-keyed
  `Coercions` table populated at the six type-equality sites + const
  initializers tells IR lower to mint `ConstValue::Int*` /
  `ConstValue::Float*` at the recorded width (with negated-literal
  fold for `-N` shapes). Out-of-range literals surface a precise
  narrow-int diagnostic; non-literal narrowing (variable → narrow
  slot) still falls through to a strict type-mismatch and waits on
  a runtime-conversion slice. The candidate user-facing surface for
  that slice is a `.to_int8()` / `.to_uint16()` / etc. method
  family per numeric type, mirroring stdlib's existing
  `.to_string()`-style API. Unblocks the stdlib's
  `const Fd{descriptor: …}` constants and lets driver/eval tests
  construct narrow-width values from int literals (the
  previously-removed narrow-width arms in
  `koja-alpha-ir-eval/tests/bitwise.rs` and
  `koja-alpha-ir-llvm/tests/bitwise.rs` are restored, plus
  `crates/koja-alpha-typecheck/tests/literal_coercion.rs` pins the
  fit/overflow/sign-mismatch matrix end-to-end).
- ~~**`move` parameters in typecheck signatures.**~~ **Shipped.**
  `lift_signatures` propagates the surface `PassMode` verbatim
  onto `ResolvedParam::mode`; the IR's `ownership_for_param`
  stamps `Owned` on `Move` heap-typed slots, and parameter
  promotion threads the ownership through to the function-exit
  `DropLocal` emission. Pinned by
  `crates/koja-alpha-typecheck/tests/pass_mode.rs` and the
  alpha move/drop foundation slice's IR / eval / LLVM tests.
- ~~**String literals in IR.**~~ **Shipped.** End-to-end through
  `lower/expr.rs` (string literal arm), `emit_const_payload`
  (shared `[i64 bit_length][payload bytes][\0]` heap layout), and
  `__koja_alpha_print_string` for auto-print. Landed alongside
  the strings/binary/bits slice (see "Already supported").
- ~~**`<>` concat operator.**~~ **Shipped.** `IRInstruction::Concat`
  with per-`ConcatKind` emission (inline `malloc`/`memcpy` for
  `String`/`Binary`, `__koja_alpha_concat_bits` runtime helper for
  `Bits`). Same slice as String literals.

### Phase 4 — Concurrency (optional)

- Required only for `process.koja` to type-check & lower. `spawn` and
  `receive` already have AST nodes and runtime support in v1.
- Eval can stub: panic on `spawn`, never-receive on `receive`. The
  goal is for `koja alpha check` to pass on `process.koja`, not for
  the alpha eval to actually run processes.
- LLVM emits `koja_rt_spawn` / `koja_rt_receive*` calls (same symbols
  v1 codegen uses).

After Phase 4, `koja alpha check expo/lib/global/src/*.koja` should be
fully green. Phases 1–3 alone get every non-`process.koja` file green,
which is most of the stdlib by line count.

---

## Out of scope for alpha

These features appear in v1 but are explicitly _not_ on the alpha
roadmap, even after stdlib parity:

- **String interpolation.** Stdlib avoids it; users tolerate
  qualifying with `<>` until alpha closes.
- **Free functions.** Every stdlib fn lives in an `impl`; alpha
  enforces the same shape.
- **Type unions** (`A | B`). The block-param SSA join we shipped uses
  `Never` as the lattice bottom and rejects mismatched arm types.
  Unions are an additive future feature; stdlib doesn't use them.
- **Nested types** (`MyApp.Config`). Tracked in
  [GAPS.md](GAPS.md); 1–2 weeks; not a stdlib blocker.
- **Default field values, default parameter values, named arguments,
  type aliases inside impls.** Pleasant-to-have; stdlib doesn't use
  them.
- **`break` / `continue`.** No use in `lib/global/src/`; defer until
  the user-code surface needs them.

If a future stdlib package (Net, HTTP, JSON) adds a dependency on any
of these, that's the trigger to revisit — not before.

---

## Status snapshot (post-generics, post-`match`, post-move/drop, post-strings/binary/bits, post-loops, post-closures, post-concurrency-primitives, post-alias-imports, post-Clone-heap-primitives)

What's shipped since the last audit:

- **Clone protocol — heap primitives.** New `Global.clone` autoimport
  (sits immediately after `Global.kernel` so future Clone impls can
  reach for `Result` / `Option`). Shape mirrors `Debug`: a single
  `protocol Clone { fn clone(self) -> Self }` decl plus three
  `@intrinsic impl Clone for {String,Binary,Bits}` blocks against
  the shared `[i64 bit_length][payload]` heap layout. Backend
  implementations live in `koja-alpha-ir-llvm/src/intrinsics/heap_clone.rs`
  (one shared `emit_payload_clone` keyed on `(with_nul,
ceil_byte_count)` — `String` writes a trailing `\0`, `Bits`
  rounds the byte count up via `(bits + 7) >> 3` to keep the
  trailing partial byte) and `koja-alpha-ir-eval/src/intrinsics/{string,binary}.rs`
  (`Vec<u8>::clone` for the runtime values). New
  `IRIntrinsicId::{String,Binary,Bits}(_::Clone)` variants in
  `koja-alpha-ir/src/intrinsic_id.rs` keep the typed dispatch
  exhaustive on both backends. Pinned by
  `crates/koja-alpha-typecheck/tests/clone.rs`,
  `crates/koja-alpha-ir/tests/lower_clone.rs`,
  `crates/koja-alpha-ir-eval/tests/clone.rs`,
  `crates/koja-alpha-ir-llvm/tests/clone.rs`, and the
  `alpha_run_{llvm,interpreter}_script_string_clone_independent_buffers`
  driver tests (clone + concat + reuse the source — observable
  proof of independent buffers). **Out**: the universal-Clone
  synthesizer (`derive_clone.rs`), value-primitive impls
  (`Int.clone()` and friends), `CPtr.clone()`, and hand-written
  generic-container impls (`List<T>` / `Map<K,V>` / `Set<T>` /
  `Option<T>` / `Result<T,E>` / `Pair<A,B>`) all queue for
  **PR2 — universal Clone slice**. Also queued for PR2: extending
  `lower::ownership::ownership_for_expr` so `s.clone()`'s
  destination slot stamps `Owned` end-to-end (today the slot is
  `Unowned`, the same gap every fresh-allocating helper —
  `String.upcase`, `String.replace`, … — already has; not a Clone
  regression).

- **Alias imports — `alias Pkg.Type [as Local]`, end-to-end with
  shadow-as-error.** `koja-alpha-typecheck/src/pipeline/aliases.rs`
  validates each file's alias decls against the post-collect
  registry (path length ≥ 2, target exists as struct/enum/protocol,
  no duplicate local names, no shadowing of current-package or
  `Global` bindings — with a redundant-self-alias carve-out that
  allows `alias TestApp.Foo as Foo` when the alias and existing
  binding resolve to the same identifier). The `rewrite_through_aliases`
  helper is **path-length agnostic** so a future `alias Pkg.Outer as O`
  resolves both `O` and `O.Inner` once nested-type lifting lands —
  no movement here. Lookup precedence is type-param scope → file
  aliases → current package → `Global`. Threaded through every
  `lift_signatures/types.rs::resolve_type_expr` caller (functions,
  structs, enums, protocols, impls, constants) plus the `resolve`
  pass via `ResolverEnv` / `Resolver` (which now also carries
  `current_file: &Path` as load-bearing infrastructure for the
  upcoming `priv` slice). Pinned by
  `crates/koja-alpha-typecheck/tests/aliases.rs` (13 tests
  covering alias-to-Global, alias-as, default-local-name, unknown-
  package / unknown-type, path-too-short, multi-segment-target
  fall-through, duplicate-name, shadow-Global / shadow-current-
  package, redundant-self-alias allowed, file-private isolation,
  type-param-vs-alias precedence) and the
  `alpha_run_llvm_script_aliased_crypto_sha256_digest_prints_thirtytwo`
  driver test that aliases `Crypto.SHA256` and runs through the
  LLVM backend.

- **`QUALIFIED` — curated qualified stdlib subset.** Parallel
  to `AUTOIMPORT`, lives in `koja-stdlib/build.rs` and is
  re-exported via `qualified_sources()`. Seeded with the
  `Crypto` package; `koja-driver/src/alpha.rs::bundle_with_autoimport`
  and the alpha-typecheck test helper both prepend the qualified
  set so any alpha pipeline can `alias Crypto.Type` without a
  per-test stub. **Pragmatic stand-in for incremental package
  loading**: retires once `IRPackage` caching + on-demand package
  loads land, at which point qualified packages load lazily from
  disk via the package graph and the curated list disappears.
  Adding `Http` / `Json` / `Net` is a single-line edit to
  `alpha_qualified_packages` once those packages typecheck cleanly
  through alpha (the inventory of remaining gaps is the same one
  this roadmap tracks).

- **Concurrency primitives — `spawn` / `receive` + `Ref<M, R>` /
  `ReplyTo<R>` intrinsics, end-to-end through LLVM emit.**
  `ExprKind::Spawn` and `ExprKind::Receive` are wired through
  `koja-alpha-typecheck` (`pipeline/resolve/process.rs` +
  `pipeline/seal/expressions.rs`, with the typed-binding tag
  discrimination on receive arms), through `koja-alpha-ir`
  (`IRInstruction::{Spawn, Receive}`,
  `FunctionKind::SpawnWrapper`, the `IRIntrinsicId::{Ref,
ReplyTo}` typed dispatch family, and the `lower/process.rs`
  lowering that synthesizes a wrapper body per spawned state
  type), and through `koja-alpha-ir-llvm` (`emit/process.rs`
  emits `koja_rt_spawn` / `koja_rt_receive` /
  `koja_rt_receive_timeout` calls and synthesizes the C-ABI
  `void(*)(i8*)` wrapper body that loads the config, calls
  `<state>.start`, branches on the `Result` enum's tag, and
  chains into `<state>.run` on `Ok`; `intrinsics/process.rs`
  handles `Ref.{self_ref, cast, signal, kill, alive?,
send_after}` + `ReplyTo.send` against the corresponding
  `koja_rt_*` externs declared in `runtime.rs`). Eval stubs
  every concurrency intrinsic as `RuntimeError::Unsupported` —
  alpha-eval is single-threaded and in-process; the LLVM
  backend is the source of truth for runtime semantics. Pinned
  by `crates/koja-alpha-typecheck/tests/process.rs`,
  `crates/koja-alpha-ir/tests/lower_process.rs`, and
  `crates/koja-alpha-ir-llvm/tests/process.rs` (10 tests
  covering spawn-wrapper synthesis, receive arm dispatch,
  timeout-driven `after` branches, and every implemented
  `Ref` / `ReplyTo` method). **Out**: `Ref.call` (synchronous
  request/reply — needs the runtime's matching reply-channel
  primitive surfaced; stubbed with a `Codegen` error today),
  `koja_rt_main_done` invocation in alpha's `main_wrapper`
  (without it, spawned processes never run after `main`
  returns; one-line addition to mirror v1 codegen), the
  `Global.process` entry in `AUTOIMPORT` (gated on
  `Task<R>.run` calling `self.work()` — see the field-as-callable
  gap above), and the `task_async` / `counter_call` /
  `receive_lifecycle` end-to-end driver tests originally
  scoped to this slice (gated on the `main_done` wiring and
  the autoimport flip).

- **Generics — full feature, end-to-end (~6 kLOC).** Generic types
  (`<T>`, `<K, V>`), generic functions, generic impls (concrete
  `impl Bag<Int>` and trait `impl Show for List<T>`), bounds
  (`<T: Eq>`, `<T: Eq & Hash>`), protocol conformance recording,
  trait-impl domain check, bounded-method dispatch (`t.method()`
  against type-param bounds), and dual-scope inference (receiver
  scope + method scope). Substitution drives end-to-end through
  monomorphization (`koja-alpha-ir/src/generics/`) and
  name-mangling (`Bag_$Int64$.greet`). Single representation:
  `Resolution::TypeParam { owner, index }` plus
  `substitute_resolved_type(ty, subst, owner)`.

- **`match` expression — full surface beyond the original
  stdlib subset.** Patterns: literal, wildcard, binding, enum-unit,
  enum-tuple, enum-struct (named-field destructure), struct
  destructure, or, and constructor shorthand (`Some(x)` via in-place
  AST rewrite at resolve). Guards (`pattern when expr -> body`)
  with `Bool` enforcement and "guards don't contribute to coverage"
  semantics. Exhaustiveness checking on enum subjects with `Bool`
  specialization. Reachability / redundancy diagnostics (dead arms,
  duplicates, overlaps) as warnings. Subset originally scoped for
  the stdlib delivered alongside the v1-parity surface; only
  `Pattern::Binary` is deferred (gated on binary literals).

- **Block-parameter SSA join** (the substrate `match` rode in on).
  `IRBasicBlock.params: Vec<BlockParam>` plus
  `IRTerminator::{Branch, CondBranch}` carrying
  `BranchTarget { block, args }`; LLVM emission goes block-params →
  phi nodes; CFG reachability emits `unreachable` for dead merge
  blocks. `ResolvedType::Never` is a first-class lattice bottom
  (maps to `IRType::Unit` in IR; LLVM elides Unit-typed phis).

- **Move / drop foundation — `Ownership` lattice + scope-exit
  drops.** Closes `move` parameters in typecheck signatures and
  the IR-level move semantics behind it. `lift_signatures`
  propagates the surface `PassMode` (`Move` / `Borrow` / `Copy`)
  verbatim onto `ResolvedParam::mode`; the IR's `Ownership`
  lattice (`Owned` / `Unowned`) flows from the param's
  `PassMode` and from RHS expressions on `LocalWrite`,
  reassignments emit a `DropLocal` for the prior `Owned` slot,
  return statements transfer ownership via `MoveOutLocal`, and
  every owned slot still live at function exit gets a final
  `DropLocal` emission. The LLVM backend lowers `DropLocal` to
  `free(payload - 8)` (one shape for all heap types); the eval
  interpreter mirrors the same drops. Heap-type taxonomy
  (`is_heap_type`) covers `String` / `Binary` / `Bits`, ready to
  extend as new heap types ship. Pinned by
  `crates/koja-alpha-typecheck/tests/pass_mode.rs`,
  the move/drop unit tests across `koja-alpha-ir/tests/` and
  `koja-alpha-ir-llvm/tests/`, and the eval `value_drops`
  coverage. **Out**: borrow-checker proper (uniqueness +
  use-after-move diagnostics) — separate slice; the lattice is
  the substrate.

- **Strings / Binary / Bits — full surface, end-to-end.** Closes
  the original "String literals in IR" + "`<>` concat" Phase 3
  blockers in a single coherent slice. `String`, `Binary`, and
  `Bits` share a v1-faithful `[i64 bit_length][payload]` heap
  layout under an opaque `i8*` SSA pointer; the `<>` operator
  typechecks polymorphically across the three; `<<segments>>`
  literals support per-segment endian / signedness / sub-byte
  widths (with `__koja_alpha_pack_bits` runtime helper for
  bit-level packing). Move/drop integration is shared across all
  three heap types — the `Ownership` lattice flags heap-typed
  writes as `Owned` and emits `DropLocal { ty }` at scope exit.
  Auto-print routing covers all three via dedicated runtime
  printers, byte-identical between LLVM and the eval interpreter.
  Alpha leapfrogs v1 here for `Bits` — v1's known coherence bug
  (sub-byte segments rejected, forced to `Primitive::Binary`)
  doesn't repeat. Pinned by `resolve_binary_literal.rs`,
  `lower_binary_literal.rs`, `binary_literal.rs` (eval),
  `program.rs` (LLVM), and the `*_concat_*` / `*_literal_*` e2e
  driver tests.

- **Loops — `while` + `for` end-to-end.** `while` lowers to a
  three-block CFG (header / body / exit) with loop-carried state in
  alloca slots (no block-param plumbing — the existing `LocalRead` /
  `LocalWrite` path handles re-reads across the back-edge). `for`
  desugars in the typecheck `synthesize` sub-pass to `while` +
  `match` over `Option<T>` (canonical `Global.Option<T>` stub is
  registered as a fully-lifted enum); `Enumeration<T>` is the
  structural contract — the iterable must expose `length()` and
  `get(idx) -> Option<T>`. Per the northstar "typecheck owns AST
  mutation" rule, the desugar runs before `resolve` so IR / eval /
  LLVM never see `ExprKind::For`. `return` from inside loops, nested
  loops, `if` / `else` inside loop bodies, and heap-typed loop-
  carried state (string accumulator, drop-on-reassignment) all fall
  out of the existing machinery. Pinned by `resolve_loops.rs`,
  `lower_loops.rs`, `eval/loops.rs`, `llvm/loops.rs`, and the
  `*_while_*` / `*_for_*` driver tests.

- **Closures — first-class functions, end-to-end.** Closes
  Phase 2. Closure parameter types lift as
  `ResolvedType::Anonymous(AnonymousKind::Function)` (the
  `Anonymous` umbrella leaves room for future structural variants
  — tuples, dictionaries — without reshaping `ResolvedType`
  again). Closure expressions (block + short form) lower to
  synthesized `IRFunctionKind::Closure` bodies named
  `<enclosing>__closure<N>`; capture analysis walks the body AST,
  deduplicates free locals by encounter order, and routes heap
  captures through `MoveOutLocal` so the env owns its captures.
  IR vocabulary: `IRType::Function`, `IRInstruction::{MakeClosure,
CallClosure, LoadCapture}`, `FunctionKind::Closure { env_layout }`.
  Named functions used in a closure-typed context auto-wrap as
  `<fn>__as_closure` adapters (captureless, null env*ptr); local
  variables of closure type dispatch via `CallClosure` rather than
  the symbol-keyed `Call`. Eval carries a
  `Value::Closure { body, captures }` runtime shape; LLVM uses a
  `{fn_ptr, env_ptr}` fat pointer with the env_ptr threaded as
  LLVM param 0 of every closure body. Backend symmetry verified
  end-to-end. The seal pass enforces structural invariants for
  `MakeClosure` / `CallClosure` / `LoadCapture`
  (`seal/closures.rs`). Move story is "closures never borrow" —
  heap captures move; closure values themselves are `Owned`.
  Pinned by `resolve_closures.rs`, `lower_closures.rs`,
  `eval/closures.rs`, `llvm/closures.rs`, and the `\*\_closure*_`/`**higher_order**`/`\_\_fn_as_value_\*` driver tests. **Out**:
  recursive drops of nested heap captures inside the env (env
  frees at scope exit, but heap-typed captures inside it leak
  today — alpha trade-off for a simpler ABI).

- **Stdlib auto-import + first stdlib files (`time` / `bitwise`).**
  `koja-stdlib` exposes `AUTOIMPORT` (currently
  `Global.time` + `Global.bitwise`) plus an
  `autoimport_sources()` helper that converts the curated
  list into `Vec<SourceFile>`. The driver's three single-file
  parse paths (`read_and_check`, `run_script_pipeline`,
  `run_check`) prepend the curated set before parsing, and every
  alpha-side test crate's `tests/common/mod.rs` does the same so
  the test surface and the user-driven pipeline see the same
  prelude. Backing the auto-import: `IRFunction.kind` grew an
  `Intrinsic { id }` payload (the dispatch key — `Type.method`
  string, e.g. `Int.band`, derived from
  `identifier.path().join(".")`); both backends key their
  intrinsic dispatch tables on the variant's `id` rather than the
  mangled symbol so monomorphized cells share one emitter without
  per-mangling rows. Forty-eight `Bitwise` cells (8 widths × 6 ops)
  ship in both backends — LLVM uses inkwell's `build_and` /
  `build_or` / `build_xor` / `build_not` / `build_left_shift` /
  `build_right_shift` (with `sign_extend` driven by the
  receiver-type prefix) and truncates the `Int64`-typed shift
  count to the operand width on narrow receivers; eval flattens
  every width to `Value::Int(i64)` and branches `bsr` signedness
  on the same prefix parse. `time.koja` ships pure-Koja (Duration
  arithmetic + DateTime field projections) with the
  `@extern "C" priv fn koja_time_now_millis` declaration inside
  `DateTime` linking against `koja-runtime`'s C symbol — the
  Phase 3 mechanical glue surface for stdlib-side externs lands
  here, validated by an e2e driver test that actually links and
  runs `DateTime.now().timestamp_millis()`. Two type-checker
  refinements rode in alongside: (1) `Int ≡ Int64` and
  `Float ≡ Float64` are equivalent at the six type-equality sites
  (struct fields / enum variants / two call-arg paths /
  bounded-method args / return types) via a single
  `types_equivalent` helper — narrows the surviving "numeric
  coercion at struct-literal sites" blocker to _narrower-than-Int_
  widths only; (2) bare-call resolution prioritizes the enclosing
  struct/enum scope before falling back to package scope, so
  sibling `priv fn`s call by their bare name without
  qualification (the standard stdlib idiom in `system.koja` /
  `fd.koja` / etc.); the escape hatch for callers who really want
  the package-level function is full qualification
  (`Global.foo()`). Generalizes to nested types when those land —
  each level wins over the next outward one. Primitive stubs in
  `with_stdlib_stubs` were promoted from `Struct(None)` to
  `Struct(Some(empty_def))` so `record_conformance` accepts them
  (`bitwise.koja` impls `Bitwise for Int` etc. at preload time).
  Hex/binary/underscored int literals (`0xFF`, `0b1010`,
  `1_000_000`) lower correctly through alpha-IR's
  `parse_int_literal` (the lexer already handled them; the IR's
  `text.parse::<i64>()` was decimal-only). Pinned by
  `crates/koja-alpha-typecheck/tests/alpha_autoimport.rs`,
  `crates/koja-alpha-ir/tests/lower_ops.rs` (radix coverage),
  `crates/koja-alpha-ir-eval/tests/{bitwise,time}.rs`,
  `crates/koja-alpha-ir-llvm/tests/{bitwise,time}.rs`, and the
  `*_bitwise_*` / `*_duration_*` / `*_datetime_now_*` driver
  tests. **Out**: the remaining 7 stdlib files
  (`cptr` / `fd` / `io` / `kernel` / `list` / `map` / `set` /
  `string` / `system`) — each rides on additional intrinsic
  families that need wiring through the same dispatch table.

- **Stdlib slice 2: `kernel` / `cptr` / `cstring` autoimport.**
  Three more `Global.*` source files now flow through the
  autoimport pipeline end-to-end. The compiler-side blockers
  closed in lockstep:
  - **Method-level type parameters** (`fn map<U>(self, f: fn(T) -> U) -> Option<U>`,
    `fn alloc<T>(count: Int) -> CPtr<T>`). `Instantiation` carries
    a separate `method_args` channel and the lower-call site
    threads both struct-level and method-level type args through
    the new structured `mangled_method_name` helper — `IRSymbol`
    stays opaque end-to-end, no string parsing at call sites. The
    monomorphizer enqueues triples `(struct_id, struct_args,
method_args)` so each cell mints exactly once. Pinned by
    `crates/koja-alpha-ir/tests/method_generics.rs`.
  - **Preload stubs for `Option<T>` / `CPtr<T>`** dropped — the
    autoimport's source definitions are now the canonical surface
    for both, so `Result<T, E>` / `Pair<A, B>` / `Range` and the
    `Equality` / `Hash` protocol impls register the same way as
    any user-defined type. The escape valve for `with_stdlib_stubs`
    contracted to the primitive numeric / bool / never tags only.
  - **`Kernel.panic` typed as `Never`** via a `lift_signatures`
    override (the source still says `-> Unit` for v1 parity);
    callers in match-arm tail position propagate the surrounding
    arm's expected type instead of mismatching against `Unit`.
    Lowering caps any `Never`-typed `Statement::Expr` with
    `IRTerminator::Unreachable` so SSA / dominator analysis stays
    well-formed across the divergent edge.
  - **Bidirectional inference** for generic enum construction
    threads an `expected: Option<&ResolvedType>` through
    `resolve_match` / `resolve_if` / `resolve_cond` /
    `resolve_ternary` arm tails, the function-body trailing
    `Statement::Expr`, and `resolve_enum_construction`. Unit
    variants (`Option.None`) and partially-constrained tuple
    variants (`Result.Ok(5)` against `Result<Int, String>`)
    resolve their type parameters from the surrounding context,
    so `Option.map` / `Option.then` / `Result.map` / `Result.then`
    use their pure-Koja source bodies rather than the temporary
    `@intrinsic` stopgap. Pinned by
    `crates/koja-alpha-typecheck/tests/bidirectional_inference.rs`.
  - **Concrete-pinned `impl_args` on `FunctionSignature`** so
    bare static calls inside `impl CPtr<UInt8>` (`strlen` from
    `to_cstring`) mangle as `Global.CPtr_$UInt8$.strlen` — the
    typechecker captures the impl block's concrete type args at
    lift time and the lower-call site consults them when the
    receiver is a `Self` static method without explicit type
    args.
  - **Backend intrinsic + extern emitters** for the families
    `kernel.koja` / `cptr.koja` / `cstring.koja` introduce —
    `Equality.eq` × 9 widths, `Hash.hash` × 9 widths,
    `Kernel.panic`, `CPtr.{alloc,free,null,offset,read,write,
null?,to_binary,to_string}`, `CString.to_string`,
    `Binary.{byte_size,ptr,to_bits,to_string}`, `Bits.to_binary`,
    `Int.parse` / `Float.parse` — register in both backends.
    LLVM emitters mirror the v1 inline-IR shapes (SplitMix64 for
    `Hash`, `icmp eq` for `Equality`, `malloc` / `free` /
    `memcpy` for `CPtr`, `__koja_alpha_panic` for `Kernel.panic`).
    Eval implementations cover the cases that fit `Value`'s
    in-process shape (`Equality.eq`, `Hash.hash`, `Kernel.panic`,
    `Binary.byte_size` / `to_bits`); CPtr / Result-returning
    intrinsics route to a clean `RuntimeError::Unsupported` with
    a pointer to `--backend=llvm`. The eval extern table grew a
    shim for `koja_kernel_exit` (live FFI into `koja-runtime`)
    plus an explicit `Unsupported` row for `strlen`
    (CPtr-trafficking). LLVM-side SSA hygiene tightened:
    `IRTerminator::Return { value: Some }` against a void-returning
    function emits `ret void` (skips the SSA lookup since
    void-typed call results don't get registered), and
    `Statement::Expr` of `Never` type emits
    `IRTerminator::Unreachable`. `Random` was extracted from
    `kernel.koja` into its own `random.koja` and held back from
    `AUTOIMPORT`: its `bytes` body chains
    `String.to_binary`, which lives in `string.koja` (still far
    from alpha-ready — uses `for/in`, multi-pattern `match`, and
    `List<u8>`), so eagerly typechecking it as part of the
    autoimport surface would break the whole pipeline. v1 still
    picks up `random.koja` through the unfiltered `SOURCES`
    list. **Out**: typed-local bidirectional inference
    (`p: CPtr<UInt8> = CPtr.alloc(8)` still fails to infer `T`).
    Result / Option construction in eval intrinsics' return slots
    requires a registry handle the dispatch seam doesn't carry
    today, so `Int.parse` / `Float.parse` / `Binary.to_string` /
    `Bits.to_binary` are stubbed `Unsupported` on eval and
    `unreachable` on LLVM. Pinned by
    `crates/koja-alpha-typecheck/tests/{kernel,cptr,cstring}.rs`,
    `crates/koja-alpha-ir-eval/tests/kernel.rs`,
    `crates/koja-alpha-ir-llvm/tests/intrinsics.rs`, and the
    `*_result_ok_map_unwrap_*` driver tests.

- **Stdlib slice 3: `list` / `string` / `random` autoimport.**
  Three more `Global.*` source files flow through the autoimport
  pipeline end-to-end, finishing the collection / text surface
  the rest of the stdlib depends on. The compiler-side blockers
  closed in lockstep:
  - **List as a primitive `IRType`.** `IRType::List(Box<IRType>)`
    sits next to `IRType::CPtr(_)` so the lowering pass stops
    synthesizing struct decls for `List<T>` and centralizes the
    `{ buf_ptr: i8*, len: i64, cap: i64 }` value shape in
    `types::list_value_type`. Element types still flow through
    generic monomorphization (`List_$Int64$`, `List_$String$`,
    etc.); the difference is that the LLVM body is now anchored
    in the alpha backend rather than the IR's struct registry,
    matching how `CPtr<T>` was already wired.
  - **List literal `[a, b, c]` desugar in typecheck-resolve.**
    `ExprKind::List` is rewritten in-place to
    `List.new().append(a).append(b).append(c)` after element
    types are inferred, so a common `T` falls out of the resolve
    walk and every generated `MethodCall` carries the right
    `ResolvedType::Named { Global.List, [T] }`. Per the northstar
    "typecheck owns AST mutation" rule, IR / eval / LLVM never
    see `ExprKind::List`. Empty `[]` with an annotated target
    (`my_list: List<Int> = []`) inherits `T` from the typed-local
    annotation via the existing bidirectional inference seam.
    Pinned by `crates/koja-alpha-typecheck/tests/list_literal.rs`.
  - **Typed enum intrinsic dispatch via `IRIntrinsicId`.**
    `FunctionKind::Intrinsic` now carries an `IRIntrinsicId`
    enum with nested `KernelMethod` / `CPtrMethod` /
    `CStringMethod` / `BinaryMethod` / `BitsMethod` /
    `ListMethod` / `StringMethod` / `IntMethod` / `FloatMethod` /
    `PrintMethod` / `BitwiseImpl` / `EqualityImpl` / `HashImpl`
    families (`EqualityImpl` and `HashImpl` flatten to
    `Bool` / `Int(IntType)` / `String` siblings so primitive
    receivers stay exhaustive at the match). Both backends key
    intrinsic dispatch on the nested variant, killing the
    string-keyed `id: "Type.method"` lookup table. `IRSymbol`
    stays opaque end-to-end — the receiver string is parsed
    once at IR build time, never at emit time. The IR-level
    extern surface picked up the libc-direct symbols the list /
    string emitters consume (`malloc`, `realloc`, `memcpy`,
    `free`, `strcmp`, `koja_string_get` / `_length` / `_slice`).
  - **List runtime ports v1's libc-direct shape.** The LLVM
    intrinsic emitters in `koja-alpha-ir-llvm/src/intrinsics/list.rs`
    mirror `koja-codegen::list` one-to-one: `new`/`append` call
    `malloc`/`realloc`/`memcpy` directly, `get` returns
    `Option<T>` via the layout-aware enum construction helper
    (see `build_enum_value` below), `pop` returns
    `Pair<Option<T>, List<T>>`, `concat` / `slice` /
    `replace_at` follow the same shape. Eval mints
    `Value::List(Rc<RefCell<Vec<Value>>>)` and routes through
    `koja-alpha-ir-eval/src/intrinsics/list.rs`. Both backends
    cover all ten `ListMethod` variants.
  - **`build_enum_value` factored out of `emit_enum_construct`.**
    Intrinsic emitters that mint `Option::Some(_)` / `None` /
    `Result::Ok(_)` used to hand-GEP raw indices `0` (tag) and
    `1` (payload) on an assumed-flat outer struct — fine in v1,
    broken under alpha's alignment-correct chunk-array outer +
    per-variant `complete` struct. The new
    `pub(crate) emit::enums::build_enum_value(symbol, tag,
payload_values)` helper allocas the outer, GEPs through the
    variant's `complete` (tag at field 0, payload at field 2),
    writes each payload field, and loads the populated outer
    back out — the same path `emit_enum_construct` uses for
    user-land `Enum.Variant(...)` literals. Killed the
    duplicated `build_option_some` / `build_option_none` shims
    in `intrinsics/list.rs` and `intrinsics/string.rs`. The
    `EmitContext::enum_outer_type` accessor replaces the
    redundant `enum_outers` map by delegating to
    `Context::get_struct_type` (the LLVM context's name table
    already keys opaque structs by name, which is what
    `declare_enum_type` registers); a sibling
    `TypeLayouts::struct_field_ir_type` index keeps IR field
    types around post-layout so `List.pop`'s
    `Pair<Option<T>, List<T>>` return can recover the inner
    `Option<T>` symbol without re-deriving from mangled names.
  - **String intrinsic emitters port v1's UTF-8 layout.**
    `String` values are `[i64 bit_length][payload bytes]` with
    the SSA pointer at the payload (header sits 8 bytes back).
    LLVM emits inline for `byte_length` / `to_binary` (header
    arithmetic only) and delegates the codepoint-aware
    `length` / `get` / `slice` to `koja_string_length` /
    `_get` / `_slice` in `koja-runtime` so Unicode walking
    stays in Rust. `Equality.eq` for `String` calls `strcmp`;
    `Hash.hash` for `String` is FNV-1a inlined. `to_cstring`
    allocates a null-terminated `CString` via `malloc` +
    `memcpy`. Eval mirrors with Rust `str` primitives;
    `to_cstring` surfaces `RuntimeError::Unsupported`.
  - **`Random` rides on `string.koja`'s landing.** Now that
    `String.to_binary` is alpha-ready, `random.koja` returns to
    `AUTOIMPORT` (after `Global.string`, since
    `Random.bytes` chains
    `koja_random_bytes(count).to_string().to_binary()`). The
    extern `koja_random_bytes` / `koja_random_int` symbols
    already live in `koja-runtime::system`, so the LLVM path
    links cleanly; eval surfaces them as
    `RuntimeError::ExternNotSupported`.

  Pinned by `crates/koja-alpha-typecheck/tests/{string,random,
list_literal,kernel}.rs`,
  `crates/koja-alpha-ir/tests/lower_list.rs`,
  `crates/koja-alpha-ir-llvm/tests/intrinsics.rs`
  (option/pair-aware GEP shape under chunked-outer enum layout),
  and the `*_list_literal_length_*` / `*_list_get_unwrap_*` /
  `*_string_length_*` / `*_random_int_fixed_*` driver tests in
  `crates/koja-driver/tests/alpha_two_plus_two.rs`. **Out**:
  `Map<K, V>` / `Set<T>` intrinsic surface (next slice); the
  deferred typed-local / runtime narrowing slice still holds
  (`p: CPtr<UInt8> = CPtr.alloc(8)` won't infer `T`).

- **Stdlib slice 4: eval-side C FFI for the autoimported externs.**
  The runtime extern surface and the `CPtr<T>` / `CString` /
  `Random` / `Int.parse` / `Float.parse` / `Binary.to_string` /
  `Bits.to_binary` intrinsics now execute under
  `--backend=interpreter` (eval) as well as `--backend=llvm`, so
  the alpha shell can call `Random.bytes(32)` /
  `Int.parse("42")` / `"hi".to_cstring().to_string()` /
  `Binary.to_string(...)` end-to-end. The compiler-side wiring:
  - **`Value::CPtr(*mut u8)`** lands as a first-class eval value
    variant. Eval is single-threaded and in-process so the
    pointer is valid for the lifetime of its referent (the same
    memory the LLVM backend would observe). Element type `T`
    stays type-level — intrinsic emitters consult
    `function.params[0].ty` / `function.return_type` when they
    need `size_of::<T>()`. `Display` renders as
    `<cptr 0x{addr:x}>` / `<cptr null>` to match the runtime
    printer's shape.
  - **`Value::String(Vec<u8>)`** replaces the old
    `Value::String(String)` so eval can carry the same arbitrary
    byte payloads v1's String type allows. `Random.bytes`'s
    `koja_random_bytes(count).to_string().to_binary()` chain
    flows raw entropy through a `String` value without the
    interpreter rejecting non-UTF-8 payloads — codepoint-walking
    methods (`length`, `get`, `slice`, `parse`) validate UTF-8
    on demand and surface `RuntimeError::Unsupported` for
    malformed input. Byte-oriented methods (`byte_length`,
    `to_binary`, `to_cstring`, `Concat<String>`) work
    unconditionally.
  - **Per-stdlib-file extern modules.**
    `koja-alpha-ir-eval/src/externs/` now matches the
    `lib/global/src/*.koja` layout one-to-one
    (`cptr.rs` ↔ `cptr.koja`'s `strlen`; `kernel.rs` ↔
    `kernel.koja`'s `koja_kernel_exit`; `random.rs` ↔
    `random.koja`'s `koja_random_bytes` + `koja_random_int`;
    `time.rs` ↔ `time.koja`'s `koja_time_now_millis`). Each
    handler `unsafe extern "C"`-declares its C symbol and calls
    straight into `koja-runtime` (or libc for `strlen` / `malloc`
    / `free`), giving eval byte-equivalent behaviour to the
    LLVM backend for the auto-imported externs.
  - **CPtr intrinsic family lands end-to-end on eval.**
    `CPtr.{null, null?, alloc, free, offset, read, write,
to_binary, to_string}` are all implemented; `alloc` /
    `offset` / `read` / `write` consult
    `helpers::size_of_primitive` (mapping `IRType` to byte
    width) for element-aware pointer arithmetic. `CPtr<UInt8>.to_string`
    reads the v1 length-prefixed `[i64 bit_length][payload…]`
    ABI, copies the payload into a `Value::String(Vec<u8>)`,
    and frees the source header chunk — matches the LLVM
    backend's move-self ownership transfer.
  - **`String.to_cstring` + `CString.to_string`** round-trip
    through `malloc` / `memcpy` like the LLVM backend (the
    `RuntimeError::Unsupported` stubs are gone). `Binary.ptr`
    allocates a length-prefixed Koja-string buffer the caller
    owns; `Binary.to_string` / `Bits.to_binary` /
    `Int.parse` / `Float.parse` construct `Result<T, E>` /
    `Option<T>` values via the new
    `intrinsics::helpers::{option_value, result_value,
enum_return_symbol}` shared helpers (deduped from the
    earlier list / string handlers).

  Pinned by
  `crates/koja-alpha-ir-eval/tests/{cptr,parse,random}.rs` and
  the `*_random_int_fixed_*` / `*_random_bytes_size_*` /
  `*_int_parse_*` driver tests in
  `crates/koja-driver/tests/alpha_two_plus_two.rs`. **Out**:
  the LLVM-side `Int.parse` / `Float.parse` runtime helpers
  still trap (eval is ahead of the LLVM backend here); the
  `Map<K, V>` / `Set<T>` slice still owns the next intrinsic
  bring-up.

The roadmap's original Phase 1 (loops) and Phase 2 (closures)
are closed; the strings/binary/bits slice closed the Phase 3
string-related items, the alpha move/drop foundation slice
closed `move` in typecheck, stdlib slice 1 closed the
auto-import substrate plus the first two `Global.*` files,
the literal-fit narrow-int slice closed the Phase 3 numeric
coercion item end-to-end (typecheck → IR lower → eval / LLVM /
driver), stdlib slice 2 closed `kernel` / `cptr` / `cstring`
(with `Result<T, E>` registered, method-level generics, and
bidirectional inference for generic enum construction), and
stdlib slice 3 closed `list` / `string` / `random` (with
`IRType::List`, list-literal desugar, typed `IRIntrinsicId`
dispatch, and the shared `build_enum_value` helper), and
stdlib slice 4 closed the eval-side C FFI surface (so
`Random.bytes(32)`, `Int.parse(...)`, and the `CPtr` /
`CString` family all run in the alpha shell, not just under
`--backend=llvm`). The surviving critical-path work is the
deferred typed-local / runtime narrowing slice
(`p: CPtr<UInt8> = CPtr.alloc(8)` → drive bidirectional
inference through typed-local annotations + a `.to_int8()` /
`.to_uint16()` method family on each numeric type), the next
slice of stdlib intrinsics (building toward `map` / `set` /
`io` / `fd` / `system`), and the optional Phase 4 concurrency
slice (`spawn` / `receive`); after those,
`koja alpha check expo/lib/global/src/*.koja` should be fully
green.

---

Audited 2026-05-10.
