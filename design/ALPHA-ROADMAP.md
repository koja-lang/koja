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
- `expo-alpha-typecheck/src/pipeline/resolve/{expr,ops,statements,strings}.rs`
  (note: `resolve/calls/` is now its own submodule — see
  `mod.rs`, `methods.rs`, `bounded.rs`)
- `expo-alpha-ir/src/lower/{expr,ops,body,structs,enums,package,calls}.rs`
- `expo-alpha-ir-llvm/src/{emit/mod,emit/instruction,main_wrapper}.rs`

---

## Stdlib feature inventory

Cataloguing what the stdlib actually reaches for. Counts are
approximate — the point is which files use a given construct.

| Construct                               | Files / examples                                                                      |
| --------------------------------------- | ------------------------------------------------------------------------------------- |
| `struct` / `enum` / `protocol`          | every file                                                                            |
| Inherent `impl Type`                    | every file                                                                            |
| Trait `impl Protocol for Type`          | `kernel` (Equality, Hash for primitives), `debug`, `string`, `set`, `map`, `list`     |
| Generic types (`<T>`, `<K,V>`, `<U>`)   | `kernel`, `list`, `map`, `set`, `process` (shipped end-to-end)                        |
| Generic impl target (`impl Pair<A,B>`)  | `kernel` (Pair, Option, Result), `list`, `map`, `set`, `process` (shipped end-to-end) |
| `Self` type in signatures               | `bitwise`, `debug`, `set`, `map`, `list`                                              |
| `move` parameter mode                   | `process` (8x), `list` (8x), `map` (5x), `cptr`, `cstring`, `set`, `string` (already shipped) |
| `priv` visibility                       | every FFI-wrapping file                                                               |
| `@intrinsic` / `@extern "C"` / `@doc`   | every file (we shipped `@extern`/`@link`/`AnnotationKind`)                            |
| Closure parameter types (`fn (T) -> U`) | `kernel` (Option/Result map/then), `list` (filter/find/all?/map/reduce), `process`    |
| Closure expressions (block & short)     | none in stdlib bodies (parameters only — closures arrive at call sites in user code)  |
| `match` expression                      | `kernel` (12x — Option/Result), `string` (8x), `process` (7x), `list`, `io`, `fd`     |
| OR pattern (`"a" \| "b" \| ...`)        | `string` (alpha?, digit?, escape_debug, trim\_\*, whitespace?, upcase, downcase)      |
| Tuple pattern (`Some(val)`, `Ok(v)`)    | every match-using file                                                                |
| Wildcard pattern (`_`)                  | every match-using file                                                                |
| **`for` loop**                          | `string` (codepoints, digit?, alpha?, …), `list` (filter/find/all?/any?/map/reduce)   |
| **`while` loop**                        | `string` (10+), `list` (reverse)                                                      |
| `return` statement                      | `string`, `list`                                                                      |
| `break` (in `loop`)                     | none in stdlib                                                                        |
| `unless`                                | `list` (`all?`)                                                                       |
| `if`/`else` value-producing             | `system`, `fd`, `cptr`, `string` (already shipped)                                    |
| `cond`                                  | `string` (alpha?, contains?, ends_with?, starts_with?) (already shipped)              |
| Ternary `cond ? a : b`                  | `fd` (5+), `system` (already shipped)                                                 |
| String concat `<>`                      | `string` (heavy — upcase/downcase/escape_debug), `debug` (`format` for `String`) (already shipped) |
| Compound assign (`+=`, `-=`)            | `string` (15+), `list` (`reverse`) (already shipped)                                  |
| String literal in IR                    | `kernel` (`Kernel.panic("…")`), `string`, `io`, `fd`, `system` — pervasive (already shipped) |
| String interpolation `#{…}`             | none — stdlib avoids it intentionally                                                 |
| **`const` declarations**                | `io` (STDIN, STDOUT, STDERR — struct-literal initializers)                            |
| Numeric coercion (`Int` → `Int32` etc.) | `io` (`Fd{descriptor: 2}`), `fd` (CPtr ops with mixed widths), `time`                 |
| `spawn`                                 | `process` (`Task.async`)                                                              |
| `receive` / `receive ... after`         | `process` (`run` loops)                                                               |
| List / Map / Set literal syntax         | none in stdlib bodies (collections are constructed via `.new()` + `.append`/`.put`)   |
| Bitstring / `<<…>>` literals            | none in stdlib (alpha ships them anyway — gates Phase 7 of the match plan) (already shipped) |
| Nested `MyApp.Config` types             | none in stdlib                                                                        |
| Type unions (`A \| B`)                  | none in stdlib                                                                        |
| `type` aliases inside `impl`            | none in stdlib                                                                        |
| Free functions (no `impl`)              | none — every fn lives in an `impl`                                                    |
| Default field values                    | none in stdlib                                                                        |

The "none in stdlib" rows are useful negative space: those features are
**not** alpha blockers for the stdlib effort, no matter how much they
matter elsewhere.

---

## Gap classification

Every gap below maps to one or more diagnostics already wired into the
alpha crates.

### Blockers — without these the stdlib does not type-check

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

- **`return` from inside `for`/`while`** — `resolve/return_type.rs`
  already handles divergence via `Never`, and `match` arms already
  return early through the same path (e.g. `String.alpha?`'s
  `return false` from inside a match arm typechecks today). The
  "from inside a loop" piece is the one remaining wedge — falls out
  once Phase 1 (loops) is in.

### Significant — required for non-trivial stdlib pieces

- **`spawn` and `receive` in IR** — `resolve/expr.rs` falls through.
  Required only for `process.expo` (`Task.async` + `Process.run`
  defaults). Eval can stub these (no scheduler in alpha-eval); LLVM
  needs the runtime calls already used by v1 codegen.

### Already supported — common false positives

- **Generics — full feature, end-to-end.** Generic types
  (`<T>`, `<K,V>`), generic functions, generic impls (concrete
  `impl Bag<Int>` and trait `impl Show for List<T>`), bounds
  (`<T: Eq>`, `<T: Eq & Hash>`), protocol conformance recording on
  the target's `StructDefinition` / `EnumDefinition`, bounded-method
  dispatch (`t.method()` against the type-param's bounds), trait-impl
  domain check (`Bag<Int>::greet` vs `Bag<String>::greet`), dual-scope
  inference (receiver scope + method scope) at every call site.
  Substitution drives end-to-end through monomorphization
  (`expo-alpha-ir/src/generics/`) and name-mangling
  (`Bag_$Int64$.greet`). Pinned by
  `concrete_impl_specialization.rs`, `bounded_dispatch.rs`,
  `generic_method_inference.rs`, `trait_impl_domain.rs`, and
  `substitution.rs` across `expo-alpha-typecheck`, `expo-alpha-ir`,
  and `expo-alpha-ir-llvm`. **Out**: generic protocol methods
  (i.e. `protocol P { fn m<U>(...) }`) — separate slice; no stdlib
  use.

- **`match` expression — full v1-parity surface.** Patterns:
  literal, wildcard, binding, enum-unit, enum-tuple, enum-struct
  (named-field destructure), struct destructure, or, and constructor
  shorthand (`Some(x)`). Guards (`pattern when expr -> body`).
  Exhaustiveness checking on enum subjects with `Bool`
  specialization. Reachability / redundancy diagnostics (dead
  arms, duplicates, overlaps) as warnings. Pinned by
  `crates/expo-alpha-typecheck/tests/resolve_match.rs`,
  `crates/expo-alpha-ir/tests/lower_match.rs`,
  `crates/expo-alpha-ir-eval/tests/interpreter.rs`,
  `crates/expo-alpha-ir-llvm/tests/control_flow.rs`. **Out**:
  `Pattern::Binary` (gated on binary literals; tracked as Phase 7
  of [ALPHA-MATCH-PLAN.md](ALPHA-MATCH-PLAN.md)).

- **`String` / `Binary` / `Bits` — full surface, end-to-end.**
  String literals lower to `[i64 bit_length][payload bytes][\0]`
  heap globals; `Binary` and `Bits` share the same `[i64
  bit_length][payload]` layout (no NUL, ceil-rounded bytes for
  `Bits`). The polymorphic `<>` concat operator typechecks against
  any of the three (same-type operands, same-type result), with
  `String`/`Binary` lowering to inline `malloc` + `memcpy` and
  `Bits` deferring to `__expo_alpha_concat_bits` for sub-byte
  alignment. `<<segments>>` literal syntax handles integer
  (signed/unsigned, big/little, literal `::N` widths including
  sub-byte), float (`: Float32` / `: Float64`), string segments,
  and the type-annotation form (`: Int16` ≡ `::16 signed big`);
  byte-aligned segments pack inline, sub-byte segments call
  `__expo_alpha_pack_bits`. Total bits `% 8 == 0` → `Binary`,
  else `Bits`. Move/drop integration: heap-typed slots are
  `Owned` at write sites and emit `DropLocal` at scope exit (one
  shape across all three types — `free(payload - 8)`). Auto-print
  routes `String` → `__expo_alpha_print_string`, `Binary` →
  `__expo_alpha_print_binary`, `Bits` → `__expo_alpha_print_bits`,
  with byte-identical output between LLVM and the eval interpreter.
  Pinned by `crates/expo-alpha-typecheck/tests/resolve_binary_literal.rs`,
  `crates/expo-alpha-ir/tests/lower_binary_literal.rs`,
  `crates/expo-alpha-ir-eval/tests/binary_literal.rs`,
  `crates/expo-alpha-ir-llvm/tests/program.rs`, and the e2e
  driver tests in `crates/expo-driver/tests/alpha_two_plus_two.rs`
  (`*_string_concat_*`, `*_binary_literal_*`, `*_bits_literal_*`).
  Alpha **leapfrogs v1 here** for `Bits` — v1's `resolve_binary_segments`
  rejects sub-byte segments and forces `Primitive::Binary`; alpha
  packs them via runtime helpers cleanly. **Out**: stdlib
  conversion methods (`String.to_binary`, `Binary.to_string`,
  `Bits.to_binary`, `String.to_cstring`, `CPtr.to_binary`),
  `Equality` / `Hash` on `Binary` / `Bits`, `Pattern::Binary` in
  `match` (Phase 7 of [ALPHA-MATCH-PLAN.md](ALPHA-MATCH-PLAN.md),
  unblocked by this slice), `<<x::n>>` with runtime-int widths,
  and `Debug.format` rendering (gated on `Debug` protocol synthesis,
  not on the alpha critical path).

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
  generic protocol methods, dotted type names, default field
  values** — none of these appear in `lib/global/src/`. Tracked in
  v1 [GAPS.md](GAPS.md) where applicable.

---

## Recommended sequencing

Order is chosen to maximize what each step _unblocks_, not by
implementation cost in isolation. Each step lands behind seal-asserted
output and standalone tests, per northstar.

### Phase 1 — Loops

- `while` first (lower to a header block with no params + a back-edge
  — the existing `if`/`else` machinery already has every primitive).
- `for` lowers to a `while` over `Enumeration<T>::length` /
  `Enumeration<T>::get` — exactly v1's strategy. The
  `Enumeration<T>` protocol is already in `kernel.expo`.
- `return` from inside loops works out of the box once block-param
  SSA carries divergence (which it does — `Never` joins fine).
- `break` is post-stdlib; nothing in `lib/global/` uses it.

### Phase 2 — Closure parameter types

- Lift `fn (T) -> U` as a `ResolvedType::Function { params, ret }`
  (new variant on `ResolvedType` or a new node type — northstar
  decision).
- No closure-expression lowering needed for stdlib (no stdlib body
  constructs a closure). Just enough to lift the signatures of
  `List.map`/`Option.then`/`Task.async`/etc.
- User-code closure-_expressions_ (block & short) is a separate later
  step, sequenced behind (or alongside) `List`/`Map` literal syntax.

### Phase 3 — Mechanical glue

Small; can land in any order or batched into one PR each.

- **Numeric coercion at struct-literal sites.** Smallest fix:
  `Int` literals coerce to the field's annotated narrower type if the
  value fits. Mirror v1's `numeric_compatible`/`record_coercion`
  exactly; flag the coercion in IR as an explicit
  `IRInstruction::Cast` (per northstar's coercion rule). Unblocks
  the stdlib's `const Fd{descriptor: …}` constants for free.
- ~~**`move` parameters in typecheck signatures.**~~ **Shipped.**
  `lift_signatures` propagates the surface `PassMode` verbatim
  onto `ResolvedParam::mode`; the IR's `ownership_for_param`
  stamps `Owned` on `Move` heap-typed slots, and parameter
  promotion threads the ownership through to the function-exit
  `DropLocal` emission. Pinned by
  `crates/expo-alpha-typecheck/tests/pass_mode.rs` and the
  alpha move/drop foundation slice's IR / eval / LLVM tests.
- ~~**String literals in IR.**~~ **Shipped.** End-to-end through
  `lower/expr.rs` (string literal arm), `emit_const_payload`
  (shared `[i64 bit_length][payload bytes][\0]` heap layout), and
  `__expo_alpha_print_string` for auto-print. Landed alongside
  the strings/binary/bits slice (see "Already supported").
- ~~**`<>` concat operator.**~~ **Shipped.** `IRInstruction::Concat`
  with per-`ConcatKind` emission (inline `malloc`/`memcpy` for
  `String`/`Binary`, `__expo_alpha_concat_bits` runtime helper for
  `Bits`). Same slice as String literals.

### Phase 4 — Concurrency (optional)

- Required only for `process.expo` to type-check & lower. `spawn` and
  `receive` already have AST nodes and runtime support in v1.
- Eval can stub: panic on `spawn`, never-receive on `receive`. The
  goal is for `expo alpha check` to pass on `process.expo`, not for
  the alpha eval to actually run processes.
- LLVM emits `expo_rt_spawn` / `expo_rt_receive*` calls (same symbols
  v1 codegen uses).

After Phase 4, `expo alpha check expo/lib/global/src/*.expo` should be
fully green. Phases 1–3 alone get every non-`process.expo` file green,
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

## Status snapshot (post-generics, post-`match`, post-move/drop, post-strings/binary/bits)

What's shipped since the last audit:

- **Generics — full feature, end-to-end (~6 kLOC).** Generic types
  (`<T>`, `<K, V>`), generic functions, generic impls (concrete
  `impl Bag<Int>` and trait `impl Show for List<T>`), bounds
  (`<T: Eq>`, `<T: Eq & Hash>`), protocol conformance recording,
  trait-impl domain check, bounded-method dispatch (`t.method()`
  against type-param bounds), and dual-scope inference (receiver
  scope + method scope). Substitution drives end-to-end through
  monomorphization (`expo-alpha-ir/src/generics/`) and
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
  `crates/expo-alpha-typecheck/tests/pass_mode.rs`,
  the move/drop unit tests across `expo-alpha-ir/tests/` and
  `expo-alpha-ir-llvm/tests/`, and the eval `value_drops`
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
  widths (with `__expo_alpha_pack_bits` runtime helper for
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

The roadmap's original Phase 1 is closed; the strings/binary/bits
slice closed the Phase 3 string-related items, and the alpha
move/drop foundation slice closed `move` in typecheck. Loops and
the surviving Phase 3 mechanical-glue item (numeric coercion at
struct-literal sites) are the next critical-path slices.

---

Audited 2026-05-08.
