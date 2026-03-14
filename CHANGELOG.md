# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- Pipe operator (`|>`) -- desugars `a |> f(b)` to `f(a, b)`. Formatter keeps short chains on one line, breaks with consistent indentation when long.
- `else ->` catch-all arm for `cond` expressions (required by the parser).
- `cond` expressions are now value-producing (can be used in assignments when all arms + `else` produce values).
- `match` expressions are now value-producing (can be used in assignments when all arms produce values).
- Ternary expressions (`condition ? then : else`), with nested ternaries disallowed.
- Qualified imports (`math.add(1, 2)`) -- module-prefixed function calls now type-check and compile. Both `add(1, 2)` and `math.add(1, 2)` work after `import math`.
- Import conflict detection -- errors on duplicate names from different imports and duplicate module qualifiers.
- LSP: hover and go-to-definition for qualified calls (`math.add()` shows signature and docs from the source module).
- LSP: nested module path resolution (`import what.util` correctly resolves to `what/util.expo`).
- Vim/VSCode syntax highlighting for module names in imports and qualified calls.

### Changed

- Tuples removed from the language. `(a, b)` is now grouping only; use a struct for multiple values. `Pair<A, B>` will be available in stdlib after generics land.

### Fixed

- Better error message for integer literals that overflow i64.

## [0.2.0] - 2026-03-13

### Added

- Multi-module support with import-driven discovery (`import math`, `import utils.strings`).
- Enum types with unit, tuple, and struct variants.
- `match` expressions with pattern matching, nested patterns, `when` guards, and exhaustiveness checking.
- `cond` expressions.
- String interpolation (`"hello #{name}"`) including enum values (prints variant name by default).
- Multiline strings (`"""`) with automatic dedent and escape sequences.
- `priv fn` visibility enforcement -- private functions are inaccessible from other modules.
- Circular import detection with clear error messages.
- `undefined function` diagnostic when calling functions that don't exist.
- Unused variable warnings (suppressed with `_` prefix).
- `@moduledoc` annotation for module-level documentation.
- `@doc` annotation support on `struct` and `enum` declarations (in addition to functions).
- Language server (LSP) with real-time diagnostics, document formatting, hover (type signatures + `@doc` with Markdown-rendered code blocks), go-to-definition, and module documentation on import hover.
- Hex (`0xFF`) and binary (`0b1010`) integer literals.
- Underscore separators in numeric literals (`1_000`, `0xFF_FF`).

## [0.1.0] - 2026-03-13

### Added

- Primitive types: `i32`, `i64`, `f32`, `f64`, `bool`, `string`.
- Functions with typed parameters and return types.
- Type inference for local variables.
- Structs with named fields.
- `impl` blocks with functions on structs.
- `if`/`else` expressions.
- `while` loops.
- `loop` with `break`.
- Arithmetic, comparison, and logical operators.
- Compound assignment (`+=`, `-=`, `*=`, `/=`).
- String literals.
- Polymorphic `print()` builtin.
- `expo build` -- compile to native binary via LLVM.
- `expo run` -- build and execute in one step.
- `expo check` -- type check without compiling.
- `expo format` -- opinionated code formatter (`--check`, `--write`).
- `expo parse` -- dump AST.
- `expo lex` -- dump tokens.
- Structured error messages with source context, underlines, and hints.
- Colored output with `--no-color` flag and `NO_COLOR` env var support.
- VS Code / Cursor syntax highlighting extension.
- Vim syntax highlighting.
