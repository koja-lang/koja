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
strip-cfg     -> [v2 POC: stub; @cfg pruning lands later]
collect       -> register surviving top-level decls; assign Identifier
synthesize    -> [v2 POC: stub; protocol-default synthesis lands later]
resolve       -> walk all bodies; populate Resolution + Expr.resolved_type
check         -> validate type compatibility (today: primitive arithmetic only)
annotate      -> [v2 POC: stub; coercion annotation lands later]
seal          -> assert seal_ast invariants; panic on violation
```

The order is forced by data dependencies, not preference. Each pass is a
single function (`pub(crate)`) called by `program::check_program`.

## What v2 covers today

`fn main; 2 + 2; end` — literally the smallest program that exercises every
sub-pass at least vacuously and produces a sealed `CheckedProgram`. Each
new feature (structs, enums, generics, ...) lands as a thin slice on top
of the existing sub-pass framework.
