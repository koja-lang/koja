# koja-lsp

Language server over stdio (tower-lsp). Provides IDE features for .koja files.
`FEATURES.md` lists the supported and declined methods and the rename limits.

Every symbol question goes through `koja-query`. This crate holds protocol
plumbing only: request parsing, `DocumentState`, and `Span` to `Range`
conversion. A handler that needs a new AST fact adds a query there, not a
walker here.

## Key files

- `backend.rs`: `Backend` struct, shared state, and the scheduling of
  analysis runs
- `server.rs`: the `LanguageServer` impl, the capabilities table, lifecycle
  notifications, and the forwards to each handler module
- `document.rs`: `DocumentState`, its position helpers, and the handler
  prelude `with_state` and `with_analysis` with `Doc`
- `buffer.rs`: the live text of every open document, keyed by `Uri`
- `diagnostics.rs`: loads the bundle through `koja_project::ProjectLoader`
  with the open buffers as overlays, parse + typecheck -> LSP diagnostics,
  the reference index build, and the per-project `Published` set
- `hover.rs`: hover text from registry entries and `@doc` annotations
- `definition.rs`: go-to-definition across files and stdlib
- `references.rs`, `highlight.rs`: occurrences from the reference index
- `rename.rs`: `prepareRename` and `rename` over the index, refusals from
  `koja_query::rename`
- `completion.rs`: dot completion (struct fields, methods) and keyword
  completion
- `signature_help.rs`: active parameter help inside function/method calls
- `inlay_hint.rs`: type and parameter name hints from `koja_query::inlay`
- `code_action.rs`: quick fixes carried in a diagnostic's `data`
- `code_lens.rs`: a run-test lens on each test
- `symbols.rs`: document and workspace symbols, mapped from
  `koja_query::outline`
- `folding.rs`: folding ranges, mapped from `koja_query::folding`
- `convert.rs`: `Span` <-> LSP `Range` conversion, file URI helpers

## Handler prelude

A handler that works on the AST alone, such as folding, calls
`with_state`. One that asks about types calls `with_analysis` and gets a
`Doc`, the analysis opened on the active file with its `Positions`. Both
return `Ok(None)` when the document has no completed run.

## Document state

`DocumentState` keeps the post-typecheck ASTs and the registry from the
same run for both the success and the failure arm, plus the reference index
over the project files. Hover, definition, references, and highlight work
while the program has type errors. Rename refuses then.

## Vocabulary

A _package_ is a unit of distribution (your app, the stdlib, a dependency). A
_file_ is a single `.koja` source file. Every diagnostic run loads its bundle
fresh through `koja-project`, stdlib included, and each open document's
parsed and checked programs live in `DocumentState`. The Koja language has no
"module" concept. When you see `module` in code below this point it is the
Rust language item (`mod foo;`). LSP-protocol enum values like
`SymbolKind::MODULE` are unrelated and stay untouched.
