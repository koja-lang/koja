# koja-lexer

Tokenizes Koja source into a stream of `Token` values.

## Key files

- `lexer.rs`: main `lex()` function. Handles strings (single-line, multiline, interpolation, escape sequences), numbers (decimal, hex, binary, float, underscores), comments, line continuation, keyword matching, TypeIdent/Ident distinction
- `cursor.rs`: generic character cursor with line/column tracking
