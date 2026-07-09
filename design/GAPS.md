# Known Compiler Gaps

Known limitations, bugs, and workarounds in the Koja compiler. New gaps
should be added here as they're discovered (agent testing, self-hosting,
etc.). For the full design of the iterator protocol replacement, see
[TYPES.md](TYPES.md).

---

## Generic enum unit variants in top-level code

`Option.None` cannot infer `T` without usage context in bare declarations.

**Workaround:** variable type annotations (`z: Option<Int32> = Option.None`).
Inside monomorphized method bodies and closures with return type annotations,
generic enum construction resolves all type parameters automatically.
Struct-literal field positions also propagate the declared field type down
into the initializer, so `Diagnostic{hint: Option.None}` resolves
`Option.None` from `hint: Option<String>` with no extra annotation.

Still affects generic free-function calls where one argument is a generic
unit variant: `Pair.new(self, Option.None)` in a function returning
`Pair<Lexer, Option<String>>` fails to infer `A` and `B` because the return
type isn't propagated into the call. Workaround: use struct literals
directly (`Pair{first: self, second: Option.None}`) where the field-type
hint pins the variant, or bind with a type annotation first.

Re-confirmed 2026-05-27 on both backends; diagnostic now reads
``typecheck cannot infer type parameter `T` of `Global.Option` from unit variant `None` ``.

---

## Iteration protocol limits (`Enumeration<T>`)

`Enumeration<T>` requires `length()` + `get(index)`, locking `for` to
index-based while loops. This precludes lazy iteration, streaming, and any
non-random-access collection (maps, linked lists, generators).

Pre-v1.0, replace with an `Iterator<T>` protocol using
`next(self) -> Option<Pair<T, Self>>`. `get` now returns `Option<T>`.
Codegen change is contained to `compile_for` in `loops.rs`; List/String
impls wrap existing index-based access in iterator state.

The current `for` loop hides the `Option` from the user (unwraps
automatically since iteration is bounds-checked). With lazy iteration,
`Option` becomes the termination mechanism -- `for` desugars to
`loop { match iter.next() ... }` and `None` breaks the loop.

Full design in [TYPES.md](TYPES.md) "Iterator protocol redesign" section.

---

## Nested types: lexical (in-body) declaration deferred

The qualified-name form is **implemented** — `struct Owner.Nested … end`
declared at top level, with construction, patterns, type-position
resolution, generics (`Owner.Nested<T>`), `extend`/`impl` on nested
targets, aliases, mangling, and `Debug` surface-name rendering all working
across both backends (design archived in
[archive/20260630-NESTED-TYPES.md](archive/20260630-NESTED-TYPES.md)).

What remains deferred is the **lexical** sugar: declaring a type inside the
owner's body rather than via a qualified top-level decl.

```koja
struct Supervisor
  enum Strategy
    OneForOne
    OneForAll
    RestForOne
  end
end
```

It resolves to the same member as `enum Supervisor.Strategy … end`; it is
purely a same-file convenience. The struct/enum body parser in
`koja-parser/src/decl.rs` only accepts fields and inline `fn` methods, so
nested type items would need to be allowed in that loop and hoisted to the
owner's namespace during collection.

**Why deferred:** the qualified-name form already covers every use site
(supervision coins its nested types that way); the lexical form is sugar,
not a 1.0 blocker.

---

## Constants are not referenceable across packages

Every `const` is same-package only in practice. Bare identifiers resolve
constants only in the current package, with no `Global` fallback, so even
`Global`'s `STDOUT`/`STDERR`/`STDIN` are unreachable outside `lib/global`.
`Pkg.CONST` parses as a unit enum construction and diagnoses "does not
recognize the enum type". `alias` rejects constant targets. As a result
`priv const` (2026-07) only affects the `@doc` rule today, though the
registry visibility gate is already wired for when a cross-package
reference path lands.

**Fix path (assessed 2026-07-09, roughly half a day):** everything below
typecheck resolve is already package-agnostic. IR reads constants by
registry id and both backends merge every package's constant pool into one
flat map. The work is a resolve-phase fallback: when the unit enum
interpretation of `Pkg.NAME` fails, look up `Identifier::new(pkg, [name])`
and on a constant hit rewrite the node to an `Ident` stamped
`Resolution::Global(id)`, the same rewrite trick `classify_receiver` uses
for static receivers, plus a `check_reference_visibility` call. A guard
must keep a real type named `Pkg` winning, mirroring
`try_package_function_call`. Optionally add a `Global` fallback for bare
constants in `resolve_ident` so stdlib constants become usable. Lowercase
constant names parse as field access instead of the enum shape, so support
only the uppercase path form.

---

## `and` / `or` do not short-circuit

Both operands of `and` and `or` are always evaluated. `lower_bin_op` in
`koja-ir/src/lower/ops.rs` maps them to strict `IRBinOp::And` / `Or`,
plain binary ops over two already-computed `Bool`s, identical on both
backends. LANGUAGE.md does not document the evaluation order either way.

This makes the universal guard idiom a runtime trap:

```koja
if i < self.size and self.byte(i) == BYTE_QUOTE   # panics past end of input
```

Every mainstream comparison point (Rust, Ruby, Elixir, Python, Go, Swift)
short-circuits, so users will write this, it compiles, and it faults at
runtime. Stdlib code works around it with nested `if` guards or
bounds-checked helpers (see `peek?` / `digit_at?` in
`lib/json/src/decoder.koja`, added 2026-07-09 for exactly this reason).

**Fix path (assessed 2026-07-09):** confined to IR lowering. Lower
`a and b` as control flow, the moral equivalent of `if a then b else
false` (mirror for `or`), so both backends inherit the semantics from the
lowered shape. The block-and-merge machinery already exists, pattern
match chains do the same thing via `ChainMode::And`. Parser, typecheck,
formatter, and precedence are untouched. LLVM folds the branch back into
branchless `and` when the right side is cheap and pure, so there is no
performance regression for the common case. The change is observable only
when the right operand panics or performs effects, which is the behavior
being fixed. Land with a LANGUAGE.md paragraph pinning left-to-right,
short-circuit evaluation and golden tests covering both backends.

---

## Arithmetic fault semantics undefined (ArithmeticError)

Investigated 2026-06-10. The fault behavior of `+ - * / %` and unary
`-` was never pinned down, and the two backends improvised
differently:

| Fault                             | Eval                                            | LLVM                                                                 |
| --------------------------------- | ----------------------------------------------- | -------------------------------------------------------------------- |
| Int `/` `%` by zero               | `RuntimeError::DivisionByZero`                  | raw `sdiv`/`srem` -- **UB** (SIGFPE on x86, silent garbage on ARM64) |
| `i64::MIN / -1`                   | overflow error via `checked_div`                | raw `sdiv` -- **UB**                                                 |
| Int overflow (`+ - *`, unary `-`) | `RuntimeError::IntegerOverflow` via `checked_*` | wraps (no `nsw`/`nuw`)                                               |
| Float ops producing `inf`/`NaN`   | allowed (IEEE)                                  | allowed (IEEE)                                                       |

The doc comment on `IRBinOp` in `koja-ir/src/types.rs` declares
two's-complement wrap the intended contract, with aligning eval "a
follow-up" -- that contract is now in question (see direction below).

The float row contradicts the finite-only stance adopted for
`Float.parse` (2026-06-10, `NumericConversionError`): `1.0 / 0.0`
quietly produces `inf` at runtime, `Debug.format` prints it as `inf`,
and `Float.parse` then refuses to read that text back. Finite text
in, non-finite values out -- a format/parse round-trip asymmetry.

**Proposed direction** -- an `ArithmeticError` trap, the Erlang
`badarith` analogue:

- Int div/mod by zero (and `i64::MIN / -1`) traps on both backends.
  This much is mandatory regardless of the rest: the LLVM behavior
  today is UB, not a semantic choice.
- A float op whose result is non-finite (`inf` from overflow or
  `x / 0.0`, `NaN` from `0.0 / 0.0` or `x % 0.0`) traps, restoring
  the finite-only invariant end to end. With that invariant, `NaN`
  can never reach comparisons, so the ordered-predicate semantics
  stay as-is.
- The trap mechanism must be a panic: binops return bare values, so
  there is no `Result` to thread an error through. Note panics
  currently abort the whole OS process (`__koja_panic` ->
  `process::abort()`); per-process isolation with
  `ExitReason.Crashed` supervision is its own unimplemented gap, and
  `ArithmeticError` inherits whatever that lands on.
- LLVM-side shape: a shared guard helper in `emit/ops.rs` -- compare
  (or `llvm.*.with.overflow`), `build_conditional_branch` to a panic
  block calling `__koja_panic` with a constant message, mirroring the
  `intrinsics/numeric.rs` range-check pattern.

**Open questions:**

1. Int overflow: trap (eval's current behavior, one overflow-flag
   branch per native int op) vs keep wrapping (the `types.rs`
   contract, align eval to wrap). Deliberately unresolved.
2. `Float.to_float32` documents "total -- out-of-range magnitudes
   become infinities", a backdoor minting non-finite `Float32`.
   Candidate fix: checked
   `Result<Float32, NumericConversionError.OutOfRange>` like the
   other narrowing methods.
3. Whether a float literal that overflows `f64` (`1e999` in source)
   should be a compile-time `OutOfRange` diagnostic to match the
   parse behavior.

---

## `koja shell` project mode

`koja shell` auto-loads the project in the working directory (its `src`,
path dependencies, and the stdlib prelude) so the REPL can call any
package function. Known limitations:

- **No explicit project selector.** The shell detects the project from
  the current directory only; there is no `-S <path>` flag yet to point
  it elsewhere (tracked in ROADMAP Phase 5 Track B).
- **Whole-program re-check per input.** Each prompt re-runs the entire
  baseline (stdlib + project + history) through the pipeline — the
  existing whole-program model, fine for small projects but linear in
  session length.
- **No FFI from the prompt.** Calling an `@extern "C"` function errors
  with `RuntimeError::Unsupported`; the interpreter has no FFI, same as
  `koja run --backend=interpreter`.
- **`Global` self-edit inconsistency.** `ProjectLoader` skips any stdlib
  package whose name matches the project (its `seen_packages` set), so a
  project named like a stdlib package — even `Global` — does not
  double-load. The one residual edge: running the REPL _inside_
  `koja/lib/global` loads the qualified stdlib packages (`Crypto`,
  `HTTP`, …, baked against the published `Global`) alongside the edited
  `Global`, since `ProjectLoader` does not replicate the
  `bundle_with_autoimport` rule that drops qualified sources on a
  `Global` self-compile. Only reachable when editing the stdlib itself.

---

## Bug triage log

Audited 2026-05-03 · re-triaged 2026-05-27 (seven fixed entries
removed: `Debug.format` tuple payloads, nested type-aliased unions,
bare closure expression statements, closures capturing `self`,
specialized self-nested impls, keyword-as-identifier silent drop, and
`<>` concat into a returned struct field corrupting under LLVM) ·
re-triaged 2026-06-07: the `List.append` / `Map.put` "borrow signature
but takes ownership" double-free was removed — the value-semantics + RC
migration dissolved it. `move` is gone, and a container now shares the
caller's reference-counted payload rather than aliasing a slot the
fn-exit drop frees, so there is no second free (the `text = "hello" <>
" world"; [text]` repro runs correctly on both backends) ·
re-triaged 2026-06-09: the "`match` arm binding a local inside a
closure body" seal ICE was fixed — `CaptureWalker` in
`lower/closures.rs` never registered pattern-introduced bindings
(match/receive arms, `for` loop patterns) or assignments nested in
`if`/`match` blocks as closure-own locals, so they were misclassified
as captures of the enclosing function. The walker now tracks
assignment targets as encountered and pushes a scope frame per arm /
loop pattern (regression coverage: `lower_closures.rs`,
`tests/lang/functions/closure_pattern_locals.kojs`).

# Audit: AST / grammar / LANGUAGE.md / ROADMAP.md / IR / codegen drift

**CLOSED 2026-06-09.** The full inventory of discrepancies between
`koja-ast`, `koja-parser`, `grammar.ebnf`, `LANGUAGE.md`,
`design/ROADMAP.md`, and downstream `koja-ir` is resolved. Surface,
grammar, and docs are 1-1 with what actually compiles. Resolution
summary:

- **B1 (`AssignTarget::Pattern`):** the `AssignTarget` enum was
  deleted — `Statement::Assignment.target` is now a bare `LValue`, the
  dead `try_expr_to_pattern` parser branch is gone (it was unreachable:
  `try_expr_to_lvalue` converts every `Ident` first), and grammar.ebnf
  dropped the `pattern , "=" , expr` alternative. The LANGUAGE.md
  Planned Features section was removed entirely — the doc describes
  only what the language actually does.
- **B2 (`ClosureParam::Destructured`):** deleted end to end — AST
  variant, block-closure parser arm (now a diagnostic with a hint),
  grammar alternatives in both `closure_param` and
  `closure_param_short`, and all downstream match arms (typecheck
  feature-gap diagnostic, IR lower panic, fmt, debug-print).
  Re-introduce if anonymous tuples ever enter the grammar properly.
- **Category C (grammar.ebnf vs parser):** C1 (`cond` mandatory `else`)
  and C4 (multiline-string patterns, now parsed with expression-equal
  dedent semantics plus an interpolation diagnostic) are fixed. C3 was
  resolved the other way: the grammar now documents that constant names
  accept `IDENT | TYPE_IDENT`, since SCREAMING_CASE constants
  (`MAX_SIZE`) lex as `TYPE_IDENT` and the codebase relies on it.
- **Category D (LANGUAGE.md drift):** D1/D2 (Process protocol +
  copy-pasteable Counter example), D4 (`receive ... after`), D5
  (`send_after` + `self_ref` on `Ref`), D7 (Debug derive: generics get
  full bodies now; opaque field types render `"..."`), and D8 (struct
  destructuring, later removed with the Planned Features section) are
  reconciled.
- **Category F:** F1 resolved with a grammar comment (single-line and
  multiline annotation strings collapse into one AST payload).
