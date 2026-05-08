# Alpha Roadmap

Sequencing for getting `expo/lib/global/src/` compiling end-to-end through
the alpha pipeline (`expo-alpha-typecheck` → `expo-alpha-ir` →
`expo-alpha-ir-llvm` / `expo-alpha-ir-eval`).

The goal is **stdlib parity, not v1 parity**: alpha needs the surface area
that `lib/global/src/*.expo` exercises. Anything outside the auto-imported
package (Net, HTTP, JSON, …) is explicitly deferred — the stdlib stays
enum-first and structurally as-is; this doc just enumerates which compiler
features have to land for it to type-check and lower.

For pipeline shape and seal contracts, see
[COMPILER-NORTHSTAR.md](COMPILER-NORTHSTAR.md). For non-alpha v1 gaps, see
[GAPS.md](GAPS.md).

---

## Goal: compile the stdlib

Concrete success criterion: `expo alpha check` and `expo alpha run` (where
applicable — concurrency primitives stay stubbed) succeed on every file
under `expo/lib/global/src/`:

```
bitwise.expo  cptr.expo    cstring.expo  debug.expo  fd.expo
io.expo       kernel.expo  list.expo     map.expo    process.expo
set.expo      string.expo  system.expo   time.expo
2356 LOC total
```

Today they don't, by a lot. The audit below enumerates why.

---

## Audit method

For each `.expo` source under `lib/global/src/`, grep for the language
constructs it uses (`match`, `for`, closure types, `<>` concat, ternary,
`@extern`, generic impls, …), then cross-reference against the
`"alpha (typecheck|IR|LLVM) does not yet ..."` diagnostics in
`crates/expo-alpha-*/src/`. Anything the stdlib reaches for that alpha
diagnoses (or silently mishandles) is a blocker.

The full list of "not yet" diagnostics lives in:

- `expo-alpha-typecheck/src/pipeline/collect.rs`
- `expo-alpha-typecheck/src/pipeline/lift_signatures/{functions,types,constants}.rs`
- `expo-alpha-typecheck/src/pipeline/resolve/{expr,ops,calls,statements,strings}.rs`
- `expo-alpha-ir/src/lower/{expr,ops,body,structs,enums,package}.rs`
- `expo-alpha-ir-llvm/src/{emit/mod,emit/instruction,main_wrapper}.rs`

---

## Stdlib feature inventory

Cataloguing what the stdlib actually reaches for. Counts are
approximate — the point is which files use a given construct.

| Construct                                  | Files / examples                                                                       |
| ------------------------------------------ | -------------------------------------------------------------------------------------- |
| `struct` / `enum` / `protocol`             | every file                                                                             |
| Inherent `impl Type`                       | every file                                                                             |
| Trait `impl Protocol for Type`             | `kernel` (Equality, Hash for primitives), `debug`, `string`, `set`, `map`, `list`      |
| Generic types (`<T>`, `<K,V>`, `<U>`)      | `kernel`, `list`, `map`, `set`, `process`                                              |
| **Generic impl target** (`impl Pair<A,B>`) | `kernel` (Pair, Option, Result), `list`, `map`, `set`, `process` (Ref, ReplyTo, Step…) |
| `Self` type in signatures                  | `bitwise`, `debug`, `set`, `map`, `list`                                               |
| `move` parameter mode                      | `process` (8x), `list` (8x), `map` (5x), `cptr`, `cstring`, `set`, `string`            |
| `priv` visibility                          | every FFI-wrapping file                                                                |
| `@intrinsic` / `@extern "C"` / `@doc`      | every file (we shipped `@extern`/`@link`/`AnnotationKind`)                             |
| Closure parameter types (`fn (T) -> U`)    | `kernel` (Option/Result map/then), `list` (filter/find/all?/map/reduce), `process`     |
| Closure expressions (block & short)        | none in stdlib bodies (parameters only — closures arrive at call sites in user code)   |
| **`match` expression**                     | `kernel` (12x — Option/Result), `string` (8x), `process` (7x), `list`, `io`, `fd`      |
| **OR pattern** (`"a" \| "b" \| ...`)       | `string` (alpha?, digit?, escape_debug, trim\_\*, whitespace?, upcase, downcase)       |
| **Tuple pattern** (`Some(val)`, `Ok(v)`)   | every match-using file                                                                 |
| **Wildcard pattern** (`_`)                 | every match-using file                                                                 |
| **`for` loop**                             | `string` (codepoints, digit?, alpha?, …), `list` (filter/find/all?/any?/map/reduce)    |
| **`while` loop**                           | `string` (10+), `list` (reverse)                                                       |
| `return` statement                         | `string`, `list`                                                                       |
| `break` (in `loop`)                        | none in stdlib                                                                         |
| `unless`                                   | `list` (`all?`)                                                                        |
| `if`/`else` value-producing                | `system`, `fd`, `cptr`, `string` (just shipped)                                        |
| `cond`                                     | `string` (alpha?, contains?, ends_with?, starts_with?) (just shipped)                  |
| **Ternary `cond ? a : b`**                 | `fd` (5+), `system`                                                                    |
| **String concat `<>`**                     | `string` (heavy — upcase/downcase/escape_debug), `debug` (`format` for `String`)       |
| **Compound assign** (`+=`, `-=`)           | `string` (15+), `list` (`reverse`)                                                     |
| **String literal in IR**                   | `kernel` (`Kernel.panic("…")`), `string`, `io`, `fd`, `system` — pervasive             |
| **String interpolation `#{…}`**            | none — stdlib avoids it intentionally                                                  |
| **`const` declarations**                   | `io` (STDIN, STDOUT, STDERR — struct-literal initializers)                             |
| Numeric coercion (`Int` → `Int32` etc.)    | `io` (`Fd{descriptor: 2}`), `fd` (CPtr ops with mixed widths), `time`                  |
| `spawn`                                    | `process` (`Task.async`)                                                               |
| `receive` / `receive ... after`            | `process` (`run` loops)                                                                |
| List / Map / Set literal syntax            | none in stdlib bodies (collections are constructed via `.new()` + `.append`/`.put`)    |
| Bitstring / `<<…>>` literals               | none in stdlib                                                                         |
| Nested `MyApp.Config` types                | none in stdlib                                                                         |
| Type unions (`A \| B`)                     | none in stdlib                                                                         |
| `type` aliases inside `impl`               | none in stdlib                                                                         |
| Free functions (no `impl`)                 | none — every fn lives in an `impl`                                                     |
| Default field values                       | none in stdlib                                                                         |

The "none in stdlib" rows are useful negative space: those features are
**not** alpha blockers for the stdlib effort, no matter how much they
matter elsewhere.

---

## Gap classification

Every gap below maps to one or more diagnostics already wired into the
alpha crates.

### Blockers — without these the stdlib does not type-check

- **`match` expressions** — `resolve/expr.rs:104` falls through to
  "alpha typecheck does not yet support expression `match`". Touches
  every "interesting" file in the stdlib (12 sites in `kernel` alone).
  Patterns the stdlib actually uses: enum-tuple (`Option.Some(val)`),
  enum-unit (`Option.None`), wildcard (`_`), literal (`"a"`, `0`),
  and OR (`"a" | "b" | ...`). No struct patterns, no nested patterns,
  no guards, no typed bindings — small subset relative to v1.

- **Loops — `for` and `while`** — `resolve/expr.rs` falls through;
  `lower/expr.rs:211` reports "alpha IR does not yet lower this
  expression kind". `for` is the iteration primitive built on
  `Enumeration<T>`; `while` powers `string` and `list.reverse`. The
  block-parameter SSA join we just shipped
  (`alpha-if-cond-blockparams`) is the substrate — loops lower the
  same way: a header block with parameters carrying the loop-carried
  state, a body, a back-edge.

- **Closure parameter types (`fn (T) -> U`)** —
  `lift_signatures/types.rs:71` diagnoses
  "function-typed annotations not yet supported". Stdlib bodies never
  construct closures (those arrive at call sites in user code), but
  every higher-order method signature
  (`List.map`, `Option.then`, `Task.async`, …) needs the type to lift.

- **Numeric coercion at struct-literal sites** —
  `io.expo` writes `const STDOUT: Fd = Fd{descriptor: 1}` where `1` is
  `Int` and `Fd.descriptor: Int32`. Already fails today on user code
  (`thing.Yeah`). Strict-equality is the alpha policy; this is the
  narrowest adjustment that unblocks the stdlib.

- **`move` parameters in typecheck** —
  `lift_signatures/functions.rs:330` diagnoses "alpha typecheck does
  not yet support `move` parameters in fn signatures". `process.expo`
  alone has 8 `move self` methods; `list.expo` has 8 (`append`,
  `concat`, `pop`, …). Today the diagnostic blocks the entire impl
  from lifting.

- **String literals in IR** — `lower/ops.rs:59` ("does not yet lower
  String literals"). Typecheck already resolves them; only IR
  lowering / runtime constant pooling is missing.
  `Kernel.panic("called unwrap on None")` and every quoted literal in
  `string.expo` / `io.expo` / `fd.expo` / `system.expo` needs it.

- **`<>` concat operator** — `lower/ops.rs:89` ("does not yet lower
  the `<>` concat operator"). Lands as part of the same binary /
  bits-and-strings runtime work — `<>` is the polymorphic concat
  that bridges String / Binary / Bits, so it slots in alongside the
  binary literal & bitstring story rather than as a one-off.

- **Ternary `?:`** — `resolve/expr.rs` falls through. `fd.expo` uses
  the form 5 times in idiomatic FFI-result wrapping
  (`result >= 0 ? Result.Ok(...) : Result.Err(...)`); rewriting to
  `if`/`else` works today but produces ugly stdlib code. Lower as
  sugar over `if`/`else` (block params already in place).

- **`return` from inside `for`/`while`** — `resolve/return_type.rs`
  handles divergence via `Never`, but `match` arms in stdlib also
  return early from inside an enclosing loop (e.g. `String.alpha?`).
  Falls out once `match` and loops are in.

### Significant — required for non-trivial stdlib pieces

- **`spawn` and `receive` in IR** — `resolve/expr.rs` falls through.
  Required only for `process.expo` (`Task.async` + `Process.run`
  defaults). Eval can stub these (no scheduler in alpha-eval); LLVM
  needs the runtime calls already used by v1 codegen.

### Already supported — common false positives

- **Generic impl targets** (`impl Pair<A, B>`, `impl Option<T>`,
  `impl Greeter for Bag<T>`, …). The diagnostic at
  `pipeline/collect.rs:260` ("alpha typecheck does not yet support
  generic impl targets") fires only when the target path is _not_
  length-1 (e.g. `impl pkg.Foo`, function types). `simple_named_target`
  accepts `TypeExpr::Named { path, .. } |
  TypeExpr::Generic { path, .. } if path.len() == 1` — exactly the
  shape every stdlib impl uses. `lift_signatures/impls.rs` resolves
  the target (concrete or generic), threads it as the `self` override
  into method lifting; the `expo-alpha-ir/src/generics/` monomorphizer
  carries it the rest of the way. Pinned by
  `concrete_impl_specialization.rs` (`impl Bag<Int>`) and
  `bounded_dispatch.rs::bounded_dispatch_generic_struct_receiver_resolves_through_substitution`
  (`impl Greeter for Bag<T>` mono'd to `Bag_$Int64$.greet`).

- **Const with struct literals.** `const STDOUT: Fd = Fd{descriptor: 1}`
  (used in `io.expo`) is wired end-to-end:
  `lift_signatures/constants.rs::struct_construction_type` validates
  non-generic struct literals;
  `lower/constants.rs::lower_constant_value` pools them as
  `IRConstantValue::Struct`. The `io.expo` STDIN / STDOUT / STDERR
  constants are blocked _only_ by the numeric-coercion blocker (the
  `1` literal is `Int`, `Fd.descriptor: Int32`, strict-equality
  rejects). The diagnostic at `lift_signatures/constants.rs:261` is
  for **generic** struct literals in `const`, which the stdlib doesn't
  use.

- **Compound assignment (`+=`, `-=`).** Wired through both typecheck
  (`resolve/statements.rs::Statement::CompoundAssign`) and IR
  (`lower/body.rs:126`). `string.expo` exercises `i += 1` / `i -= 1`
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
  generic protocol methods, dotted type names, default field values,
  binary literals** — none of these appear in `lib/global/src/`.
  Tracked in v1 [GAPS.md](GAPS.md) where applicable.

---

## Recommended sequencing

Order is chosen to maximize what each step _unblocks_, not by
implementation cost in isolation. Each step lands behind seal-asserted
output and standalone tests, per northstar.

### Phase 1 — `match`

- AST is already there (`ExprKind::Match`, `Pattern`, `MatchArm`).
- Typecheck: arm-tail join (already have `body_tail_type` /
  `join_arm_tails` from the if/else work; `match` reuses these
  verbatim), pattern resolution (only the subset stdlib uses:
  enum-tuple, enum-unit, wildcard, literal, OR), exhaustiveness check
  for enum subjects.
- IR lowering: leverages block-parameter SSA. Each arm is a basic
  block with the merge taking the result via a `BlockParam`. Pattern
  matching lowers to a switch terminator (or a chain of conditional
  branches when the discriminant is small). OR patterns split each
  alternative into its own predecessor block branching to a shared
  arm body.
- Eval: pattern interpreter — straight `match` on `Value`.
- LLVM: phi at the merge block (already in place); discriminant load +
  conditional branch tower or LLVM `switch` for enums.

This is the single highest-leverage blocker. The block-parameter work
we just shipped was sequenced specifically to make `match` cheap.
Estimated lift: comparable to `if`/`else`/`cond` combined, since the
control-flow shape is the same and the pattern subset is narrow.

### Phase 2 — Loops

- `while` first (lower to a header block with no params + a back-edge
  — the existing `if`/`else` machinery already has every primitive).
- `for` lowers to a `while` over `Enumeration<T>::length` /
  `Enumeration<T>::get` — exactly v1's strategy. The
  `Enumeration<T>` protocol is already in `kernel.expo`.
- `return` from inside loops works out of the box once block-param
  SSA carries divergence (which it does — `Never` joins fine).
- `break` is post-stdlib; nothing in `lib/global/` uses it.

### Phase 3 — Closure parameter types

- Lift `fn (T) -> U` as a `ResolvedType::Function { params, ret }`
  (new variant on `ResolvedType` or a new node type — northstar
  decision).
- No closure-expression lowering needed for stdlib (no stdlib body
  constructs a closure). Just enough to lift the signatures of
  `List.map`/`Option.then`/`Task.async`/etc.
- User-code closure-_expressions_ (block & short) is a separate later
  step, sequenced behind (or alongside) `List`/`Map` literal syntax.

### Phase 4 — Mechanical glue

Small; can land in any order or batched into one PR each.

- **Numeric coercion at struct-literal sites.** Smallest fix:
  `Int` literals coerce to the field's annotated narrower type if the
  value fits. Mirror v1's `numeric_compatible`/`record_coercion`
  exactly; flag the coercion in IR as an explicit
  `IRInstruction::Cast` (per northstar's coercion rule). Unblocks
  the stdlib's `const Fd{descriptor: …}` constants for free.
- **`move` parameters in typecheck signatures.** Already lowers in
  IR (move semantics are pipeline-internal). The diagnostic in
  `lift_signatures/functions.rs:330` is the only thing blocking.
- **String literals in IR.** Wire `expo-alpha-ir/src/lower/ops.rs`
  and `expo-alpha-ir-llvm` to the existing runtime `String` constants;
  typecheck already resolves them. `<>` concat lands separately
  alongside binary / bitstring support.
- **Ternary** — desugar at IR-lowering time to the same `if`/`else`
  shape we just shipped. Trivial.

### Phase 5 — Concurrency (optional)

- Required only for `process.expo` to type-check & lower. `spawn` and
  `receive` already have AST nodes and runtime support in v1.
- Eval can stub: panic on `spawn`, never-receive on `receive`. The
  goal is for `expo alpha check` to pass on `process.expo`, not for
  the alpha eval to actually run processes.
- LLVM emits `expo_rt_spawn` / `expo_rt_receive*` calls (same symbols
  v1 codegen uses).

After Phase 5, `expo alpha check expo/lib/global/src/*.expo` should be
fully green. Phases 1–4 alone get every non-`process.expo` file green,
which is most of the stdlib by line count.

---

## Out of scope for alpha

These features appear in v1 but are explicitly _not_ on the alpha
roadmap, even after stdlib parity:

- **String interpolation.** Stdlib avoids it; users tolerate
  qualifying with `<>` until alpha closes.
- **Free functions.** Every stdlib fn lives in an `impl`; alpha
  enforces the same shape.
- **Bitstring / binary literals.** Networking & cryptographic code
  depends on these; that code lives outside the auto-imported
  package and isn't on the alpha critical path.
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

## Status snapshot (post-`alpha-if-cond-blockparams`)

What just shipped (block-parameter SSA join):

- `if`/`else` and `cond` are value-producing, with `Never` as the
  lattice bottom for diverging arms.
- `IRBasicBlock.params: Vec<BlockParam>` and
  `IRTerminator::{Branch, CondBranch}` carry `BranchTarget { block,
args }`.
- LLVM emission goes block-params → phi nodes; CFG reachability
  analysis emits `unreachable` for dead merge blocks.
- `ResolvedType::Never` is a first-class lattice bottom in typecheck;
  the IR side maps it to `IRType::Unit` (LLVM elides Unit-typed phis
  entirely).

This substrate is exactly what `match` and the loops want. Phase 1
above starts from a load-bearing position rather than a green field.

---

Audited 2026-05-07.
