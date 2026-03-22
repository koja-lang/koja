# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- Cooperative process runtime -- processes block properly on `receive` and resume when a message arrives. Supports aarch64 (Apple Silicon, Linux ARM), x86_64 (Linux, macOS), and x86_64 Windows.
- `Ref.call` -- synchronous request/reply. `ref.call(msg, timeout_ms)` sends a message, waits for the reply, and returns `Option<R>` (`Some` on reply, `None` on timeout).
- `Ref.cast` -- fire-and-forget message send. `ref.cast(msg)` sends a message and returns immediately.
- `fn main` runs as a process -- `main` can now use `call`, `receive`, and other blocking operations alongside spawned processes.
- Protocol-based process model -- structs implement `Process<C, M, R>` to become processes. `C` is the config type, `M` is the message type, `R` is the reply type. Two required methods: `new(config: C) -> Self` and `handle(move self, msg: M, from: Option<ReplyTo<R>>) -> Self`. Replaces the old caller-side `Process<M>` annotation.
- Default protocol implementations -- protocols can now provide method bodies that serve as defaults for implementors. Types that `impl` a protocol without defining a default method automatically inherit it. Types can override defaults by providing their own implementation. Synthesized at the AST level with full type parameter substitution (`Self`, protocol type params).
- Pair-based process mailbox envelope -- `cast` and `call` wrap messages in `Pair<M, Option<ReplyTo<R>>>` before sending. The default `run` loop receives the pair and unpacks `msg` and `from` for `handle`. `cast` sends `Pair<msg, Option.None>`; `call` sends `Pair<msg, Option.Some(ReplyTo{id: caller_pid}))` using `expo_rt_self()`.
- `spawn T.new(config)` syntax -- creates a process by calling the struct's `new` method with a config value, runs `run` in a new process, and returns a typed `Ref<M, R>` handle.
- `receive ... after` timeout clause -- `receive` blocks now support an optional `after timeout_ms` body that executes when no message arrives within the timeout. No arrow on the `after` clause (it's not a pattern match). Wired end-to-end through parser, type checker, and codegen (`expo_rt_receive_timeout`).
- Recursive types -- structs and enums that reference themselves (e.g. linked lists, trees) are now supported without any special syntax. The compiler automatically detects cycles in the type graph, inserts heap-allocated indirection where needed, and frees the memory on drop. Works with generics and stdlib types like `Option<T>`.
- Union types -- `Post | Comment | Ad` as anonymous unions, `type Pet = Cat | Dog | Fish` as named union aliases. Values of a member type widen automatically to the union type at assignment, call, and return sites. `match` works on union-typed values with typed binding patterns (`p: Post -> p.title`) for matching by type and binding the unwrapped value.
- Typed constants -- `const NAME: Type = expr` now accepts an optional type annotation, enabling generic type inference for constant declarations (e.g. `const SESSIONS: TableDefinition<String, Bytes> = TableDefinition.new("sessions")`).
- LSP -- hover and go-to-definition on type names and enum variants inside match patterns (including typed bindings, constructors, and enum patterns).
- LSP -- hover and go-to-definition on type names within constant type annotations.
- LSP -- document symbols (outline view) for functions, structs, enums, constants, impl blocks, protocols, type aliases, and shared declarations.
- LSP -- completion for keywords and known symbols (functions, structs, enums, constants, imported modules) from the current module and stdlib.
- LSP -- signature help with parameter hints when typing function calls.
- VS Code -- "Expo: Run File" and "Expo: Build File" commands in the command palette.
- VS Code -- `expo.path` setting to configure the `expo` CLI binary location.

### Changed

- **Breaking**: Remove the `await` keyword; task completion uses `.await()` on handles (see [CONCURRENCY.md](design/CONCURRENCY.md)). The identifier `await` is no longer reserved.
- The EBNF grammar (`grammar.ebnf`) now spells out `while` loops explicitly (behavior matches what the compiler already accepted).
- **Breaking**: `spawn` now requires the `T.new(config)` form (`spawn Counter.new(config)`). Bare function spawn (`spawn some_function`) is a compile error. Processes must implement `Process<C, M, R>`.
- **Breaking**: `spawn` returns `Ref<M, R>` (typed process handle) instead of `Process<M>`. `M` and `R` are inferred from the struct's `Process<C, M, R>` implementation.

### Fixed

- `Ref.call` now correctly delivers the `ReplyTo` handle to the process handler, and returns the reply value to the caller.
- `receive` -- when the mailbox is empty and there is no `after` clause, the process blocks until a message arrives instead of crashing.
- Clean exit -- when `main` finishes, the program exits immediately instead of reporting a false deadlock for background processes.
- Typed binding patterns on non-union types -- `pair: Pair<M, Option<ReplyTo<R>>>` in a `receive` arm now works when the annotation matches the subject type (previously only worked for union member discrimination).
- Proper coercion of Int type in structs, enums, functions, and more.
- Integer literals in binary operations now coerce to match the other operand's width (e.g. `x * 2` where `x: Int32` no longer produces an LLVM type mismatch).
- Method arguments on monomorphized generic types are now properly coerced (e.g. `Option<Int32>.or(99)` correctly truncates the literal to `Int32`).
- `Enumeration` protocol, `List`, `Map`, and `Set` now declare `length` and `get` with `Int` (i64) to match intrinsic implementations.
- Generic enum unit variants (e.g. `Option.None`) inside methods with their own type parameters (e.g. `map<U>`) now resolve to the correct monomorphized type instead of producing `ret void`.
- Enums now support line breaks in construction.
- Generic struct construction -- when a field is initialized from a local variable, the compiler uses that variable's type to infer generic parameters (fixes cases such as storing a closure in a field typed with a function type parameter).
- Monomorphized calls that involve function types as generic arguments (e.g. passing a closure where a `fn(...) -> ...` type parameter is expected) compile more reliably.
- Building the `expo` CLI from source reliably links the embedded process runtime, including when using a non-default Cargo target directory or building crates in parallel.
- Vim: syntax highlighting stays consistent when jumping or scrolling in long files with multiline docstrings (`"""`); reserved-word list updated; docstrings no longer mis-highlight prose like `` `key: value` `` as typed field syntax.

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
