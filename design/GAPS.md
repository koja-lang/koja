# Known Compiler Gaps

Known limitations, bugs, and workarounds in the Koja compiler. New gaps
should be added here as they're discovered (agent testing, self-hosting,
etc.). For the full design of the iterator protocol replacement, see
[TYPES.md](TYPES.md).

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

## No wrapping-arithmetic escape hatch

Integer arithmetic always traps on overflow (2026-07). There are no
`wrapping_add` / `wrapping_mul` style operations, and the Erlang idiom
of masking after the math does not transfer, since the operation traps
before a `band` can run. Consequence: a 64-bit wrapping multiply is
inexpressible in pure Koja, which locks out most non-cryptographic hash
functions (FNV, xxHash, SplitMix). 32-bit wrapping can be simulated by
computing in `Int` and masking.

**Fix path:** a named-operation family on the integer types, following
the `Bitwise` precedent (the specialized algebra gets words, not
symbols). Both backends already thread the operand type through
`BinaryOp`, so codegen is the existing arithmetic minus the overflow
guard.

---

## `CPtr` float reads bypass the finite-only invariant

A non-finite float returned by an `@extern "C"` call traps at the call
site, but `CPtr<Float64>.read()` (and `Float32`) does not. A NaN or
infinity sitting in a C-filled buffer walks straight into a `Float`,
silently breaking the invariant. This is consistent with the FFI
stance that safety is the wrapper author's responsibility, but it is
currently undocumented rather than decided.

**Fix path options:** guard `read` (costs a check on the bulk-transfer
path), document `CPtr` as an unchecked boundary, or offer both a
checked and an unchecked read. A carrier type for full IEEE values
(the `Binary`-to-`String` pattern applied to floats) would subsume
this: pointer reads and extern returns typed as IEEE floats make no
finiteness promise, and the checked crossing moves to an explicit
conversion.

---

## No exponent notation in numeric literals

The lexer does not accept `1e9` / `1.5e-3`. Large or small float
literals must be written with every digit (the arithmetic-fault test
fixtures write 160-digit literals). `Float.parse("1e999")` handles the
notation at runtime, so the gap is literal syntax only.

**Fix path:** lexer support for an optional exponent suffix on float
literals, plus the same round-to-infinity `OutOfRange` check float
literals already get.

---

## `Result<(), E>` fails LLVM codegen

Found 2026-07-12 (postgres driver). A function whose return type
instantiates a generic enum with `()` (e.g.
`fn close(self) -> Result<(), Error>` returning `Result.Ok(())`) fails
LLVM codegen with `expected a value-level IRType, got Unit`
(`types.rs`). `()` works as a `Process` type parameter
(`Process<(), (), ()>` is the scaffold default), so the gap is specific
to unit flowing through a value-level slot such as an enum payload.
Workaround: return `Result<Bool, E>`.

---

## No definite-assignment analysis for locals

Found 2026-07-12 while fixing the duplicate-`LocalDecl` seal panic.
Locals are function-scoped and nothing verifies that every path to a
read actually assigned the local first. A read after a
conditionally-executed first assignment compiles and reads an
uninitialized slot (verified on 0.14.0):

```koja
i = 5
while i < 3   # never executes
  n = i * 2
end
n.print()     # uninitialized read. Garbage Int, or a crash for
              # heap-managed types.
```

The IR side already defends itself. `merge_slot_states` keeps a slot
in the live set only when every branch assigned it, so no exit-drop is
emitted for a maybe-uninitialized slot. The surface language should
match with a proper diagnostic. The likely fix is a
definite-assignment dataflow pass in typecheck (a read is an error
unless every path from function entry assigns first, with `if`/`else`
where both arms assign counting as assigned and loop bodies counting
as maybe-assigned). Needs LANGUAGE.md wording and will flag existing
code that relies on a loop always executing at least once.

---

## `koja format` comment edge cases

Two known comment-placement gaps remain in the formatter (found
2026-07-12 during the comment-relocation fix; both are fiddly and low
value):

- A trailing comment on a declaration header line
  (`fn foo(x: Int) # note`) is claimed by the first body statement's
  leading drain and moves into the body. Signatures can wrap across
  lines, so the header's true end line isn't tracked.
- A leading comment above a field inside a multi-line enum struct
  variant leaks to before the next variant (`enum_variant_to_doc`
  doesn't drain per inner field).

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
