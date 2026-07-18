# koja-lexer

Tokenizes Koja source into a stream of `Token` values.

## Key files

- `cursor.rs`: character lookahead with UTF-8 byte offsets and character-based line and column tracking
- `lexer.rs`: main `lex()` function. Handles comments, identifiers and keywords, line continuation, numbers, punctuation, and strings

The reserved keyword inventory is defined by `lexer.rs` and mirrored in `grammar.ebnf` and `LANGUAGE.md`.

Inline tests in `lexer.rs` pin token and diagnostic details. `tests/proptest_lex.rs` covers corpus behavior, determinism, panic safety, and UTF-8 span invariants.
