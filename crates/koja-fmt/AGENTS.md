# koja-fmt

Opinionated code formatter. Wadler-Lindig document algebra, 80-column width, 2-space indent.

## Key files

- `lib.rs`: public `format()` / `format_width()`, parse -> Doc -> render
- `doc.rs`: `Doc` algebra (`Text`, `Line`, `Group`, `Indent`, `Concat`) and `render` with line-width fitting
- `printer/mod.rs`: `file_to_doc`, `Printer` struct, top-level item formatting (structs, enums, impls, protocols)
- `printer/expr.rs`: expression and match arm formatting
- `printer/util.rs`: stateless `Doc` helpers for types, patterns, literals, annotations
- `printer/comments.rs`: `CommentCursor` for preserving comment positions through full AST traversal
