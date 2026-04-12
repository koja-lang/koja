# expo-lsp

Language server over stdio (tower-lsp). Provides IDE features for .expo files.

## Key files

- `backend.rs` -- `Backend` struct, `DocumentState`, `LanguageServer` trait dispatch
- `diagnostics.rs` -- Parse + typecheck -> LSP diagnostics, project-aware context building
- `hover.rs` -- Hover text from symbols and `@doc` annotations
- `completion.rs` -- Dot completion (struct fields, methods) and keyword completion
- `signature_help.rs` -- Active parameter help inside function/method calls
- `definition.rs` -- Go-to-definition across files and stdlib
- `symbols.rs` -- Document and workspace symbol providers
- `folding.rs` -- Folding ranges for blocks and comment runs
- `lookup/traverse.rs` -- AST walker to find expression/symbol at cursor position (~1046 lines, largest file)
- `lookup/mod.rs` -- `SymbolInfo` classification API
- `convert.rs` -- `Span` <-> LSP `Range` conversion, file URI helpers
