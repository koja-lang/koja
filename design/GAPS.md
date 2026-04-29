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

## Free function codegen gap

Free functions (outside `impl` blocks) pass `expo check` but crash at
codegen with `"assignment value produced no value"`. The type checker still
has old semantics from before the migration to `impl`-based functions.

**Fix:** either reject free functions at the type-check stage with a clear
error (`"functions must be declared inside impl blocks"`), or finish codegen
support.

Surfaced during the agent expression evaluator test.

**Why this is still around:** the long-term plan is to remove free
functions entirely in favour of a "single file mode" where the whole file
is implicitly the body of `main`. That mode requires being able to declare
`struct`/`enum` _inside_ a function body (so a script can define its own
local types without falling back to top-level items). Local types in
function bodies is currently deferred — the `(package, bare_name)` type
identity model used everywhere in `expo-typecheck` and `expo-codegen`
needs a `DefId`-style overhaul before nested decls can be represented
cleanly, and that's a multi-week refactor on top of an already-fragile
generic-impl pipeline (see the "Cached impl ASTs" entry below). Order of
operations when this becomes a priority: introduce `DefId`, then local
types fall out almost mechanically, then free functions can finally be
deleted.

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
