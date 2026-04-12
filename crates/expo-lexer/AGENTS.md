# expo-lexer

Tokenizes Expo source into a stream of `Token` values.

## Key files

- `lexer.rs` -- Main `lex()` function. Handles strings (single-line, multiline, interpolation, escape sequences), numbers (decimal, hex, binary, float, underscores), comments, line continuation, keyword matching, TypeIdent/Ident distinction
- `cursor.rs` -- Generic character cursor with line/column tracking
