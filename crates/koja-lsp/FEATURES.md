# koja-lsp features

What the language server answers, what it declines, and why.

## Supported methods

| Method                                         | Notes                                                                                                                                                                       |
| ---------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `initialize`, `initialized`, `shutdown`        | Standard lifecycle.                                                                                                                                                         |
| `textDocument/didOpen`, `didChange`, `didSave` | Full document sync. Each change reparses and rechecks the bundle.                                                                                                           |
| `textDocument/publishDiagnostics`              | Parse and typecheck diagnostics, grouped by owning file. Related locations link to their own file.                                                                          |
| `textDocument/hover`                           | Signature and `@doc` text of the symbol under the cursor. Function signatures are laid out by `koja-fmt`. Locals show their type.                                           |
| `textDocument/definition`                      | Lands on the name of the declaration, in this project or in the stdlib.                                                                                                     |
| `textDocument/references`                      | Every occurrence across the project files. `includeDeclaration` adds the declaration.                                                                                       |
| `textDocument/documentHighlight`               | Occurrences in the active file. Reads are `Read`, declarations and assignments are `Write`.                                                                                 |
| `textDocument/prepareRename`, `rename`         | One `WorkspaceEdit` over every file that mentions the symbol. See the limits below.                                                                                         |
| `textDocument/completion`                      | Keywords, package symbols, and members after `.`.                                                                                                                           |
| `textDocument/signatureHelp`                   | Parameter list of the call around the cursor with the active parameter marked.                                                                                              |
| `textDocument/inlayHint`                       | `: Type` after an unannotated binding, closure parameter, or tuple destructure. `name:` before a positional argument, except when the argument is a local of the same name. |
| `textDocument/documentSymbol`                  | Outline of the active file.                                                                                                                                                 |
| `workspace/symbol`                             | Search across every open document and its project siblings.                                                                                                                 |
| `textDocument/foldingRange`                    | Blocks and comment runs.                                                                                                                                                    |
| `textDocument/formatting`                      | Runs `koja-fmt` over the buffer.                                                                                                                                            |

Navigation keeps working while the program has type errors. Typecheck runs
every pass before it reports, so the failure result carries the same
resolution stamps as a successful check. Only a parse failure leaves the
server with no symbol information.

## Rename limits

Rename edits text the user does not see, so it refuses when the result might
be incomplete or wrong. Each refusal is a request error with a message the
editor shows.

- The program has an error diagnostic. Unresolved references would be left
  behind.
- The declaration is in the stdlib or another dependency, or the compiler
  synthesized it.
- The symbol is `self`, a builtin type, or a method that implements a protocol
  requirement. The protocol owns that name.
- The symbol is reached through a file `alias` somewhere. The alias use site
  would be rewritten to the wrong text. A function with more than one arity
  is refused as soon as an alias names it, since the alias line binds every
  arity and renaming one would take the others out from under it.
- The new name is not one identifier in the same case class as the old one.
  Types start uppercase. Everything else starts lowercase or with `_`. A
  trailing `?` is allowed on lowercase names only. Keywords are rejected.

Struct fields, enum variants, and protocol method declarations have no
registry entry of their own, so they are not renameable yet.

## Declined methods

| Method                                             | Reason                                                                        |
| -------------------------------------------------- | ----------------------------------------------------------------------------- |
| `textDocument/codeAction`                          | Planned. Needs diagnostics that carry a fix.                                  |
| `textDocument/codeLens`                            | Planned, for a run-test lens on each `test` block.                            |
| Incremental text sync                              | Planned. Full sync is correct and fast enough at current project sizes.       |
| `textDocument/typeDefinition`, `implementation`    | Not requested. `definition` covers types directly.                            |
| `textDocument/semanticTokens`                      | Grammar files highlight well enough. Revisit if a client lacks one.           |
| `textDocument/rangeFormatting`, `onTypeFormatting` | `koja-fmt` formats whole files only.                                          |
| `workspace/didChangeWatchedFiles`                  | Sibling files are read from disk on each check, so watching adds nothing yet. |
