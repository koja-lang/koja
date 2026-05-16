# expo-ir

Lowering phase built to the
[`COMPILER-NORTHSTAR.md`](../../design/COMPILER-NORTHSTAR.md) contract.

## Public surface

One entry point:

```rust
pub fn lower_program(
    checked: &CheckedProgram,
    entry: Identifier,
) -> Result<IRProgram, LowerError>;
```

`checked` is a sealed [`CheckedProgram`] from `expo-typecheck`. `entry` is the
fully-qualified identifier of the function to mark as the program entry point
(typically `Identifier::new(package, vec!["main".into()])`).

Success arm is **always sealed** — every block ends in a terminator, every
value reference points at a previously-defined value in the same function,
every function in `prog.functions` is reachable through one of
`prog.packages`, and `prog.entry_point` resolves to a registered function.
The `seal_program` invariant check runs as the last sub-pass of
`lower_program` and panics on violation; seal failures are compiler bugs,
not recoverable conditions.

Failure arm carries a [`LowerError`] for user-actionable problems. Two
disjoint variants today:

- `LowerError::Diagnostics(Vec<Diagnostic>)` — one or more feature-gap
  diagnostics accumulated while walking the sealed AST (unsupported
  expression kinds, Float literals, assignment statements, extern fn
  bodies, `self` receivers, and so on). Lowering is per-function fail-fast:
  a failed function contributes one diagnostic and is dropped from the
  package; `lower_program` short-circuits to this variant before
  `seal_program` ever runs, so seal never sees a partial IR. Compiler-bug
  cases (e.g. a call callee with `Unresolved` resolution after the
  typecheck seal) stay loud panics.
- `LowerError::EntryPointNotFound { identifier }` — the caller-supplied
  entry point is not present in the lowered program. Only surfaced when
  lowering produced zero diagnostics.

## Sub-passes

```
lower-package -> per-package translation: sealed AST  ->  IRPackage
                 (pushes feature-gap diagnostics into a shared buffer;
                 dropped fns are simply omitted from the fragment)
diagnostics?  -> short-circuit to LowerError::Diagnostics if non-empty
merge         -> stitch IRPackage fragments into a working IRProgram
entry-check   -> surface LowerError::EntryPointNotFound on miss
seal          -> assert seal_program invariants; panic on violation
```

The order is forced by data dependencies, not preference. Each pass is a
single function (`pub(crate)`) called by `program::lower_program`.

Future sub-passes (e.g. `closure` for generic-instantiation discovery,
`elaborate` for coercion emission) land between `merge` and `seal` when the
work they do becomes load-bearing. They aren't in the pipeline yet because
there's nothing for them to do — no-op pass-throughs would be dead
architecture.

## IR vocabulary

The IR is intentionally narrow — every `IRInstruction` variant pairs with
explicit lowering rules and a 1:1 emit step in `expo-ir-llvm` /
`expo-ir-eval`. New instructions land as new features create pressure
(function calls, struct construction, pattern matching, and so on).

## Hard contract

- **No `IRInstruction::Stub` variant.** Lowering helpers panic on lookup
  misses; misses are seal violations upstream, never recoverable conditions.
- **No `Coercion` metadata in the sealed `IRProgram`.** Coercions become
  explicit `IRInstruction`s during lowering when that machinery lands.
