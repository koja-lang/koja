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

## Nested enum pattern matching with literal payloads

Matching a nested variant with a literal payload (e.g.,
`Some(TokenKind.Ident("and"))`) causes a segfault at runtime.

**Workaround:** bind the payload and check it in the body:
`Some(TokenKind.Ident(name)) -> name == "and"`.

Surfaced during the self-hosted lexer port (`continues_line?`).

---

## `match` inside `while`/`loop` with `return`

When a `match` expression appears inside a `while` or `loop` body and any
arm contains a `return` statement, the generated binary segfaults on startup
(before `main` runs). The crash is in LLVM codegen -- likely incorrect basic
block wiring for the match's phi nodes when nested inside a loop's
back-edge structure.

**Workaround:** use recursion instead of loops with `match`. Since Expo is
FP-oriented, recursive helpers with `move` parameters are idiomatic and
avoid the bug entirely.

Surfaced during the `json` package decoder (recursive descent parser for
arrays and objects).

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
generic-impl pipeline (see the four "user-defined generic types" entries
below and the "Cached impl ASTs" entry). Order of operations when this
becomes a priority: chip away at the inherent-generic-impl gaps first,
then introduce `DefId`, then local types fall out almost mechanically,
then free functions can finally be deleted.

---

## Static-method type inference on user-defined generic types

Calling a generic static method on a user-defined generic struct/enum fails
type inference (e.g. `MyBox.new(42)` for `impl MyBox<T> { fn new(v: T) -> MyBox<T> ... }`):

```
error: cannot infer type parameter `T` for `MyBox.new`
```

The typechecker does not propagate argument types into static-method calls
on user generic types, even though it does the equivalent inference for
free generic functions (`fn make_pair<A, B>(a: A, b: B) -> Pair<A, B>`).
The same machinery presumably exists; it just isn't wired up for the
`Type.method(...)` static-call path on user types.

**Workaround:** construct via struct literal (`MyBox{value: 42}`) where
field types pin the type parameters, or annotate the binding's type and
let the inner construction propagate (`b: MyBox<Int> = MyBox.new(42)` --
untested but expected to work via expected-type propagation).

The same pattern works for stdlib types because they go through
specialized inference paths (or have explicit type-arg syntax in the
collection `new` helpers).

Surfaced while writing lock-in tests for the codegen `find_type_current`
fix (April 2026).

---

## Generic methods on generic impls cannot infer their own type parameters

A generic method _inside_ a generic impl (e.g.
`impl MyBox<T> { fn map_to_pair<U>(self, other: U) -> Pair<T, U> ... }`)
fails at codegen with a mangled-name including `unknown`:

```
error: no LLVM type for method parameter type `Unknown` in
       `inherent_generic_impl.MyBox_$Int$_map_to_pair_$unknown$`
```

The outer type parameters (`T`) are resolved by the receiver type, but the
method-local parameter (`U`) never gets bound from the call site's
argument types. Codegen receives `Unknown` for `U` and the method-mangled
symbol carries `$unknown$` straight through to LLVM type construction.

**Workaround:** lift the helper to a free function or split into a
non-generic method that takes a pre-built generic value.

Surfaced while writing lock-in tests for the codegen `find_type_current`
fix (April 2026).

---

## Pattern matching on user-defined generic enums

Matching a value of a monomorphized user-defined generic enum fails to
re-resolve the bare enum name from the mangled type. With:

```
enum MyEither<A, B>
  Left(A)
  Right(B)
end

l: MyEither<Int, String> = MyEither.Left(7)
match l
  MyEither.Left(n) -> ...
  MyEither.Right(s) -> ...
end
```

codegen reports:

```
error: cannot resolve enum name from pattern `MyEither` for match subject
       type `<pkg>.MyEither_$Int.String$`
```

The same pattern works for stdlib generic enums (`Option`, `Result`)
because their bare names are in the global `name_index`. The pattern
resolver has a path that strips monomorphization suffixes for stdlib
types but doesn't account for user-package qualification on top of the
mangled name.

**Workaround:** none today for user-defined generic enums. Use stdlib
`Option`/`Result` if their shape fits, or wrap your sum-type in a
non-generic enum.

Surfaced while writing lock-in tests for the codegen `find_type_current`
fix (April 2026).

---

## Struct construction inside generic impl method bodies

Constructing a generic struct _inside_ a method on its own generic impl
fails with the bare-name lookup, distinct from the (now-fixed) `find_type`
path:

```
impl MyBox<T>
  fn replace(self, new_value: T) -> MyBox<T>
    MyBox{value: new_value}   # error: unknown struct type: MyBox
  end
end
```

The struct-construction path in `expo-codegen/src/structs.rs` resolves the
type identifier via `Compiler::resolve_name_current` correctly, but then
calls `compiler.types.get_concrete(&lookup_id)`, which has no entry for
the monomorphized `MyBox<Int>` registered under the user package.

**Workaround:** construct the struct _outside_ the method body and pass
it in, or reach for a free function.

**Fix sketch:** ensure the monomorphization driver registers concrete
types under the package-qualified `TypeIdentifier`, or have
`get_concrete` fall back to the bare-name index the way
`find_type_current` now does.

Surfaced while writing lock-in tests for the codegen `find_type_current`
fix (April 2026).

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

Surfaced during Stage 5 of the fix-generic-impl-typecheck plan; that stage
is paused until this is sorted.

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
weeks for generics (which would also benefit from closing the four
"user-defined generic types" gaps above first to avoid debugging on two
axes at once).

**Why deferred:** much cheaper than local-types-in-function-bodies but
still a sizeable feature; not a 1.0 blocker. Tracked here so the design
analysis isn't lost.
