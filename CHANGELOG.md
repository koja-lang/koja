# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- Struct and enum constants -- `const HEADING = Direction.North` (enum unit variants) and `const ORIGIN = Point{x: 0, y: 0}` (struct literals with all-constant fields) now work as constant initializers. No type annotation required for non-generic types.
- `expo build --emit-llvm` flag -- dumps the LLVM IR for a module or project to stdout instead of producing an executable. Useful for debugging codegen issues.
- Project system -- `project.expo` config file with `Project{name: "my_app", version: "0.1.0"}` struct literal format. `expo build`, `expo run`, and `expo check` detect `project.expo` in the current directory when no source file is given. Project name doubles as the module namespace prefix (`import my_app.server` resolves to `src/server.expo`). Configurable `src` dirs and `entry` module with sensible defaults.
- `expo-stdlib` crate -- standalone crate housing all standard library `.expo` sources with fully qualified module names (`std.kernel`, `std.list`, `std.string`, etc.). Both `expo-driver` and `expo-lsp` depend on it. Stdlib modules are auto-imported into every compilation.
- Unified module resolution -- project modules, stdlib modules, and (future) package modules resolve through the same namespace-aware resolver. `import my_app.X` dispatches to project `src/` dirs, `import std.X` resolves from embedded sources.
- Context-driven parameter type inference for closures -- `opt.map(v -> v * 10)` infers `v: Int` from `Option<Int>`. Works for short and block closures at inline call sites, including generic methods.
- Short closures compile to native code -- `x -> expr` closures with variable capture (copy for primitives, move for non-copy types).
- `@doc` on type aliases -- `@doc` annotations can now precede `type Name = ...` declarations. Parser, formatter, and LSP hover all support it.
- File I/O -- `Fd` type (raw file descriptor with `read`, `write`, `close`) and `File` type (wraps `Fd` with `File.read(path)` for whole-file read, `File.open(path)` for handle-based access, `File.close(move self)`). Both return `Result<T, String>` for error handling. Runtime intrinsics use POSIX I/O and Rust's `std::fs`.
- OR patterns in match arms -- `1 | 2 | 3 -> "small"` combines multiple patterns sharing one arm body. Works in `match` and `receive`. Variable bindings inside OR patterns are disallowed for now.
- String standard library -- `alpha?`, `at`, `codepoints`, `contains?`, `downcase`, `empty?`, `ends_with?`, `graphemes`, `join` (static), `replace`, `reverse`, `split`, `starts_with?`, `to_float`, `to_int`, `trim`, `trim_end`, `trim_start`, `upcase`, `whitespace?`. ASCII-only case conversion and codepoint-level iteration for now; full Unicode deferred.
- Binary and bitstring literals -- `Binary` and `Bits` as built-in types. `<<>>` syntax for construction and pattern matching. Segment modifiers: `::N` bit-width, `::N byte`, `signed`/`unsigned`, `big`/`little`, type annotations (`: Float32`, `: Int16`). Byte-aligned totals infer `Binary`, non-byte-aligned infer `Bits`. Greedy rest capture (`rest: Binary`) in match patterns. Compile-time overflow checking.
- `<>` concatenation operator for `Binary <> Binary`, `Bits <> Bits`, and `String <> String`. Type-checked, no cross-type mixing.
- `Bitwise` protocol -- `band`, `bor`, `bxor`, `bnot`, `bsl`, `bsr` as methods on all integer types.
- String/Binary/Bits conversions -- `String.to_binary()` and `Binary.to_bits()` (zero-cost, always succeed). `Binary.to_string()` and `Bits.to_binary()` return `Result` (validate UTF-8 and byte alignment respectively).
- String segments in binary literals -- string literals inside `<<>>` for construction and pattern matching (`<<"HTTP/1.1 ", rest: Binary>>`).
- Methods on primitive types -- `impl` blocks can define methods on built-in types (`String`, `Binary`, `Bits`, `Int`, etc.).
- `List.last()` -- returns `Option<T>`: `Some(element)` for the last element, `None` if empty.
- Compiler warning when the return value of a `move self` method is discarded in statement position. Suggests reassignment (`x = x.method(...)`) to capture the result.
- Tail call optimization for self-recursive `move self` methods -- the compiler detects self-recursive calls in tail position (both implicit returns and explicit `return`) and rewrites them as loops, eliminating stack growth. Covers both `-> Self` and void-returning methods (e.g., `Process.run`). The `move self` recursive idiom is now safe for unbounded iteration.

### Changed

- `List.push` renamed to `List.append` -- better reflects functional semantics (returns a new list rather than mutating in place).
- `List.get` and `String.get` now return `Option<T>` instead of panicking on out-of-bounds. Consistent with `Map.get` which already returned `Option<V>`.

- Refactored `declare_builtins` in codegen -- replaced ~200 lines of repetitive add-function/insert boilerplate with a table-driven helper, organized by category (C stdlib, process runtime, string intrinsics, file I/O).
- Stdlib sources moved from `expo-typecheck` to `expo-stdlib` -- `expo-typecheck` is now a pure checker with no embedded source files. `STDLIB_SOURCES`, `KERNEL_SOURCE`, and `merge_stdlib` removed.
- Pipeline refactored -- `parse_stdlib()`/`typecheck_modules()` replaced by unified `typecheck_graph()` and `build_from_graph()`. Both single-file and project builds flow through the same compilation path.

### Fixed

- Block expression return type inference -- `match`, `if/else`, and `cond` now infer their result type from arm bodies instead of returning `Type::Unknown`. Functions whose last expression is a block are now correctly checked against the declared return type (previously, mismatches were silently ignored).
- Constant type propagation in codegen -- constants referenced by name now carry their source-level type through compilation instead of being typed as `Unknown`. Enables downstream type-aware codegen (e.g., correct struct/enum handling when a constant is passed to a generic function).
- List/Map/Set element stride used `type_byte_size` which returned a flat 8 for all non-primitive types, ignoring actual struct layout. `List<Token>` (36 bytes per element) was stepping through memory at 8-byte intervals, causing overlapping writes and corrupted field reads. All collection codegen now uses `llvm_field_byte_size` to compute ABI-correct element strides from the real LLVM type.
- Enum payload sizing (`llvm_field_byte_size`) did not account for ABI alignment padding in nested structs, producing undersized payload arrays (e.g. `[33 x i8]` instead of `[36 x i8]` for `Option<Token>`). Rewrote with a proper alignment-aware layout algorithm.
- Functions where all control-flow paths explicitly `return` (e.g. exhaustive `match` arms each returning) emitted an unreachable `ret void` fallthrough, causing LLVM verification failures for non-Unit return types. Codegen now emits `unreachable` instead.
- `return` of heap-owning types (`List`, `Map`, `Set`, `String`) from inside `if` blocks no longer causes a use-after-free. The codegen was dropping live variables before evaluating the return expression; now the return value is loaded first and excluded from cleanup.
- Formatter: `|` patterns in match arms pack densely with trailing `|` at line breaks instead of producing a single overflowing line. Blank lines are inserted between arms when any pattern wraps.
- Formatter: `or` and `and` chains in cond conditions now pack densely (fill-style) instead of cascading one-per-line after the first break. Blank lines between cond arms when any condition wraps.

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
