# expo-ir-v2

Lowering phase built to the [`COMPILER-NORTHSTAR.md`](../../design/COMPILER-NORTHSTAR.md)
contract. Sibling to the legacy `expo-ir`; the two share **no code** and **no types** —
this crate defines its entire IR vocabulary from scratch.

## Public surface

One entry point:

```rust
pub fn lower_program(
    checked: &CheckedProgram,
    entry: Identifier,
) -> Result<IRProgram, LowerError>;
```

`checked` is a sealed [`CheckedProgram`] from `expo-typecheck-v2`. `entry` is the
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
closure       -> [v2 POC: stub; generic instantiation discovery lands later]
elaborate     -> [v2 POC: stub; reserved for later refinements]
seal          -> assert seal_program invariants; panic on violation
```

The order is forced by data dependencies, not preference. Each pass is a single
function (`pub(crate)`) called by `program::lower_program`.

## What v2 covers today

`fn main; 2 + 2; end` — the smallest program that exercises every sub-pass at
least vacuously and produces a sealed `IRProgram` consumable by
`expo-ir-eval`. The IR vocabulary is intentionally narrow: `Const`, `BinaryOp`,
and `Return` cover everything `2 + 2` requires. New instructions land as new
features create pressure (function calls, struct construction, pattern matching,
and so on).

## Hard contract

- **Zero dependency on `expo-ir`.** That crate is the legacy v1 codegen path;
  v2 is a clean cut. Do not add it as a dep, do not import a single type, do
  not even glance at it for inspiration without first asking whether the v2
  shape should differ.
- **No `IRInstruction::Stub` variant.** Lowering helpers panic on lookup
  misses; misses are seal violations upstream, never recoverable conditions.
- **No `Coercion` metadata in the sealed `IRProgram`.** Coercions become
  explicit `IRInstruction`s during lowering when that machinery lands.
