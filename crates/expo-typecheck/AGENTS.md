# expo-typecheck

Two-pass type checker: collect (build TypeContext) then check (verify types).

## Key files

- `lib.rs` -- Public API: `check()`, `check_module()`, `collect_all_names()`, `TypeContext`
- `collect.rs` -- Pass 1: walks AST to register structs, enums, functions, protocols, constants. Builds `TypeContext`. Largest file (~1672 lines)
- `check.rs` -- Pass 2: type-checks function bodies, impl blocks, protocol conformance
- `context.rs` -- `TypeContext` struct: functions, types, closures, coercions, diagnostics
- `expr.rs` -- Expression inference and checking (calls, closures, methods, generics). Largest checking file (~1837 lines)
- `stmt.rs` -- Statement checking (assignments, moves, returns, breaks)
- `pattern.rs` -- Pattern checking and match exhaustiveness
- `types.rs` -- Type helpers: `resolve_type_expr`, alias resolution, substitution, unification
- `cycle.rs` -- Recursive struct/enum detection, marks `Type::Indirect`
- `env.rs` -- Per-function `CheckEnv`, variable state tracking for ownership
- `resolve.rs` -- Resolves `Package::Unresolved` on types after module merge

## Pipeline

```
collect_all_names (global struct/enum names)
  -> collect_module (build TypeContext per module)
    -> merge (combine all modules + stdlib)
      -> synthesize_protocol_defaults
        -> mark_recursive_fields
          -> resolve_imports
            -> check_module (type-check bodies)
```
