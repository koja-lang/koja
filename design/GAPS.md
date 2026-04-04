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
