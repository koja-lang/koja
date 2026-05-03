# expo-alpha-ir

Lowering phase built to the [`COMPILER-NORTHSTAR.md`](../../design/COMPILER-NORTHSTAR.md)
contract. Alpha-track sibling to the legacy `expo-ir`; the two share **no code**
and **no types** — alpha is a clean cut and defines its entire IR vocabulary
from scratch.

## Public surface

One entry point:

```rust
pub fn lower_program(
    checked: &CheckedProgram,
    entry: Identifier,
) -> Result<IRProgram, LowerError>;
```

`checked` is a sealed [`CheckedProgram`] from `expo-alpha-typecheck`. `entry` is the
fully-qualified identifier of the function to mark as the program entry point
(typically `Identifier::new(package, vec!["main".into()])`).

Success arm is **always sealed** — every block ends in a terminator, every value
reference points at a previously-defined value in the same function, every
function in `prog.functions` is reachable through one of `prog.packages`, and
`prog.entry_point` resolves to a registered function. The `seal_program`
invariant check runs as the last sub-pass of `lower_program` and panics on
violation; seal failures are compiler bugs, not recoverable conditions.

Failure arm carries a [`LowerError`] for user-actionable problems (today: the
caller-supplied entry-point is not present in the lowered program).

## Sub-passes

```
lower-package -> per-package translation: sealed AST  ->  IRPackage
merge         -> stitch IRPackage fragments into a working IRProgram
seal          -> assert seal_program invariants; panic on violation
```

The order is forced by data dependencies, not preference. Each pass is a single
function (`pub(crate)`) called by `program::lower_program`.

Future sub-passes (e.g. `closure` for generic-instantiation discovery,
`elaborate` for coercion emission) land between `merge` and `seal` when the
work they do becomes load-bearing. They're not in the pipeline yet because the
POC has nothing for them to do — no-op pass-throughs would be dead
architecture.

## What alpha covers today

`fn main; 2 + 2; end` — the smallest program that exercises every sub-pass at
least vacuously and produces a sealed `IRProgram` consumable by
`expo-alpha-ir-eval`. The IR vocabulary is intentionally narrow: `Const`,
`BinaryOp`, and `Return` cover everything `2 + 2` requires. New instructions
land as new features create pressure (function calls, struct construction,
pattern matching, and so on).

## Hard contract

- **Zero dependency on `expo-ir`.** That crate is the legacy v1 codegen path;
  alpha is a clean cut. Do not add it as a dep, do not import a single type,
  do not even glance at it for inspiration without first asking whether the
  alpha shape should differ.
- **No `IRInstruction::Stub` variant.** Lowering helpers panic on lookup
  misses; misses are seal violations upstream, never recoverable conditions.
- **No `Coercion` metadata in the sealed `IRProgram`.** Coercions become
  explicit `IRInstruction`s during lowering when that machinery lands.
