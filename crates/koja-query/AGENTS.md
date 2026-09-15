# koja-query

Questions about a typechecked program, asked by position or by symbol.
Protocol-neutral. `koja-lsp` is the first consumer. A documentation tool or
the shell can use the same API.

## Contract

Typecheck writes, this crate reads. Every answer comes from a stamp on the
AST (`Resolution`, `LocalId`, `Expr.resolution`) or from the `GlobalRegistry`
keyed by canonical `Identifier`. Nothing here resolves a name by scope rules.
If a query needs information that is not stamped, the fix is a stamp in
`koja-typecheck`, not a lookup here.

Synthesized nodes carry synthetic spans and never produce an occurrence, so
the `eq` behind `a == b` or the `Option` behind a `for` loop stays invisible.

## Entry point

```rust
let analysis = Analysis::from_checked(&checked);        // or from_failure(&failure)
let index = ReferenceIndex::build_filtered(&analysis, |file| is_project(file));
let symbol = symbol::symbol_at(&analysis, &index, file_id, line, col);
```

`Analysis` borrows files and registry. `from_failure` returns `None` only
when the parser failed, because typecheck runs every pass before it reports
and leaves a complete registry behind.

## Files

- `lib.rs`: `Analysis`, file table lookups by `FileId` and path.
- `visit.rs`: `Visitor` trait with `walk_*` defaults over every node
  family. Parents before children.
- `index.rs`: `ReferenceIndex`, `SymbolKey`, `Occurrence`, `Role`. One pass
  over the files. Local keys carry the enclosing function span because
  typecheck restarts local ids per function.
- `symbol.rs`: `symbol_at`, the symbol under a cursor with its registry
  entry or local type.
- `rename.rs`: `prepare_rename` and `validate_new_name`. The refusal rules
  are here so every consumer applies the same ones.
- `expr_at.rs`: innermost expression and enclosing call at a position, for
  completion and signature help.
- `docs.rs`: `@doc` text for a registry entry, found by its `name_span`.
- `display.rs`: one-line rendering of `ResolvedType` and signatures, for
  completion detail.
- `signature.rs`: a function header built from the registry entry and laid
  out by `koja-fmt`, for hover. `type_expr_of` turns a `ResolvedType` back
  into source syntax.
- `position.rs`: span containment with 1-indexed positions.

## Tests

`tests/` builds real programs against the embedded stdlib and asserts
occurrences by `(line, col)`. Add a case there when a new AST node starts
carrying a stamp.
