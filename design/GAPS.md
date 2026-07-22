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
`next(self) -> Option<(T, Self)>`. `get` now returns `Option<T>`.
Codegen change is contained to `compile_for` in `loops.rs`; List/String
impls wrap existing index-based access in iterator state.

The current `for` loop hides the `Option` from the user (unwraps
automatically since iteration is bounds-checked). With lazy iteration,
`Option` becomes the termination mechanism -- `for` desugars to
`loop { match iter.next() ... }` and `None` breaks the loop.

Full design in [TYPES.md](TYPES.md) "Iterator protocol redesign" section.

---

## Tuple equality skips closure and union elements

Tuple equality lowers element-wise calls only for types with a usable
`Equality` implementation. Closure and union elements are currently treated
as opaque and skipped, so tuples that differ only in those positions compare
equal. The same hole applies when an opaque element appears inside a nested
tuple.

This does not satisfy the language contract that tuples support equality only
when every element does. Until closures and unions gain defined equality
semantics, typecheck should reject equality on tuple shapes containing either
type instead of allowing lowering to omit them.

---

## Explicit `return` values are not type-checked

Typecheck only validates a function's trailing expression against the
declared return type (`resolve/return_type.rs`). An explicit
`return <value>` resolves its expression with the declared return type as
an inference hint, but never runs `check_compatible_stamping` on the
result. Two consequences:

- `return "x"` inside `fn f() -> Int` passes typecheck and produces
  ill-typed IR (the LLVM backend rejects it late; the interpreter
  errors at runtime).
- `return Cat{}` inside `fn f() -> Cat | Dog` never gets its
  `Coercion::UnionWiden` stamped, so no `UnionWrap` is emitted and
  union-typed early returns miscompile.

Script bodies have no declared return type at all, so their explicit
returns are entirely unconstrained; script return typing (`Unit` when
flow closes via `return`) and REPL echo semantics are open design
questions tied to this gap. Relatedly, the compiled script path only
supports a single reachable `Return`-terminated block
(`main_wrapper.rs::find_return_block`), so a script with an early
`return` runs on the interpreter but fails `koja run --backend llvm`
with a codegen error.

The fix belongs in typecheck: check and coercion-stamp every
`Statement::Return` value the same way trailing expressions are handled
today. Until then, `seal/types.rs` deliberately exempts
`IRTerminator::Return` from the typed-IR seal, since lowering cannot
guarantee an invariant typecheck doesn't establish.

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

## Inference and ergonomics warts from the pooler build

Found 2026-07-15 while building the `pooler` package (a generic
`Process` implementation, the first real one outside the stdlib). The
blocking bug is fixed. `spawn` on a generic process target was not
substituting the call site's type args into the conformance's `M`/`R`,
and the monomorphizer skipped `LValue.head_resolved_type` on field
assignments (regression coverage in
`tests/lang/generics/generic_process_spawn.kojs`). Also fixed
2026-07-16: `priv fn` helpers inside `impl Protocol for Type` blocks
were rejected despite LANGUAGE.md allowing them; the conformance check
now skips private members and only rejects public extras (regression
coverage in `tests/lang/protocols/priv_impl_helper.kojs`). Three
non-blocking warts remain, each with a workaround.

- **Generic enum unit variants don't infer from parameter types.**
  `consume(Signal.Done)` fails with "cannot infer type parameter `T`
  from unit variant `Done`" even though `consume`'s parameter is
  `Signal<T>` and `T` is bound in the enclosing scope. Payload
  variants at the same call site infer fine. Workaround is binding
  with an annotation first (`done: Signal<T> = Signal.Done`).
- **`x = match … end` doesn't cross-infer generic payloads.** Arms
  building `Result.Ok(true)` / `Result.Err("nope")` each fail to
  infer the sibling's type parameter when the match is assigned to a
  local, while the same match as a trailing expression (with the
  function return type as the expected hint) compiles. The arms could
  unify against each other. Workaround is restructuring so the match
  is in return position, or annotating the binding.
- **Nested enum patterns defeat exhaustiveness.** Splitting
  `Result.Err` by payload (`Result.Err(CallError.Timeout)` +
  `Result.Err(CallError.ProcessDown)`) reports "missing variant
  `Err`" because the checker doesn't combine nested coverage into
  coverage of the outer variant. Workaround is a `Result.Err(_)`
  catch-all arm with an inner match on the payload.

---

## Runtime: adjacent issues from the worker-migration TLS audit

Found while root-causing the 2026-07 Linux shutdown crash (a process
resuming on a different worker thread after socket I/O switched through
the old worker's cached TLS base; fixed with `#[inline(never)]`
barriers, see the note in `koja-runtime-posix/src/scheduler.rs`). One
neighbor remains open:

- **Reduction counter writes can land on the wrong worker.** Compiled
  process code decrements the C thread-local `koja_reductions_left`
  inline, and LLVM may cache its address across a suspension point, so
  a migrated process keeps decrementing the previous worker's counter
  until the next runtime call. Consequence is mistimed yield checks
  (never memory unsafety). A fix needs codegen to recompute the TLS
  address after every call that can suspend, or a non-TLS budget.

---

## Runtime: RSS grows linearly under server load

Found benchmarking the concurrent shortener rewrite (2026-07,
process-per-connection workers + keep-alive, release build). RSS grows
~127 MB per 10,000 requests, linear across repeated identical loads,
with and without keep-alive, on both DB-backed and 404 paths. No
crashes, drops, or latency degradation — but a long-running server
would exhaust memory. Something on the per-request path is not
reclaimed: candidate suspects are dead one-shot worker processes not
being fully reaped, per-request heap values surviving the worker's
recursive `serve` loop (tail-call frames keeping buffers alive), or
mailbox/envelope allocations. Needs a runtime heap audit with a
minimal spawn-per-message reproducer.

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
  Anonymous tuples now exist, but closure parameters remain name-only.
  Destructure a tuple in the closure body when needed.
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
