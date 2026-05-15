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
preload         -> GlobalRegistry::with_stdlib_stubs seeds Global.Int/Bool/Unit/
                   Float/String as struct entries (temporary; real stdlib
                   compilation will supplant this)
collect         -> register top-level decls; assign Identifier
lift_signatures -> resolve TypeExpr params/return into ResolvedType and
                   stamp Function entries with their signature
resolve         -> walk all bodies AND any File.body (script mode);
                   populate Resolution + Expr.resolution (a
                   registry-pointing ResolvedType)
seal            -> assert seal_ast invariants over both items and
                   File.body; panic on violation
```

The order is forced by data dependencies, not preference. Each pass is a
single function (`pub(crate)`) called by `program::check_program`.

Script-mode files (top-level expressions, no surrounding `fn`) keep their
statements on `File.body`. There is no synthetic `fn main` wrapper —
`resolve` and `seal` walk `File.body` directly, and the IR layer's
`lower_script` consumes that shape. Project-mode files leave `File.body`
as `None`; their work lives in `File.items[Function]`.

Future sub-passes land in this orchestration when the work they do becomes
load-bearing — `strip_cfg` for `@cfg`-driven pruning, `synthesize` between
`collect` and `lift_signatures` for protocol defaults, `check` between
`resolve` and `seal` for compatibility validation beyond what `resolve`
enforces inline, and `annotate` between `check` and `seal` for coercion
emission. They aren't in the pipeline yet because there's nothing for
them to do — no-op pass-throughs would be dead architecture.

## What alpha covers today

`fn main; 2 + 2; end` (project mode) and bare `2 + 2` (script mode) —
literally the smallest programs that exercise every sub-pass at least
vacuously and produce a sealed `CheckedProgram`. Each new feature
(structs, enums, generics, ...) lands as a thin slice on top of the
existing sub-pass framework.
