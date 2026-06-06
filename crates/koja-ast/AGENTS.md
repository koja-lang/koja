# koja-ast

Shared AST definitions used by every other crate. No logic -- just data types.

## Key files

- `ast.rs` -- Core AST nodes: `File`, `Item`, `Expr`, `Statement`, `Pattern`, comments
- `token.rs` -- `Token` and `TokenKind` for the lexer
- `identifier.rs` -- `Identifier`, `Resolution`, `ResolvedType`, `LocalId` -- the registry-pointing names and resolutions stamped by typecheck
- `coercion.rs` -- per-expression `LiteralCoercion` and `Coercion` slots populated by typecheck and consumed by IR lowering
- `labels.rs` -- short, stable diagnostic labels for AST shapes
- `debug_print.rs` -- compact tree printer for `--emit-ast`
- `span.rs` -- `Position` and `Span` for source locations

## Key types

- `Visibility` -- `Public`, `Private`
- `ResolvedType` -- registry-pointing type annotation on every `Expr`; populated by typecheck-resolve and asserted by seal
