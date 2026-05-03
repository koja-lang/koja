# expo-typecheck-v2

Sealed-AST typechecker built to the [`COMPILER-NORTHSTAR.md`](../../design/COMPILER-NORTHSTAR.md)
contract. Sibling to the legacy `expo-typecheck`; the two share no code and no types.

## Public surface

One entry point:

```rust
pub fn check_program(parsed: ParsedProgram) -> Result<CheckedProgram, CheckFailure>;
```

Success arm is **always sealed** — every relevant `Expr.resolved_type` populated, every
`Resolution` either `Global(Identifier)` or `Unresolved` (only for nodes the seal
contract excludes). The `seal_ast` invariant check runs as the last sub-pass of
`check_program` and panics on violation; seal failures are compiler bugs, not
recoverable conditions.

Failure arm carries diagnostics + the partial `ParsedProgram` for LSP / IDE
best-effort consumption.

## Sub-passes

```
collect       -> register top-level decls; assign Identifier
resolve       -> walk all bodies; populate Resolution + Expr.resolved_type
seal          -> assert seal_ast invariants; panic on violation
```

The order is forced by data dependencies, not preference. Each pass is a
single function (`pub(crate)`) called by `program::check_program`.

Future sub-passes land in this orchestration when the work they do becomes
load-bearing — `strip_cfg` between parse and `collect` for `@cfg`-driven
pruning, `synthesize` after `collect` for protocol defaults,
`lift_signatures` between `synthesize` and `resolve` for cross-decl
signature resolution, `check` between `resolve` and `seal` for compatibility
validation beyond what `resolve` enforces inline, and `annotate` between
`check` and `seal` for coercion emission. They're not in the pipeline yet
because the POC has nothing for them to do — no-op pass-throughs would be
dead architecture.

## What v2 covers today

`fn main; 2 + 2; end` — literally the smallest program that exercises every
sub-pass at least vacuously and produces a sealed `CheckedProgram`. Each
new feature (structs, enums, generics, ...) lands as a thin slice on top
of the existing sub-pass framework.
