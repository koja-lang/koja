# expo-parser

Recursive descent parser. Pratt precedence for expressions.

## Key files

- `parser.rs` -- `Parser` state machine, `ParseResult`, entry `parse()` function
- `decl.rs` -- Top-level items: structs, enums, fns, impl blocks, protocols, aliases, constants
- `expr.rs` -- Pratt/infix expression parsing and operator precedence
- `construct.rs` -- String/multiline construction, interpolation, binary literals, struct/list/map literals
- `control.rs` -- `if`/`cond`/`match`/`receive`/`loop`/`while`/`for` expressions
- `stmt.rs` -- Statements: return, break, assignments, expression statements
- `pattern.rs` -- Pattern parsing for `match` arms and bindings
- `types.rs` -- `TypeExpr` parsing (unions, generics, function types)
