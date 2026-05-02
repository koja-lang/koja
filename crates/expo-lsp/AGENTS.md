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

## Vocabulary

A _package_ is a unit of distribution (your app, the stdlib, a dependency). A
_file_ is a single `.expo` source file. The LSP holds parsed files in
`Backend.stdlib_files`, `Backend.project_files`, and `DocumentState.file` /
`DocumentState.project_files`. The Expo language has no "module" concept --
when you see `module` in code below this point it is the Rust language item
(`mod foo;`) or the AST type (`expo_ast::Module`, an unrelated holdover that a
later refactor will rename to `File`). LSP-protocol enum values like
`SymbolKind::MODULE` are also unrelated and stay untouched.
