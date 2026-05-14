# Known Compiler Gaps

Known limitations, bugs, and workarounds in the Expo compiler. New gaps
should be added here as they're discovered (agent testing, self-hosting,
etc.). For the full design of the iterator protocol replacement, see
[TYPES.md](TYPES.md).

---

## Generic enum unit variants in top-level code

`Option.None` cannot infer `T` without usage context in bare declarations.

**Workaround:** variable type annotations (`z: Option<Int32> = Option.None`).
Inside monomorphized method bodies and closures with return type annotations,
generic enum construction resolves all type parameters automatically.

Also affects generic function calls where one argument is a generic unit
variant: `Pair.new(self, Option.None)` in a function returning
`Pair<Lexer, Option<String>>` fails to infer `A` and `B` because the return
type isn't propagated into the call. Workaround: use struct literals directly
(`Pair{first: self, second: Option.None}`) where the return type annotation
provides context, or bind with a type annotation first.

---

## `ref T` parsed but deferred

The type checker parses `ref T` but defers it. Redundant with
borrow-by-default semantics. Revisit if a concrete use case emerges.

---

## Iteration protocol limits (`Enumeration<T>`)

`Enumeration<T>` requires `length()` + `get(index)`, locking `for` to
index-based while loops. This precludes lazy iteration, streaming, and any
non-random-access collection (maps, linked lists, generators).

Pre-v1.0, replace with an `Iterator<T>` protocol using
`next(move self) -> Option<Pair<T, Self>>`. `get` now returns `Option<T>`.
Codegen change is contained to `compile_for` in `loops.rs`; List/String
impls wrap existing index-based access in iterator state.

The current `for` loop hides the `Option` from the user (unwraps
automatically since iteration is bounds-checked). With lazy iteration,
`Option` becomes the termination mechanism -- `for` desugars to
`loop { match iter.next() ... }` and `None` breaks the loop.

Full design in [TYPES.md](TYPES.md) "Iterator protocol redesign" section.

---

## Cached impl ASTs are pre-typecheck clones

`expo-typecheck/src/collect.rs` clones every `ImplBlock` into
`ctx.generic_impl_asts` and `ctx.specialized_impl_asts` _before_
`check.rs` runs. Type-checking mutates `module.items` in place (populating
`Expr::resolved_type` etc.), so the cached clones used by codegen never
see those mutations. Same story for protocol-default bodies stored in
`ctx.synthesized_default_fns`.

Today's `compile_match` hides this by emitting the subject first and
reading `subject_tv.expo_type` from codegen's own type tracking; pure
lower-then-emit splits in IR can't rely on `subject.resolved_type` because
of this gap. (See the doc comment on `patterns.rs::compile_match` for the
"why pre-emit" rationale.)

A naive fix -- writing the typechecked `impl_block` back into both caches
keyed by `Span`, plus running a `rebuild_impl_asts_from_modules` pass after
context merge -- gets `test-rust` green but still leaves `test-stdlib`
failing on protocol-default bodies (their synthesized functions share
spans across impls and the dedupe-by-span logic in `TypeContext::merge`
prefers the stale clone).

**Fix:** make the caches store references / IDs back into `module.items`
so there's only one source of truth, or have `synthesize_protocol_defaults`
type-check its outputs eagerly so the stored AST is authoritative.

**Current state (Apr 2026):** the user-visible symptoms previously
catalogued as separate "user-defined generic types" gaps (static-method
type inference, generic methods on generic impls, struct construction
inside impl method bodies) are now papered over by:

- `infer_arg_expo_type` consulting `expr.resolved_type` as a fallback so
  literal call-site arguments still drive type-arg inference;
- `lookup_struct_info` and `try_parse_mangled_name` routing through the
  package-aware bare-name resolvers so missing `resolved_type` on cached
  AST nodes inside impl method bodies doesn't block construction.

Those fallbacks keep the call-site/construction surface working for
v0.10. The underlying cache duplication is still here and will keep
biting deeper IR splits (`compile_match` in particular) until the cache
is fixed for real.

Surfaced during Stage 5 of the fix-generic-impl-typecheck plan; that stage
is paused until this is sorted.

---

## `try_parse_mangled_name` strips package prefix before AST lookup

`expo-ir/src/lower/mangling.rs::try_parse_mangled_name` strips the package
prefix from the base of a flat-mangled name (e.g. `pkg.MyBox_$Int$` →
`MyBox`) before looking it up in `generic_struct_asts` /
`generic_enum_asts`, then re-packages via `resolve_name_current` using the
current codegen scope. This works because those caches are keyed by bare
names today, but it introduces a cross-package collision risk: if package
`a` is being compiled and encounters a substituted mangled name from
package `b` (e.g. `b.Box_$Int$`) while `a` _also_ defines a generic
`Box<T>`, `resolve_name_current` will prefer `a.Box` and produce
`Type::Named { id: a.Box, type_args: [Int] }` for what was originally
`b.Box<Int>`. Same-package generics (the only shape exercised by current
tests and stdlib) are always correct.

The flat-mangled form itself is the real culprit. `Type::substitute`
intentionally collapses fully-monomorphized `Type::Named { id, type_args }`
into `Type::Named { id: unresolved("pkg.Type_$args$"), type_args: [] }` to
encode "no further substitution needed", and the
`try_parse_mangled_name` machinery is the bridge that recovers structure.

**Resolution plan:** the EXPOIR refactor threads structured
`Type::Named { id, type_args }` end-to-end (no flat-mangled form in IR),
which deletes both `try_parse_mangled_name` and this collision risk. As a
smaller pre-EXPOIR fix, swap `substitute()` for `substitute_preserving()`
in `resolve_method_signature` so the structured form survives the impl
boundary -- but auditing every `substitute()` consumer for the change has
broader surface than the current fallback.

If we ever ship cross-package generic reuse before EXPOIR is done, add a
debug assertion in `try_parse_mangled_name` that warns when the bare-name
strip produces a different `TypeIdentifier` than the original
`pkg.Type` would have, so the collision shows up as a clear failure
rather than a silent miscompilation.

Surfaced as a known follow-up while landing the GAPS 2/3/5 generics fix
(April 2026).

---

## `Debug.format` for tuple variants drops payloads beyond the first

The auto-derived `Debug` implementation only renders the payload of
single-arg tuple variants. Multi-arg tuple variants render only the variant
name and recursive payloads through deeply-nested constructions print only
the head:

```expo
enum Shape
  Circle(Int)
  Rect(Int, Int)
end

print(Shape.Circle(5))    # "Circle(5)"        (correct)
print(Shape.Rect(3, 4))   # "Rect"             (payload dropped)
```

```expo
enum Expr
  Num(Int)
  Add(Expr, Expr)
end

print(Expr.Add(Expr.Num(1), Expr.Num(2)))   # "Add"
print(Expr.Num(1))                          # "Num(1)"
```

The single-arg path in `expo-codegen/src/debug.rs` works; the multi-arg
case appears to short-circuit before formatting the tuple body. Fix should
also exercise nested cases (variant inside variant) since printing is the
default debug surface.

Surfaced during agent compiler-fuzz testing (April 2026).

---

## Nested type-aliased unions don't expand inner aliases

A `type` alias whose RHS is a union of unions leaves the inner alias
unexpanded in the type checker, causing both arm-membership errors and a
spurious `unknown` member in the union:

```expo
type AB = A | B
type ABC = AB | C

abc: ABC = ...
match abc
  x: A -> ...    # error: type `A` is not a member of union `C | unknown`
  x: B -> ...
  x: C -> ...
end
# also: error: non-exhaustive match on union type: missing `unknown`
```

Widening (`abc: ABC = ab` where `ab: AB`) is accepted; the bug is in how
`ABC`'s definition is resolved. The inner `AB` alias doesn't get expanded
into its members, leaving the union as effectively `<unresolved> | C` and
later normalized to `C | unknown`.

**Workaround:** flatten unions manually -- write `type ABC = A | B | C`
instead of composing aliases.

Surfaced during agent compiler-fuzz testing (April 2026).

---

## Bare closure expression as a statement fails to parse

A `fn (...)` closure used as an expression-statement (no surrounding
assignment, return, or call) is misparsed as a nested function declaration
and produces a cascade of errors complaining about a missing identifier
between `fn` and `(`:

```expo
fn main
  fn (x: Int) -> Int x + 1 end   # error: expected identifier, found LParen
  print("ok")
end
```

The issue is purely syntactic -- assigning the closure first
(`f = fn (x: Int) -> Int x + 1 end`) parses fine. In practice this matters
inside method bodies that try to return a closure as the final expression,
because the parser hits the same `fn (` start-of-statement ambiguity:

```expo
impl Foo
  fn make(self) -> fn (Int) -> Int
    fn (x: Int) -> Int x + 1 end   # same parse error
  end
end
```

**Workaround:** bind the closure to a local first and return the local
(`f = fn ... end; f`).

**Fix sketch:** when `parse_statement` sees `fn` followed by `(`, treat it
as an expression-statement (closure) rather than a function declaration.

Surfaced during agent compiler-fuzz testing (April 2026).

---

## Closures inside impl methods cannot capture `self`

A closure created inside an `impl` method that references `self`
(directly or through field access) is rejected with a misleading
"self used outside of impl method" error pointing at the struct
declaration, not the offending closure:

```expo
impl Counter
  fn make_adder(self) -> fn (Int) -> Int
    f = fn (x: Int) -> Int
      x + self.value     # error: self used outside of impl method
    end
    f
  end
end
```

Capturing through a local works (`v = self.value` then capture `v`), so
the closure capture machinery is fine -- the limitation is that `self`
specifically isn't visible from inside a nested closure scope. The error
span is also wrong (it points at the struct decl rather than the `self`
reference inside the closure).

**Workaround:** copy the relevant fields into locals before constructing
the closure.

Surfaced during agent compiler-fuzz testing (April 2026).

---

## Specialized impl loses concrete type when the type parameter recurses through itself

For an `impl` specialized to a self-nested instantiation
(`impl Box<Box<Int>>`), inner field access is type-checked using the
struct's _generic_ parameter rather than the inner concrete substitution:

```expo
struct Box<T>
  value: T
end

impl Box<Box<Int>>
  fn get_inner(self) -> Int
    self.value.value     # error: field access on non-struct type `T`
  end
end
```

`self.value` is correctly typed as `Box<Int>`, but the next field access
sees that inner `Box`'s declared field type as the original `T` and
refuses the field access. Specializations to a single concrete level
(`impl Box<Int>` where `self.value` is `Int`, or `impl Box<Inner>` for
some non-generic struct `Inner`) work correctly -- the bug is specifically
the case where the specialization substitutes the same generic shape.

**Workaround:** decompose the access through a local
(`inner = self.value; inner.value`), or lift the helper to a free
function that takes the inner type explicitly.

Surfaced during agent compiler-fuzz testing (April 2026).

---

## Nested types (`MyApp.Config`) deferred

Declaring a `struct` or `enum` inside another `struct`/`enum` body, accessed
via dotted syntax (`MyApp.Config`, `Lexer.Token`, `Json.Decoder`), is not
supported. The struct/enum body parser in
`expo-parser/src/decl.rs` only accepts fields and inline `fn` methods --
nested type items would need to be allowed in the same loop. Collection in
`expo-typecheck/src/collect.rs` would need to recurse into bodies and
register nested decls under their dotted name.

The naming machinery is already friendly: `TypeIdentifier.name` is an
opaque `String`, and `qualified_name()` / `mangle_name` preserve dots, so
`name = "MyApp.Config"` flows through codegen registration with zero
changes. Identity stays at `(package, name)` -- no `DefId` overhaul needed
(unlike local-types-in-function-bodies).

The two real obstacles:

1. **`path.len() == 2` resolver assumption.**
   `expo-typecheck/src/types.rs::resolve_type_expr_full` treats a 2-segment
   path as `package.Type`. We'd need a third precedence rule for
   `OuterType.NestedType` and a tie-break when both interpretations exist
   (e.g. an aliased package whose name shadows a local type).

2. **`Foo.Bar` ambiguity with enum variants.**
   The parser sends both `Color.Red` (variant) and `MyApp.Config` (would-be
   nested type) down the same enum-construction AST shape. Today
   `expo-typecheck/src/expr.rs::infer_enum_construction` only succeeds if
   the head is an enum; the fallback would need to also try resolving the
   path as a nested type when followed by a struct literal or in type
   position.

Side bits: `classify_impl_target` in `check.rs` only handles
`path.len() == 1`, so `impl MyApp.Config` would need a one-line extension.
Bare `Config` resolving to `MyApp.Config` inside `impl MyApp` (the
"implicit prefix" nicety) would add ~1-2 days of `CheckEnv` plumbing and
can be deferred to a v2 by requiring fully-qualified names initially.

**Cost estimate:** ~1-2 weeks for non-generic nested types; +1-2 more
weeks for generics.

**Why deferred:** much cheaper than local-types-in-function-bodies but
still a sizeable feature; not a 1.0 blocker. Tracked here so the design
analysis isn't lost.

---

Audited 2026-05-03

# Audit: AST / grammar / LANGUAGE.md / ROADMAP.md / IR / codegen drift

Inventory of every discrepancy between `expo-ast`, `expo-parser`,
`grammar.ebnf`, `LANGUAGE.md`, `design/ROADMAP.md`, and downstream
`expo-ir` / `expo-codegen` (non-alpha). Grouped by category so each item
can be triaged independently: remove the cruft, tighten the AST, or just
reconcile the docs.

## A. Dead end cruft (parsed, but dies at codegen or earlier)

### A3. `ExprKind` variants that only go through the legacy codegen path (never real IR)

These lower to `IRInstruction::Stub`, which `expo-codegen`'s instruction
executor unwraps back into the legacy AST emitter at
[instructions.rs:369-378](../crates/expo-codegen/src/control/instructions.rs).
They work, but represent IR gaps, not AST gaps:

- `Closure`, `ShortClosure` → `compile_closure_core` ([expr.rs:296-302](../crates/expo-codegen/src/expr.rs))
- `List`, `Map` → `compile_list_literal`, `compile_map_literal`
- `For` → `compile_for` (note: there IS a `lower_for` in `loops.rs` but it's dead code — never called)
- `Receive`, `Spawn` → `compile_receive`, `compile_spawn`
- `Literal::Unit` → falls through `resolve_const_inline` → Stub → legacy

**Action:** not AST cruft. Flag for the EXPOIR roadmap /
stub-categorization doc — no changes to AST/grammar/LANGUAGE needed.

---

## B. AST shapes never produced by the parser (dead AST subspace)

### B1. `AssignTarget::Pattern` with non-trivial patterns

- **Grammar:** [grammar.ebnf:150-152](../grammar.ebnf) `assignment = IDENT, ":", type_expr, "=", expr | lvalue, "=", expr | pattern, "=", expr`.
- **AST:** [ast.rs:410-415](../crates/expo-ast/src/ast.rs) `AssignTarget::LValue | AssignTarget::Pattern`.
- **Parser reality:** `try_expr_to_pattern` ([stmt.rs:146-157](../crates/expo-parser/src/stmt.rs)) only accepts `Ident` (→ `Binding`) and `_` (→ `Wildcard`). List, struct, enum, and OR patterns on assignment LHS are _not_ parseable today.
- **Typecheck:** `AssignTarget::Pattern(_) => {}` is empty ([stmt.rs:130-131](../crates/expo-typecheck/src/stmt.rs)).
- **Codegen:** errors with `destructuring patterns not yet supported` ([stmt.rs:278-280](../crates/expo-codegen/src/stmt.rs)) for anything non-trivial.
- **LANGUAGE.md lines 1831-1836:** "parsed and/or type-checked" — overstates.

**Action:** tighten `AssignTarget` to
`AssignTarget::LValue(LValue) | AssignTarget::Binding { name, wildcard: bool }` —
or just keep `Pattern` but document the pattern subspace actually
accepted. Update LANGUAGE.md Planned Features to read "designed; not
parsed yet".

### B2. `ClosureParam::Destructured` inside `ShortClosure`

- **Grammar:** line 246-247 allows `(a, b) -> expr`.
- **Parser:** `expr_to_closure_params` ([construct.rs:474-492](../crates/expo-parser/src/construct.rs)) handles only `Ident`, `_`, and single-element `Group`. A parenthesized list short closure collides with `parse_paren_expr` tuple rejection.
- **Block `Closure`:** `Destructured` _is_ produced from `parse_closure_params` ([construct.rs:424-435](../crates/expo-parser/src/construct.rs)). So the variant isn't dead overall — just dead for short closures.

**Action:** either remove destructured-form from `closure_param_short`
in grammar, or implement it. Grammar line 246-247 is the liar today.

---

## C. Grammar.ebnf vs parser shape mismatches (grammar lies, parser right)

### C1. `cond` mandatory `else`

- **Grammar:** lines 279-284 say `cond` is `{ cond_arm } end` with no else arm.
- **Parser:** `parse_cond_expr` ([control.rs:167-203](../crates/expo-parser/src/control.rs)) **requires** an `else -> ...` terminal arm.

**Action:** update grammar.ebnf to reflect parser truth
(`cond_expr = "cond" , { cond_arm } , "else" , "->" , match_body , "end"`).

### C2. Missing `move` modifier on `closure_param`

- **Grammar:** lines 238-241 — no `move`.
- **Parser:** accepts `move` for block closure params ([construct.rs:418-422](../crates/expo-parser/src/construct.rs)).

**Action:** add `[ "move" ]` to `closure_param` in grammar.ebnf.

### C3. `constant_decl` accepts TypeIdent as name

- **Grammar:** line 472 — `IDENT` only.
- **Parser:** [decl.rs:657-661](../crates/expo-parser/src/decl.rs) — accepts `Ident | TypeIdent`.

**Action:** tighten parser to match grammar (constants must be `IDENT`).

### C4. Pattern literals and multiline strings

- **Grammar:** `pattern → literal → multiline_string_lit` legal.
- **Parser:** `parse_literal_pattern` ([pattern.rs:74-95](../crates/expo-parser/src/pattern.rs)) handles `StringStart` only, not `MultilineStringStart`.

**Action:** trivial parser fix (a few lines) or disallow in grammar.
Probably fix the parser since the feature is cheap.

---

## D. LANGUAGE.md drift (docs lie about reality)

### D1. `Process` protocol surface (biggest documented lie)

- **LANGUAGE.md:** shows `fn new(config: C) -> Self`, `handle -> Self | StopReason`, default `run` dispatching `Pair<M, Option<ReplyTo<R>>>` (lines ~991-1042).
- **Reality:** stdlib has `fn start(move config: C) -> Result<Self, StopReason>`, `handle -> Step<Self>`, and `run` also dispatches `Lifecycle` ([process.expo:162-206](../lib/global/src/process.expo)). Typecheck requires `spawn Type.start(config)` form ([expr.rs:312-318](../crates/expo-typecheck/src/expr.rs)).

**Action:** rewrite the Concurrency section to match reality — `start`
not `new`, `Step<Self>` not union, mention `Lifecycle` and
`Step.Continue` / `Step.Done`.

### D2. `Process` example won't copy-paste

- The `spawn Counter.new(Counter{count: 0})` example (line ~1045) is wrong on two counts — `new` should be `start`, and the arg pattern doesn't match the signature.

**Action:** replace with a minimal copy-pasteable `Counter` example
matching today's Process protocol.

### D4. `receive ... after` underdocumented

- LANGUAGE.md lines 1133-1139 show only the mailbox arm.
- Parser supports `after timeout -> body` and codegen emits `expo_rt_receive_timeout`.

**Action:** add an `after` example.

### D5. `Ref<M, R>` missing `send_after`

- Stdlib exposes `send_after(self, msg, delay_ms)` at [process.expo:147-154](../lib/global/src/process.expo).
- LANGUAGE.md lists cast / call / signal / kill / alive? only.

**Action:** add `send_after` to the Ref API list.

### D7. `Debug` auto-derive for generics is degraded

- LANGUAGE.md ~1603 says Debug is auto-derived "for all types with rich formatting".
- Reality: generic types get a type-name-only `format` because `<A: Debug>` bounds aren't inferred ([synthesize.rs:13-23](../crates/expo-typecheck/src/synthesize.rs)).

**Action:** note the generics limitation in docs.

### D8. Struct destructuring assignment status

- LANGUAGE.md 1831-1836 says "parsed and/or type-checked" — typecheck is a no-op, codegen errors.

**Action:** demote to "designed; not parsed yet" pending
shorthand-in-struct-pattern work.

---

## E. ROADMAP.md drift

### E1. `Global.Mmap` — "DONE" but absent

- ROADMAP.md line 169 lists `Mmap` as shipped.
- `rg Mmap expo/lib` finds nothing. No Mmap type, no `mmap` intrinsic.

**Action:** remove the `Mmap` line or move it to "Remaining".

---

## F. Internal AST/AST-user consistency findings (minor)

### F1. `Annotation.value` has 2 variants but grammar allows 3

- **AST:** `AnnotationValue::String | False` ([ast.rs:69-75](../crates/expo-ast/src/ast.rs)).
- **Grammar:** line 442-444 includes `string_lit | multiline_string_lit | "false"`.
- `String` variant holds both single-line and multiline — parser collapses. OK in practice, but grammar suggests a distinction that doesn't exist.

**Action:** tweak grammar comment or leave — low priority.

---

## Recommended execution order

1. **Category A1 + A2** (remove `arena`, `shared`) — most cruft, clear decision, ~200 LOC deletion across parser/AST/grammar/LANGUAGE.md/lexer token list.
2. **Category C** (grammar.ebnf sync) — 5 minutes, grammar-only.
3. **Category D** (LANGUAGE.md drift) — rewrite Concurrency section, fix TOC, fix `receive` / `reply` / `send_after` / generics note.
4. **Category E** (ROADMAP.md sync) — tiny, remove Mmap claim, rewrite arena status.
5. **Category B / F** (AssignTarget tightening, short closure destructure, other minor cleanups) — pick off as bite-sized PRs.

Each step is independent and can land in its own commit/PR.
