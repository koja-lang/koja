# koja-typecheck

Sealed-AST typechecker built to the
[`COMPILER-NORTHSTAR.md`](../../design/COMPILER-NORTHSTAR.md) contract.

## Public surface

One entry point:

```rust
pub fn check_program(parsed: ParsedProgram) -> Result<CheckedProgram, CheckFailure>;
```

Success is always sealed. Concrete runtime types contain no unresolved or
type-parameter leaves. Generic templates may retain resolved `TypeParam`
leaves, but never `Unresolved`. Every registry declaration is fully stamped.
`seal_ast` runs last and panics on violation.

Type identity flows through `Expr.resolution`, a registry-pointing
`ResolvedType` (see `koja_ast::identifier`).

Failure arm carries diagnostics + the partial `ParsedProgram` for LSP / IDE
best-effort consumption.

## Sub-passes

```
preload          -> seed temporary Global primitive stubs
derive           -> append Debug and Equality impls before binding
collect          -> register declarations, then impl blocks
validate         -> check nested declarations and file aliases
lift_signatures  -> stamp resolved registry definitions
visibility       -> reject private types in public signatures
synthesize       -> rewrite typed surface shapes such as `for`
resolve          -> resolve and type-check every body
borrows          -> reject escaping CPtr.borrow results
diagnostic gate  -> return CheckFailure when errors exist
seal             -> assert AST and registry invariants
```

The order is forced by data dependencies, not preference. Each pass is a
single function (`pub(crate)`) called by `program::check_program`.

Script-mode files (top-level expressions, no surrounding `fn`) keep their
statements on `File.body`. There is no synthetic `fn main` wrapper:
`resolve` and `seal` walk `File.body` directly, and the IR layer's
`lower_script` consumes that shape. Project-mode files leave `File.body` as
`None`. Their work lives in `File.items[Function]`.

New sub-passes land only when their work is load-bearing. `strip_cfg` will run
before collect so excluded nodes never enter the registry. Compatibility
checks and coercion annotation stay folded into resolve unless a concrete
need justifies splitting them.

## Coverage today

Every shape that exercises a sub-pass: literals, arithmetic, comparisons,
unary ops, struct/enum decls, protocols, impls, generics, closures, pattern
match, and the seal-asserted control-flow forms. New features land as thin
slices on top of the existing sub-pass framework.
