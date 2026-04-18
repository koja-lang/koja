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

## Nested enum equality codegen

Comparing `Option<SomeEnum>` with `==` generates invalid LLVM IR (phi node
predecessors mismatch) when the inner enum has many variants.

**Workaround:** use `match` instead of `==` for `Option<Enum>` comparisons.

Surfaced during the self-hosted lexer port (`lex_newline` duplicate newline
check).

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

---

## Codegen `find_type` lookup misses user-defined generic types

Calling a method on a user-defined generic struct via an inherent generic
impl (e.g. `impl MyBox<T> { fn get(self) -> T self.value end }` followed by
`MyBox{value: 7}.get()`) fails at codegen with
`"no type info for \`MyBox\`"`. The dispatch path in
`expo-codegen/src/generics.rs` (`resolve_method_signature`) calls
`compiler.type_ctx.find_type(base_type)` with the bare name; `find_type`
goes through unscoped `resolve_name`, which only finds types in the global
`name_index`. User-defined types (in the project's package) only get
package-qualified entries, so the bare lookup fails -- but the *same*
function does a successful `resolve_name_current(base_type)` lookup a few
lines above, proving the type is in fact registered.

The same pattern works for stdlib types (`Option`, `List`, `Pair`, etc.)
because they happen to be in the global bare index.

**Workaround:** none for inherent generic impls today; works through
`impl Trait<T> for MyBox` (protocol dispatch hits a different path).

**Fix:** swap `type_ctx.find_type(base_type)` for the package-aware
equivalent (`resolve_name_current(base_type)` + `get_type(&id)`) in
`resolve_method_signature` and the matching sites in `structs.rs` /
`control/loops.rs`.

Surfaced while writing positive lock-in tests for the
fix-generic-impl-typecheck PR.

---

## Cached impl ASTs are pre-typecheck clones

`expo-typecheck/src/collect.rs` clones every `ImplBlock` into
`ctx.generic_impl_asts` and `ctx.specialized_impl_asts` *before*
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
