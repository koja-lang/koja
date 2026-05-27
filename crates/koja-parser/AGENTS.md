# koja-parser

Recursive descent parser. Pratt precedence for expressions.

## Key files

- `parser.rs` -- `Parser` state machine, `ParseResult`, entry `parse()` function, shared helpers (`comma_separated`, `parse_until`, `Checkpoint`, `ERROR_IDENT`)
- `program.rs` -- Multi-file `parse_program` / `parse_file` driver, threads package + path metadata onto each `File`
- `decl/` -- Top-level items, split by kind: `struct_decl`, `enum_decl`, `protocol`, `impl_block`, `function`, `constant`, `alias`, `annotation`
- `expr.rs` -- Pratt/infix expression parsing and operator precedence
- `construct/` -- Constructive expressions, split by shape: `string`, `list`, `closure`, `binary`, `type_construction`
- `control/` -- Control flow, split by family: `conditional` (`if`/`unless`), `loops` (`for`/`while`/`loop`), `match_arms` (`match`/`cond`/`receive`)
- `stmt.rs` -- Statements: return, break, assignments, expression statements
- `pattern.rs` -- Pattern parsing for `match` arms and bindings
- `types.rs` -- `TypeExpr` parsing (unions, generics, function types)
