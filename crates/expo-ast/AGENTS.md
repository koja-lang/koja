# expo-ast

Shared AST definitions used by every other crate. No logic -- just data types.

## Key files

- `ast.rs` -- Core AST nodes: `Module`, `Item`, `Expr`, `Statement`, `Pattern`, `PassMode`, comments
- `types.rs` -- Resolved `Type` enum, `FnParam`, `Primitive`. Every `Expr` carries `resolved_type: Option<Type>` populated by the type checker
- `token.rs` -- `Token` and `TokenKind` for the lexer
- `identifier.rs` -- `Package` and `TypeIdentifier` for package-qualified type names
- `span.rs` -- `Position` and `Span` for source locations

## Key types

- `PassMode` -- `Copy`, `Move`, `Borrow` (parameter ownership semantics)
- `Visibility` -- `Public`, `Private`
- `Type` -- the resolved type representation, including `Struct`, `Enum`, `GenericInstance`, `Indirect`, `Union`, `Pointer`
