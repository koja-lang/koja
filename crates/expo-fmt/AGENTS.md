# expo-fmt

Opinionated code formatter. Wadler-Lindig document algebra, 80-column width, 2-space indent.

## Key files

- `lib.rs` -- Public `format()` / `format_width()`: parse -> Doc -> render
- `doc.rs` -- `Doc` algebra (`Text`, `Line`, `Group`, `Nest`, `Concat`) and `render` with line-width fitting
- `printer/mod.rs` -- `file_to_doc`, `Printer` struct, top-level item formatting (structs, enums, impls, protocols)
- `printer/expr.rs` -- Expression and match arm formatting
- `printer/util.rs` -- Stateless `Doc` helpers for types, patterns, literals, annotations
- `printer/comments.rs` -- `CommentCursor` for preserving comment positions through full AST traversal
