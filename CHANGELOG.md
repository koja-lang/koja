# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- Generics -- generic functions (`fn identity<T>(x: T) -> T`), generic structs (`struct Pair<A, B>`), and generic enums (`enum Option<T>`) now compile to native code via monomorphization. Type arguments are inferred at call sites.
- Variable type annotations -- `x: i32 = 42`, `z: Option<i32> = Option.None`. Annotations are optional and support all types including generics. Enables explicit typing for generic enum unit variants where the type can't be inferred.

### Changed

- Numeric literals coerce to any same-category type annotation (`x: u8 = 4`, `y: i64 = 10`, `f: f32 = 3.14`). Cross-category assignments (int to float or vice versa) remain errors.

### Fixed

- `print(true)` now outputs `true` instead of `1`. Booleans print correctly in `print()`, `print_bool()`, and string interpolation (`"#{some_bool}"`).

## [0.3.0] - 2026-03-14

### Added

- Non-capturing block closures -- `fn (a: i32, b: i32) -> i32 ... end`. Mirrors function signature syntax with required parens and explicit types. Closures compile to function pointers and can be called through variables.
- `Type::Function` in the type system -- closures are typed as `(params) -> return_type`.
- Pipe operator (`|>`) -- desugars `a |> f(b)` to `f(a, b)`. Formatter keeps short chains on one line, breaks with consistent indentation when long.
- `const` keyword for module-level constants (`const MAX_SIZE = 100`). Constants are compile-time inlined literal values (int, float, string, bool). Replaces the previous `SCREAMING_SNAKE` naming convention with an explicit keyword. Fully wired through type checker and codegen.
- Qualified imports (`math.add(1, 2)`) -- module-prefixed function calls now type-check and compile. Both `add(1, 2)` and `math.add(1, 2)` work after `import math`.
- Import conflict detection -- errors on duplicate names from different imports and duplicate module qualifiers.
- Ternary expressions (`condition ? then : else`), with nested ternaries disallowed.
- `expo doc` command -- generates static HTML documentation from `@doc` and `@moduledoc` annotations. Supports `@doc false` and `@moduledoc false` to exclude items. Renders markdown in doc strings via pulldown-cmark. Uses askama templates for HTML generation.
- Documentation generator supports recursive directory input (`expo doc src/`) with dotted module names derived from file paths (e.g. `src/what/util.expo` becomes `what.util`).
- Global sidebar navigation across all module pages with active module highlighting.
- Brand-colored documentation theme (burnt orange `#dd6900` + warm charcoal) with Source Sans 3 / Source Code Pro typography.
- LSP: hover and go-to-definition for qualified calls (`math.add()` shows signature and docs from the source module).
- LSP: nested module path resolution (`import what.util` correctly resolves to `what/util.expo`).
- LSP: closure body traversal for hover and go-to-definition inside closures.
- Vim/VSCode syntax highlighting for module names in imports and qualified calls.

### Changed

- `match` expressions are now value-producing (can be used in assignments when all arms produce values).
- `cond` expressions are now value-producing (can be used in assignments when all arms + `else` produce values).
- `else ->` catch-all arm is now required for `cond` expressions.

### Removed

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
