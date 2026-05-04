# expo-alpha-typecheck

Sealed-AST typechecker built to the [`COMPILER-NORTHSTAR.md`](../../design/COMPILER-NORTHSTAR.md)
contract. Alpha-track sibling to the legacy `expo-typecheck`; the two share no
code and no types — alpha is a clean cut, not an evolution.

## Public surface

One entry point:

```rust
pub fn check_program(parsed: ParsedProgram) -> Result<CheckedProgram, CheckFailure>;
```

Success arm is **always sealed** — every relevant `Expr.resolution` fully populated
into the registry, every `Resolution` either `Global(GlobalRegistryId)` or
`Unresolved` (only for nodes the seal contract excludes). The `seal_ast` invariant
check runs as the last sub-pass of `check_program` and panics on violation; seal
failures are compiler bugs, not recoverable conditions.

Alpha does **not** populate the legacy `Expr.resolved_type` field — that slot is v1's
annotation, preserved on the shared `Expr` struct during the v1 → alpha migration and
ignored by alpha. Type identity in alpha flows through `Expr.resolution`, a
registry-pointing `ResolvedType` (see `expo_ast::identifier`).

Failure arm carries diagnostics + the partial `ParsedProgram` for LSP / IDE
best-effort consumption.

## Sub-passes

```
preload       -> GlobalRegistry::with_stdlib_stubs seeds Global.Int/Bool/Unit/
                 Float/String as struct entries (temporary; real stdlib
                 compilation will supplant this)
lift_script   -> hoist File.body into synthesized fn main (script mode only)
collect       -> register top-level decls; assign Identifier
resolve       -> walk all bodies; populate Resolution + Expr.resolution
                 (a registry-pointing ResolvedType)
seal          -> assert seal_ast invariants; panic on violation
```

The order is forced by data dependencies, not preference. Each pass is a
single function (`pub(crate)`) called by `program::check_program`.

`lift_script` is intentionally narrow and self-contained — the synthetic
`fn main` wrap is a transient bridge while the parser learns to express
script-mode semantics natively. When the wrap is replaced, this single
pass disappears.

Future sub-passes land in this orchestration when the work they do becomes
load-bearing — `strip_cfg` between `lift_script` and `collect` for
`@cfg`-driven pruning, `synthesize` after `collect` for protocol defaults,
`lift_signatures` between `synthesize` and `resolve` for cross-decl
signature resolution, `check` between `resolve` and `seal` for compatibility
validation beyond what `resolve` enforces inline, and `annotate` between
`check` and `seal` for coercion emission. They're not in the pipeline yet
because the POC has nothing for them to do — no-op pass-throughs would be
dead architecture.

## What alpha covers today

`fn main; 2 + 2; end` — literally the smallest program that exercises every
sub-pass at least vacuously and produces a sealed `CheckedProgram`. Each
new feature (structs, enums, generics, ...) lands as a thin slice on top
of the existing sub-pass framework.
