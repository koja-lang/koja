# koja-lsp

Language server over stdio (tower-lsp). Provides IDE features for .koja files.
`FEATURES.md` lists the supported and declined methods and the rename limits.

Every symbol question goes through `koja-query`. This crate holds protocol
plumbing only: request parsing, `DocumentState`, and `Span` to `Range`
conversion. A handler that needs a new AST fact adds a query there, not a
walker here.

## Key files

- `backend.rs`: `Backend` struct, `DocumentState`, `LanguageServer` trait dispatch
- `diagnostics.rs`: parse + typecheck -> LSP diagnostics, project-aware context building, reference index build
- `hover.rs`: hover text from registry entries and `@doc` annotations
- `definition.rs`: go-to-definition across files and stdlib
- `references.rs`, `highlight.rs`: occurrences from the reference index
- `rename.rs`: `prepareRename` and `rename` over the index, refusals from `koja_query::rename`
- `completion.rs`: dot completion (struct fields, methods) and keyword completion
- `signature_help.rs`: active parameter help inside function/method calls
- `symbols.rs`: document and workspace symbol providers
- `folding.rs`: folding ranges for blocks and comment runs
- `convert.rs`: `Span` <-> LSP `Range` conversion, file URI helpers

## Document state

`DocumentState` keeps the post-typecheck ASTs and the registry from the
same run for both the success and the failure arm, plus the reference index
over the project files. Hover, definition, references, and highlight work
while the program has type errors. Rename refuses then.

## Vocabulary

A _package_ is a unit of distribution (your app, the stdlib, a dependency). A
_file_ is a single `.koja` source file. The LSP holds the embedded stdlib in
`Backend.autoimport_sources` / `Backend.qualified_sources` and each open
document's parsed and checked programs in `DocumentState`. The Koja language
has no "module" concept. When you see `module` in code below this point it is
the Rust language item (`mod foo;`). LSP-protocol enum values like
`SymbolKind::MODULE` are unrelated and stay untouched.
