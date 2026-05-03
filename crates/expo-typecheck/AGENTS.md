# expo-typecheck

Two-pass type checker: collect (build TypeContext) then check (verify types).

## Key files

- `lib.rs` -- Public API: `check()`, `check_file()`, `collect_all_names()`, `TypeContext`
- `collect.rs` -- Pass 1: walks AST to register structs, enums, functions, protocols, constants. Builds `TypeContext`. Largest file (~1672 lines)
- `check.rs` -- Pass 2: type-checks function bodies, impl blocks, protocol conformance
- `context.rs` -- `TypeContext` struct: functions, types, closures, coercions, diagnostics
- `expr.rs` -- Expression inference and checking (calls, closures, methods, generics). Largest checking file (~1837 lines)
- `stmt.rs` -- Statement checking (assignments, moves, returns, breaks)
- `pattern.rs` -- Pattern checking and match exhaustiveness
- `synthesize.rs` -- Pre-collect AST rewrites (today: auto-derive `Debug` impls). Runs at the top of `collect_file`. See `design/COMPILER-NORTHSTAR.md` for the full sub-pass plan.
- `types.rs` -- Type helpers: `resolve_type_expr`, alias resolution, substitution, unification
- `cycle.rs` -- Recursive struct/enum detection, marks `Type::Indirect`
- `env.rs` -- Per-function `CheckEnv`, variable state tracking for ownership
- `resolve.rs` -- Resolves `Package::Unresolved` on types after cross-file merge

## Vocabulary

A _package_ is a unit of distribution (your app, the stdlib, a dependency). A
_file_ is a single `.expo` source file. Multiple files come together to form a
package; all top-level types in a package can be referenced from any file in
that package without imports. The Expo language has no "module" concept --
when you see `module` in code below this point it is the Rust language item
(`mod foo;`).

## Pipeline

```
collect_all_names (global struct/enum names)
  -> collect_file (synthesize -> build TypeContext per file)
    -> merge (combine all files + stdlib)
      -> synthesize_protocol_defaults
        -> mark_recursive_fields
          -> resolve_imports
            -> check_file (type-check bodies)
```
