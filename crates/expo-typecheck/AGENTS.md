# expo-typecheck

Sealed-AST typechecker built to the
[`COMPILER-NORTHSTAR.md`](../../design/COMPILER-NORTHSTAR.md) contract.

## Public surface

One entry point:

```rust
pub fn check_program(parsed: ParsedProgram) -> Result<CheckedProgram, CheckFailure>;
```

Success arm is **always sealed** — every relevant `Expr.resolution` fully
populated into the registry, every `Resolution` either
`Global(GlobalRegistryId)` or `Unresolved` (only for nodes the seal contract
excludes). The `seal_ast` invariant check runs as the last sub-pass of
`check_program` and panics on violation; seal failures are compiler bugs, not
recoverable conditions.

Type identity flows through `Expr.resolution`, a registry-pointing
`ResolvedType` (see `expo_ast::identifier`).

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
`lower_script` consumes that shape. Project-mode files leave `File.body` as
`None`; their work lives in `File.items[Function]`.

Future sub-passes land in this orchestration when the work they do becomes
load-bearing — `strip_cfg` for `@cfg`-driven pruning, `synthesize` between
`collect` and `lift_signatures` for protocol defaults, `check` between
`resolve` and `seal` for compatibility validation beyond what `resolve`
enforces inline, and `annotate` between `check` and `seal` for coercion
emission. They aren't in the pipeline yet because there's nothing for them
to do — no-op pass-throughs would be dead architecture.

## Coverage today

Every shape that exercises a sub-pass: literals, arithmetic, comparisons,
unary ops, struct/enum decls, protocols, impls, generics, closures, pattern
match, and the seal-asserted control-flow forms. New features land as thin
slices on top of the existing sub-pass framework.
