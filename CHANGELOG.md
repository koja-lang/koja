# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.0] - 2026-03-22

### Added

- Union types -- `Post | Comment | Ad` as anonymous unions, `type Pet = Cat | Dog | Fish` as named aliases. Automatic widening at assignment/call/return sites. `match` with typed binding patterns (`p: Post -> p.title`).
- `Process<C, M, R>` protocol -- structs implement `Process<C, M, R>` to become processes. `spawn T.new(config)` returns a typed `Ref<M, R>` handle. `Ref.cast` for fire-and-forget, `Ref.call` for synchronous request/reply with timeout.
- Default protocol implementations -- protocols can provide method bodies as defaults. Types inherit defaults automatically or override with their own implementation.
- `Task<R>` -- `Task.async(fn () -> R)` / `Task.await` for one-off async work, built on `Process` / `Ref` / `call`.
- Cooperative process runtime -- processes block on `receive` and resume on message arrival. `receive ... after` timeout clause for timed receives. Cross-platform: aarch64, x86_64, Windows.
- Recursive types -- structs and enums can reference themselves (linked lists, trees). Automatic cycle detection, heap indirection, and cleanup on drop.
- Typed constants -- `const NAME: Type = expr` with optional type annotations for generic inference.
- LSP: autocomplete (keywords, symbols, imports), signature help, document symbols, hover/go-to-definition on type names in match patterns and constant annotations.
- VS Code: "Expo: Run File" and "Expo: Build File" commands, `expo.path` setting.

### Changed

- **Breaking**: `spawn` requires `T.new(config)` form and returns `Ref<M, R>` (typed handle). Bare function spawn is a compile error. Replaces the old `Process<M>` model.
- **Breaking**: `fn main` runs as a process -- `main` can use `receive`, `call`, and other blocking operations directly alongside spawned processes.
- **Breaking**: `await` keyword removed; task completion uses `Task.await(ref)`.

### Fixed

- `receive` blocks until a message arrives instead of crashing on empty mailbox. Clean exit when `main` finishes (no false deadlock for background processes). `Ref.call` correctly delivers `ReplyTo` and returns the reply value.
- Integer literals in binary operations coerce to match the other operand's width. Method arguments on monomorphized generic types are properly coerced.
- Generic struct literals infer type arguments from field-access types. Generic enum unit variants resolve correctly inside methods with their own type parameters. Generic struct construction from local variables works with function type parameters.
- `Pair<Unit, T>` and similar types with zero-sized fields keep LLVM field indices aligned with layout metadata.
- `expo` CLI reliably links the embedded process runtime across Cargo target directories.
- Vim: multiline docstring highlighting stays consistent when scrolling.

## [0.6.0] - 2026-03-18

### Added

- Lightweight processes -- `spawn` creates a process, `receive` blocks for messages, `Process<M>` is a typed handle with `send`. Message type can be any type (primitives, structs, enums).
- `Map<K, V>` -- generic hash map. Methods: `new`, `put`, `get`, `has?`, `remove`, `length`, `empty?`. Keys must implement `Hash` and `Equality`.
- `Set<T>` -- generic hash set. Methods: `new`, `insert`, `has?`, `remove`, `length`, `empty?`.
- Map literal syntax -- `["key": value]` for populated maps, `[:]` for empty maps.
- List literal syntax -- `[1, 2, 3]` backed by `ListLiteral<T>` protocol.
- `Hash` and `Equality` protocols with built-in implementations for all primitives.
- `List<T>` iterator functions: `map`, `filter`, `any?`, `all?`.
- Bare function references -- `f = double`, `list.map(double)`.
- String equality (`==`, `!=`).
- `unless` expression -- negated `if` for guard clauses.
- `Self` type expression in `protocol` and `impl` blocks.

### Changed

- `Enumerable<T>` protocol renamed to `Enumeration<T>`.

### Fixed

- Interpolated strings no longer produce dangling pointers when returned from functions.
- String memory is now freed at scope exit when owned.
- VS Code: function names with `?` or generics now highlight correctly.

## [0.5.0] - 2026-03-17

### Added

- Ownership and borrowing -- move semantics for non-copy types (structs, enums, `String`). Assignment moves by default; using a moved value is a compile error. Copy types (primitives, `Bool`, `()`, function pointers) are implicitly duplicated. Function parameters borrow by default (read-only); use `move` to take ownership. `move self` enables mutating impl functions that return the modified value (`list = list.push(42)`). `fn (move T) -> U` function type syntax distinguishes borrowing from owning signatures. Variable state tracking (`Live`, `Moved`, `MaybeMoved`) catches use-after-move across branches.
- `List<T>`, `for` loops, and `Enumerable<T>` protocol -- dynamically-sized, heap-backed collection with `List.new()`, `push`, `get`, `length`, and `empty?`. `for item in list ... end` iterates over any type implementing `Enumerable<T>` (defines `length` and `get`). Push uses move semantics and returns the updated list.
- Protocols -- `protocol` keyword for defining function contracts. `impl Protocol for Type` for conformance. Protocol functions are validated for completeness and signature compatibility. `priv fn` helpers allowed in impl blocks. `@doc` annotations supported on protocol declarations.
- Closure captures -- closures can now capture variables from their enclosing scope. Copy types are duplicated; non-copy types are moved, making the original unusable after capture. Captured closures use heap-allocated environment structs that are automatically freed when the closure goes out of scope.
- `clone()` and drop insertion -- `clone()` produces a new owned value without moving the original. Drop insertion provides deterministic cleanup at scope boundaries: `List<T>` backing buffers and captured closure environments are freed automatically.
- Static functions on `impl` blocks -- functions without a `self` parameter are called on the type directly (`List.new()`, `Option.None`). No special syntax needed; just omit `self`.
- Annotation-driven type inference for generics -- `list: List<Int32> = List.new()` infers `T = Int32` from the variable's type annotation.
- VS Code extension: syntax highlighting for ` ```expo ` fenced code blocks in Markdown files.

### Fixed

- Formatter: short struct and enum-struct construction literals now format inline (e.g. `Config{name: "yo", enabled: true}`) instead of always breaking across multiple lines. Trailing commas are added only in the multi-line form.

### Removed

- Pipe operator (`|>`). Dot-call chaining with `move self` functions covers the same use case. The planned `command` construct will handle complex sequential data flow.
- Try operator (`?`). Hidden control flow violates the "no magic" principle. Error handling uses explicit `map`/`then`/`or` chaining instead.
- `ref T` type syntax, turbofish (`::<T>`), and bare `none` keyword. `ref T` is redundant with borrow-by-default; type arguments are inferred from arguments and annotations; `Option.None` replaces bare `none` for proper type checking.

## [0.4.0] - 2026-03-15

### Added

- Generics -- generic functions (`fn identity<T>(x: T) -> T`), generic structs (`struct Pair<A, B>`), and generic enums (`enum Option<T>`) now compile to native code via monomorphization. Type arguments are inferred at call sites.
- Stdlib types via `std.kernel` -- `Option<T>`, `Result<T, E>`, and `Pair<A, B>` are auto-imported into every module. Methods: `unwrap`, `or`, `some?`/`none?` (Option), `ok?`/`err?` (Result), `map`, `then` (both).
- Function type syntax and higher-order methods -- `fn(Int32) -> String` as a type expression enables declaring parameters that accept closures. `map` transforms the contained value; `then` (flat map) chains operations returning `Option`/`Result`.
- Variable type annotations -- `x: Int32 = 42`, `z: Option<Int32> = Option.None`. Optional, supports all types including generics.
- `panic(message)` builtin -- prints the message to stderr and aborts the process. Used by `unwrap` for fatal failures.
- `or` and `and` keywords are now valid as method and field names after `.` (e.g. `x.or(default)`).

### Changed

- **Breaking**: All primitive types renamed to PascalCase. `i32` -> `Int32`, `i64` -> `Int`, `f32` -> `Float32`, `f64` -> `Float`, `bool` -> `Bool`, `string` -> `String`. Unsigned types: `u8` -> `UInt8`, `u16` -> `UInt16`, `u32` -> `UInt32`, `u64` -> `UInt64`. Primitives and user-defined types are now visually uniform.
- **Breaking**: `ref<T>` syntax changed to `ref T` (no angle brackets). `ref` is now a keyword modifier like `const`, `priv`, and `move`.
- Numeric literals coerce to any same-category type annotation (`x: UInt8 = 4`, `y: Int = 10`, `f: Float32 = 3.14`). Cross-category assignments (int to float or vice versa) remain errors.

### Fixed

- `print(true)` now outputs `true` instead of `1`. Booleans print correctly in `print()`, `print_bool()`, and string interpolation (`"#{some_bool}"`).
- Formatter: preserves blank lines between comments and code, no longer inserts spurious `()` on unit enum variant patterns, and correctly places comments inside enum/struct bodies.

## [0.3.0] - 2026-03-14

### Added

- Non-capturing block closures -- `fn (a: Int32, b: Int32) -> Int32 ... end`. Mirrors function signature syntax with required parens and explicit types. Closures compile to function pointers and can be called through variables.
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

- Primitive types: `Int`, `Int32`, `Float`, `Float32`, `Bool`, `String` (and sized integer types).
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
