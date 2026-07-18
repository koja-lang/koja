# koja-parser

Recursive descent parser. Pratt precedence for expressions.

## Key files

- `parser.rs` -- `Parser` state machine, `ParseResult`, entry `parse()` function, shared helpers (`comma_separated`, `parse_until`, `Checkpoint`, `ERROR_IDENT`)
- `program.rs` -- Multi-file `parse_program` / `parse_file` driver, threads package + path metadata onto each `File`
- `decl/` -- Top-level items, split by kind: `alias`, `annotation`, `constant`, `enum_decl`, `extend_block`, `function`, `impl_block`, `protocol`, `struct_decl`
- `expr.rs` -- Pratt expression parsing, operator precedence, and statement-only diagnostics for `break` and `return`. Short closures are accepted only as call arguments
- `construct/` -- Constructive expressions, split by shape: `string`, `list`, `closure`, `binary`, `type_construction`
- `control/` -- Control flow, split by family: `conditional` (`if`/`unless`), `loops` (`for`/`while`/`loop`), `match_arms` (`match`/`cond`/`receive`). Arm bodies use statement parsing and reserve an unframed `x -> ...` for the next arm
- `stmt.rs` -- Statements: return, break, assignments, expression statements
- `pattern.rs` -- Pattern parsing for `match` arms and bindings
- `types.rs` -- `TypeExpr` parsing (unions, generics, function types)
