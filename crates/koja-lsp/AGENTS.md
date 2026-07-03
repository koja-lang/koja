# koja-lsp

Language server over stdio (tower-lsp). Provides IDE features for .koja files.

## Key files

- `backend.rs`: `Backend` struct, `DocumentState`, `LanguageServer` trait dispatch
- `diagnostics.rs`: parse + typecheck -> LSP diagnostics, project-aware context building
- `hover.rs`: hover text from symbols and `@doc` annotations
- `completion.rs`: dot completion (struct fields, methods) and keyword completion
- `signature_help.rs`: active parameter help inside function/method calls
- `definition.rs`: go-to-definition across files and stdlib
- `symbols.rs`: document and workspace symbol providers
- `folding.rs`: folding ranges for blocks and comment runs
- `lookup/traverse.rs`: AST walker to find expression/symbol at cursor position (~1046 lines, largest file)
- `lookup/mod.rs`: `SymbolInfo` classification API
- `convert.rs`: `Span` <-> LSP `Range` conversion, file URI helpers

## Vocabulary

A _package_ is a unit of distribution (your app, the stdlib, a dependency). A
_file_ is a single `.koja` source file. The LSP holds the embedded stdlib in
`Backend.autoimport_sources` / `Backend.qualified_sources` and each open
document's parsed and checked programs in `DocumentState`. The Koja language
has no "module" concept. When you see `module` in code below this point it is
the Rust language item (`mod foo;`). LSP-protocol enum values like
`SymbolKind::MODULE` are unrelated and stay untouched.
