# Expo Language Roadmap

Solo developer + AI assistance. Bootstrap in Rust, self-host in Expo.

---

## Current state

### Compiler

An 11-crate Rust workspace that compiles Expo source to native binaries via LLVM:

- `expo-ast` -- tokens, spans, AST node definitions
- `expo-lexer` -- custom tokenizer
- `expo-parser` -- recursive descent parser (Pratt precedence for expressions)
- `expo-typecheck` -- type inference and semantic analysis
- `expo-codegen` -- LLVM IR generation via `inkwell`
- `expo-stdlib` -- embedded standard library `.expo` sources with fully qualified module names
- `expo-fmt` -- opinionated code formatter
- `expo-doc` -- HTML documentation generator (askama templates, pulldown-cmark)
- `expo-runtime` -- native process scheduler (C ABI static library linked into compiled binaries)
- `expo-driver` -- CLI binary (`expo`)
- `expo-lsp` -- language server (diagnostics, formatting, hover, go-to-definition, pattern symbol resolution)

### CLI

Seven commands: `expo build`, `expo run`, `expo check`, `expo format`, `expo doc`, `expo lex`, `expo parse`. All commands support multi-module projects.

### What compiles to native binaries today

- Multi-module imports (including qualified calls like `math.add()`)
- Functions (`fn`/`priv fn`)
- Constants (`const`) with optional type annotations (`const NAME: Type = expr`), including enum unit variants and struct literals with all-constant fields
- Structs
- Enums
- Impl blocks and methods (`self`)
- Generic functions with monomorphization
- Generic structs with monomorphization
- Generic enums with monomorphization
- Variable type annotations (`x: Int32 = 42`, `z: Option<Int32> = Option.None`)
- Numeric type coercion for annotated variables (`x: UInt8 = 4` casts at compile time)
- `if`/`else`
- `unless`
- `while`
- `loop` and `break`
- `return`
- `match`
- `cond`
- Ternary (`? :`)
- Compound assignment (`+=`, `-=`, `*=`, `/=`)
- String interpolation
- Protocols (`protocol` keyword, `impl Protocol for Type` conformance)
- Closures (block form, with variable capture -- copy for primitives, move for structs/enums)
- Function type syntax (`fn(T) -> U`) for closure-accepting parameters
- `print` builtin
- `panic` builtin (prints to stderr, aborts)
- Lightweight processes -- structs implement `Process<C, M, R>` protocol. `spawn T.new(config)` creates a process and returns `Ref<M, R>`. `receive` blocks for messages with optional `after timeout` clause. Message type `M` can be any type (primitives, structs, enums). Backed by `expo-runtime` cooperative scheduler.
- **`Task<R>`** -- `Task.async(fn () -> R)` / `Task.await` for one-off async work on top of processes (`Ref<(), R>` + `call`); see `std.kernel` and `tests/lang/task.expo`.
- Primitives: `Int`, `Int8`, `Int16`, `Int32`, `UInt8`, `UInt16`, `UInt32`, `UInt64`, `Float`, `Float32`, `Bool`, `String`
- List literal syntax (`[1, 2, 3]`) backed by `ListLiteral<T>` protocol
- Map literal syntax (`["key": value]`, `[:]` empty) backed by `MapLiteral<K, V>` protocol
- `Self` type expression in `protocol` and `impl` blocks
- `Hash` and `Equality` protocols with intrinsic implementations for all primitives
- Stdlib types: `Option<T>`, `Result<T, E>`, `Pair<A, B>`, `Map<K, V>`, `Set<T>` (auto-imported from `std.kernel`)
- `List<T>` iterator functions (`map`, `filter`, `any?`, `all?`) implemented as pure Expo code in `std.kernel`
- Bare function names as references (`f = double; f(5)`, `list.map(double)`) -- top-level functions produce closure-compatible fat pointers via thunk wrappers
- Union types (`A | B | C`) -- anonymous tagged unions with widening coercion, exhaustiveness checking, and `match` support with typed binding patterns (`p: Post -> p.title`)
- Named union aliases (`type FeedItem = Post | Comment | Ad`) -- `type` keyword declarations resolved in the type context

### Parsed and type-checked but NOT yet in codegen

- `arena` blocks (deferred post-v1)
- Trait bounds on generic type parameters

### Design notes

- **No tuples**: Expo does not have anonymous tuple syntax. `(a, b)` is grouping only. For multiple return values, use a struct. `Pair<A, B>` (with `.first` / `.second`) is available in the stdlib for lightweight two-value cases. 3+ values should always be a struct. Note: `(a, b)` pair syntax may return once protocols land via a `PairLiteral<A, B>` literal protocol -- this would be protocol-backed syntax, not a built-in tuple type, and is limited to arity 2.
- **`()` as the unit expression**: `()` is a "do-nothing" expression (empty closure that runs and returns nothing). Use `else -> ()` in `cond` for side-effect-only fallthrough.
- **Closures**: Block closures with explicit types and parens: `fn (a: Int32, b: Int32) -> Int32 ... end`. Mirrors function signature syntax. Short closures (`x -> expr`) with full capture support and context-driven parameter type inference at inline call sites. Used by `map`/`then` on `Option` and `Result`.
- **No private modules**: Files are modules, and all modules are importable. Access control lives at the function level (`priv fn`), not the module level. Use `@moduledoc false` to signal "internal, don't depend on this" -- a documentation-level convention, not a compiler wall. This matches Elixir's approach and avoids the complexity of Rust's `pub(crate)` or Go's `internal/` directory enforcement.
- **PascalCase primitives and type simplification** (done): Primitives renamed from `i32`/`i64`/`f32`/`f64`/`bool`/`string` to PascalCase: `Int` (64-bit default), `Int32`, `Float` (64-bit IEEE default), `Float32`, `Bool`, `String`. User-defined types (`Pair`, `User`) and language types (`Int`, `String`) are now visually uniform. `Decimal` will ship in the stdlib as an exact-arithmetic type for financial/business logic, sitting alongside the primitives with no visual distinction.
- **`ref T` syntax** (parsed, deferred): Reference types use `ref T` (space, no angle brackets) instead of `ref<T>`. `ref` is a lowercase keyword modifier, consistent with the modifier pattern (`const`, `priv`, `move`, `ref`): lowercase keywords modify the thing that follows them, PascalCase names are always types. However, `ref T` is redundant in parameter position (borrow-by-default) and unsafe in return position without lifetime tracking. Deferred until a concrete use case emerges.
- **Map literal syntax** (decided): `[key: value, key: value]` with `[:]` for empty maps. Maps are collections (like `List<T>`), not struct-like, so they share the bracket family rather than curly braces. The parser disambiguates list vs. map by peeking for `:` after the first expression. Curly braces remain exclusive to struct construction (`Config{name: "yo"}`).
- **Subscript syntax** (deferred): `map["key"]` / `list[0]` as sugar for `.get()`. Would be backed by a protocol (e.g., `Subscript<K, V>`). Not needed yet -- method access (`map.get(key)`, `list.get(0)`) works. Can be added later without grammar conflicts since `[` after an expression is a different parse context than `[` at expression start.
- **Planned: Irrefutable struct destructuring**: `Config{name, port} = load_config()` as syntactic sugar for pulling struct fields into local variables. Compile-time verified exhaustive -- only works for structs (single shape), not enums. Enum destructuring uses `match`.

### Known gaps

- **Generic enum unit variants in top-level code**: `Option.None` cannot infer `T` without usage context in bare declarations -- workaround: variable type annotations (`z: Option<Int32> = Option.None`). Inside monomorphized method bodies and closures with return type annotations, generic enum construction resolves all type parameters automatically. Also affects generic function calls where one argument is a generic unit variant: `Pair.new(self, Option.None)` in a function returning `Pair<Lexer, Option<String>>` fails to infer `A` and `B` because the return type isn't propagated into the call. Workaround: use struct literals directly (`Pair{first: self, second: Option.None}`) where the return type annotation provides context, or bind with a type annotation first.
- **Type checker**: `ref T` parsed but deferred (redundant with borrow-by-default, revisit if a concrete use case emerges)
- **Formatter**: `fn()` vs `fn ()` spacing inconsistency in function type syntax vs closure literal syntax -- needs a consistent formatting rule
- **Iteration protocol**: `Enumeration<T>` requires `length()` + `get(index)`, locking `for` to index-based while loops. This precludes lazy iteration, streaming, and any non-random-access collection (maps, linked lists, generators). Pre-v1.0, replace with an `Iterator<T>` protocol using `next(move self) -> Option<Pair<T, Self>>`. `get` now returns `Option<T>`. Codegen change is contained to `compile_for` in `loops.rs`; List/String impls wrap existing index-based access in iterator state. Note: the current `for` loop hides the `Option` from the user (unwraps automatically since iteration is bounds-checked). With lazy iteration, `Option` becomes the termination mechanism -- `for` desugars to `loop { match iter.next() ... }` and `None` breaks the loop.
- **Generic containers of recursive types**: `List<T>` and `Map<K, V>` fail in codegen when `T`/`V` is a recursive enum (e.g., `enum Val` with a `Arr(List<Val>)` variant). Direct recursion works (`Tree.Branch(Tree, Tree)`) because `Indirect` wrapping handles the pointer indirection, but the monomorphized generic (`List_$Val$`) can't be loaded from a match binding. Blocks JSON-style data models (`Array(List<JsonValue>)`, `Object(Map<String, JsonValue>)`). Fix is in codegen type mapping -- `to_llvm_type` / variable loading needs to handle `Indirect` element types inside generic containers.
- **Closure `move` params**: `ClosureParam` has no `PassMode` field -- `fn (move x: T) -> U ... end` doesn't parse. `Type::Function` also doesn't carry param modes, so the type checker can't enforce `fn(move T) -> U` vs `fn(T) -> U` contracts. Both need fixing: add `mode` to `ClosureParam`, parse `move` in closure params, and add param modes to `Type::Function` for type-level enforcement.
- ~~**Tail call optimization**~~: **Done.** Self-recursive `move self` methods are rewritten as loops when a self-call appears in tail position (implicit returns and explicit `return`). Covers both `-> Self` and void-returning methods (e.g., the default `Process.run` server loop). Eliminates stack growth for the language's core recursive idiom. General TCO (mutual recursion, arbitrary tail calls) remains future work.
- **List mutation methods**: `List<T>` has no `pop`/`remove_last`, `replace_at`, or `set` methods. Elements retrieved via `get` or `last` are copies, so mutating them has no effect on the list. Blocks natural patterns like tracking brace depth in an interpolation stack (`string_stack.last()` returns a copy). Fix: add `pop` (returns `Option<Pair<T, List<T>>>` or similar) and `replace_at(index, value)` as intrinsics in `std.list` with codegen in `list.rs`. Surfaced by the self-hosted lexer port.
- **Identifier priming for keyword/builtin collisions**: (for self-hosting) `IDENT` and `TYPE_IDENT` cannot use reserved words or built-in type names as identifiers. Trailing prime notation (`'`) would allow `end'` as a field name and `Self'` or `String'` as enum variant names without ambiguity. Grammar change: append `[ "'" ]` to both `IDENT` and `TYPE_IDENT` rules (trailing-only, single prime). Surfaced by the `expo-ast` self-hosting port: `Span.end` had to become `Span.stop`, and enum variants like `Self`, `String`, `Bool`, `Int`, `Float` needed descriptive renames (`SelfReceiver`, `StringVal`, `BoolLit`, etc.). Leading `'` stays invalid, so `'wrongstring'` is always a syntax error.

### Design artifacts

- **Language design** -- syntax decisions, memory model, async model, module system, all finalized through iterative design sessions
- **EBNF grammar** -- `grammar.ebnf`, ~460 lines covering all syntax constructs
- **Example codebase** -- 17 `.expo` files porting `auth-manager` (a real Rust microservice) into Expo pseudocode, validating the language feels right
- **Memory strategy** -- documented in `archive/20260323-MEMORY.md` (stack, ownership+move, explicit arena)
- **Concurrency model** -- documented in `archive/20260313-CONCURRENCY.md` and `archive/20260323-CONCURRENCY.md` (processes, native runtime, supervision)
- **Project config format** -- `project.expo` replacing `Cargo.toml`

### Tooling (pulled forward)

- **Formatter** -- `expo format --write` / `--check`, opinionated and zero-config, handles escape re-encoding for round-trip correctness, preserves `@moduledoc`/`@doc` annotations
- **LSP** -- `expo-lsp` binary providing real-time diagnostics, document formatting, hover (Markdown-rendered type signatures + `@doc`/`@moduledoc`), and go-to-definition (including qualified module calls) over stdio, integrated with the VSCode/Cursor extension
- **VSCode extension** -- syntax highlighting and LSP client for `.expo` files

### Build history

Phase 1 (bootstrap compiler) and Phase 2 (core language) are complete. The full build history with detailed implementation notes is preserved in [archive/20260318-ROADMAP.md](archive/20260318-ROADMAP.md).

---

## Phase 3: Language surface + Runtime maturity

Phase 2 proved the core language works. Phase 3 makes it real on two fronts simultaneously. These tracks are independent -- no blocking dependencies between them -- so they can be interleaved based on energy, or worked in focused bursts.

### Dependency graph

```
Track A:  A1a (lexer/parser/AST) ✓ → A1b (types + type checker) ✓ → A1c (codegen: construction) ✓
          → A1d (codegen: pattern matching) ✓ → A1e (concat + bitwise) ✓
          → A2a (type conversion) ✓ → A2b (string stdlib) ✓ → A2c (OR patterns) ✓ → A2d (ranges, deferred)
          → A3a (file I/O) ✓ → A3b (project system) ✓ → A3c (test runner) → A4 (lexer port)

Track B:  B1 (union types) ✓ → B2 (Process<C,M,R> protocol + default impls + Ref + cast/call) ✓ → B3 (Task) ✓
          B4 (scheduler/IO) -- independent, anytime
```

No dependencies between tracks. B1, B2, and B3 are complete. B4 can slot in anywhere. Track A sub-milestones are sequential -- each builds on the previous. See `archive/20260323-BITSTRINGS.md` for the full design document covering A1 and A2.

### Track A: Language surface

The foundation for writing real programs in Expo. `String`, `Binary`, and `Bits` are three distinct types with explicit conversion between them. `Binary` and `Bits` use `<<>>` syntax for construction and pattern matching, compiled to native shift-and-mask operations. `String` provides codepoint-aware text methods. All design decisions are finalized in `archive/20260323-BITSTRINGS.md`.

#### A1. Binary/bitstring literals

Expo's `<<>>` syntax with full bit-level precision. `<<>>` infers its type from the total bit count: byte-aligned totals produce `Binary`, non-byte-aligned produce `Bits`. Defaults are unsigned big-endian (matching Erlang). Sub-byte field extraction like `<<_::1, stream_id::31>>` compiles to native shift-and-mask code.

##### A1a. Lexer + Parser + AST -- done

- New tokens: `<<`, `>>`, `<>`, `::`, modifiers (`signed`, `unsigned`, `big`, `little`, `byte`)
- New AST nodes: `BinaryLiteral` (construction), `BinaryPattern` (match arms), `BinarySegment` (shared)
- Parser: `<<segments...>>` in expression position and pattern position
- Segment forms: `value`, `value::N`, `value::N byte`, `value: Type`
- `>>` ambiguity with nested generics resolved via `pending_token` + `expect_gt()` in the parser
- **Done**: `expo parse` produces correct AST for binary literals and patterns

##### A1b. Binary/Bits types + type checker -- done

- Register `Binary` and `Bits` as built-in types (distinct, no subtype relationship)
- Type checker validates: segment sizes, modifier combos, alignment, greedy-rest rules, catch-all requirement
- Binding type assignment (`::N` → Int, `::N byte` → Binary, `: Bool` → Bool, `: Int16` → Int16, etc.)
- `<<>>` type inference rule (byte-aligned total → Binary, non-byte-aligned → Bits)
- Literal overflow checking at compile time (e.g., `<<256>>` errors)
- **Done**: type checker validates binary literals and patterns with 8 unit tests

##### A1c. Codegen: binary construction -- done

- Emit LLVM IR for `<<segments...>>`: allocate buffer, pack segments with shifts/masks
- Handle default 8-bit segments, `::N` bit-width, `::N byte`, type-annotated segments (`: Float32`, `: Float64`, etc.)
- Modifiers: unsigned (default), big-endian (default), signed, little
- Length-prefixed heap layout: `[i64 byte_length][payload...]`, returned pointer targets payload
- Ownership tracking (`Owned` for `BinaryLiteral`) and scope-drop freeing at `ptr - 8`
- **Done**: byte-aligned binary construction compiles to LLVM IR with proper allocation and cleanup

##### A1d. Codegen: binary pattern matching -- done

- Emit LLVM IR for binary patterns in `match` arms
- Single length check per arm: `EQ` for fixed-size patterns, `UGE` for patterns with greedy rest
- Segment extraction via byte loads, shifts, and masks (inverse of A1c packing)
- Greedy rest capture (`rest: Binary`, `rest: Bits`): `malloc` + `memcpy` with length prefix, bound as `Owned`
- Literal matching: extracted values compared with `icmp eq`, `and`-chained into overall condition
- Variable binding: extracted values stored in alloca, inserted into scope with inferred type and `Unowned`
- Float segments: bitcast from integer to `f32`/`f64` for `: Float32` / `: Float64` bindings
- Integration with existing `compile_match` in `expo-codegen/src/control/patterns.rs`
- **Done**: binary pattern matching compiles to LLVM IR with lang tests for literal matching, variable extraction, greedy rest, multi-arm dispatch, endianness, and discard segments

##### A1e. Concatenation + Bitwise protocol -- done

- `<>` operator for `Binary <> Binary`, `Bits <> Bits`, `String <> String` (type-checked, no cross-type mixing)
- `Bitwise` protocol in `std.bitwise`: `band`, `bor`, `bxor`, `bnot`, `bsl`, `bsr` with `@moduledoc`/`@doc` annotations
- All integer types (`Int`, `Int8`, `Int16`, `Int32`, `UInt8`, `UInt16`, `UInt32`, `UInt64`) implement `Bitwise` via compiler intrinsics
- **Done**: concat and bitwise operations compile to LLVM IR with lang tests for string concat, binary concat, bitwise ops, and compile-fail type mismatch

#### A2. String stdlib + type conversions

`String`, `Binary`, and `Bits` are distinct types with explicit conversion. No `Char` type -- single-codepoint strings serve the same purpose (fractal design). String ranges and OR patterns enable clean pattern matching for lexers and parsers.

##### A2a. String/Binary/Bits distinct types + conversion -- done

- Unified `[i64 bit_length][payload...]` memory layout for `String`, `Binary`, and `Bits`
- `String.to_binary()` -- zero-cost widening (same pointer, always succeeds)
- `Binary.to_string()` → `Result<String, String>` (validates UTF-8 via `expo_utf8_validate` runtime function)
- `Binary.to_bits()` -- zero-cost widening (same pointer, always succeeds)
- `Bits.to_binary()` → `Result<Binary, String>` (validates byte alignment)
- Conversion methods declared as `panic("intrinsic")` in `std.kernel`, compiled to LLVM IR via intrinsic dispatch
- String literals in `<<>>` for construction (`memcpy`) and pattern matching (`memcmp`)
- Type function dispatch: `impl` blocks on all types (structs, enums, primitives) store functions in a unified `TypeContext.types` registry via `TypeInfo`, merged from stdlib, dispatched by both type checker and codegen
- **Done**: conversion round-trips work, `<<"hello">>` constructs a Binary from a string literal, all lang tests pass

##### A2b. String stdlib methods -- done

- Core: `length()`, `byte_length()`, `empty?()`, `at()`, `contains?()`, `starts_with?()`, `ends_with?()`
- Transform: `trim()`, `trim_start()`, `trim_end()`, `upcase()`, `downcase()`, `replace()`, `reverse()`
- Split/join: `split()`, `join()` (on List of String)
- Classification: `alpha?()`, `digit?()`, `whitespace?()` (on String -- checks all codepoints)
- Iteration: `codepoints()`, `graphemes()`
- Parsing: `to_int()`, `to_float()`
- **Done**: string-heavy programs compile and work (character iteration, splitting, searching, classification)
- **Note**: A2b ships ASCII-only case conversion (`upcase`/`downcase`) and codepoint-level iteration (`codepoints`/`graphemes`). Full Unicode support is deferred but required before production HTTP microservices handling internationalized content (JSON payloads, form input, non-ASCII URLs). Specifically:
  - **Unicode case mapping tables** (UCD CaseFolding/SpecialCasing) for proper `upcase()`/`downcase()` beyond ASCII (e.g., `ß` → `SS`, Turkish dotted i)
  - **Unicode grapheme cluster segmentation** (Grapheme_Cluster_Break property tables, ~100KB+ in runtime) for `graphemes()` to correctly handle emoji sequences, combining marks, etc.
  - **Unicode-aware classification** (`alpha?`, `digit?`, `whitespace?`) currently only handles ASCII ranges; full Unicode requires General_Category property lookups
  - These tables will be embedded in `expo-runtime` when the need arises.

##### A2c. OR patterns -- done

- OR patterns (`|`) in match arms -- multiple patterns sharing one arm body
- Same-binding constraint: all alternatives must bind the same set of variables with compatible types
- New AST variant: `Pattern::Or`
- Variable bindings inside OR patterns are currently disallowed (compile error); same-binding support is deferred
- Works in both `match` and `receive` arms
- **Done**: `match x 1 | 2 | 3 -> "small" ...` compiles and runs

##### A2d. Ranges -- deferred

- `..` range operator (always inclusive on both ends)
- Range patterns in match (`0..255`, `"a".."z"`) -- `Pattern::Range` AST node, codegen as `>= start && <= end`
- `1..10` as expression sugar for range construction
- String ranges ordered by codepoint value, endpoints must be single-codepoint string literals
- **Done when**: `for i in 1..10` iterates, `match c "a".."z" -> ...` compiles and runs, lexer-style `"a".."z" | "A".."Z" | "_" -> ...` works (combined with A2c)
- **Deferred**: the current `Range{start: Int, stop: Int}` struct is a placeholder that works for `slice()` but isn't the right foundation for the `..` operator. Open design questions that need resolution before shipping:
  - **Generic vs integer-only**: `Range<T>` would be type-safe but `Range<String>` is semantically wrong for multi-codepoint strings. Expo's "no Char type" design means single-codepoint strings serve as characters, but the type system can't enforce single-codepoint at the `Range<String>` level.
  - **String range iteration**: `for c in "a".."z"` should yield single-codepoint strings (not integers), but integer ranges and string ranges have fundamentally different stepping behavior. Needs intrinsics (`codepoint_value`, `from_codepoint`) or a different approach.
  - **Relationship to Enumeration**: should ranges implement `Enumeration<T>`, or should `for` loops recognize `..` as compiler-native syntax with optimized codegen (no struct allocation)?
  - The lexer port (A4) can proceed without ranges by using OR patterns for character matching. Ranges must be resolved before v1.0.
- **Not blocking**: A3 and A4 do not depend on ranges.

#### A3. File I/O + project system

Prerequisites for the lexer port (A4). File I/O lets Expo programs read source files; the project system lets the toolchain manage multi-module builds and tests.

##### A3a. File I/O -- done

Minimal file I/O via runtime intrinsics -- just enough to read and write files from Expo code.

- `std.fd` -- `Fd` type wrapping an OS file descriptor (an integer). `read`, `write`, `close` as runtime intrinsics. Also contains `File` struct wrapping `Fd`.
- `File.open(path) -> Result<File, String>`, `File.read(path) -> Result<String, String>` (convenience for read-entire-file), `File.close(move self)`. Move semantics ensure single ownership of file handles.
- `std.io` -- deferred to A3b (needs module namespacing from the project system for `io.puts()` style calls).
- **Done when**: an Expo program can read a file from disk and print its contents

##### A3b. Project system -- done

`project.expo` config file and unified module resolution. The project file defines `name`, `version`, `src` dirs, and `entry` module. The project name doubles as the module namespace prefix (`import my_app.server` → resolves `src/server.expo`).

- `project.expo` uses `Project{...}` struct literal format, parsed by the existing expression parser
- `expo build`, `expo run`, and `expo check` detect `project.expo` when invoked with no file argument
- Stdlib sources moved from `expo-typecheck` to a standalone `expo-stdlib` crate with fully qualified module names (`std.kernel`, `std.list`, etc.)
- All stdlib modules are auto-imported -- inserted into the module graph before user modules, resolved through the same pipeline
- Unified resolver dispatches by namespace: project name prefix → `src/` dirs, `std` prefix → embedded sources
- Single-file mode (`expo run foo.expo`) is backward compatible and unchanged
- **Done**: `expo build` and `expo run` work with a `project.expo` file; stdlib resolved through the same mechanism as project modules

##### A3c. Test runner

`@test` annotated functions with `expo test` to discover and run them. Depends on the project system (A3b) for test file discovery.

- **Done when**: `expo test` discovers and runs `@test` functions in a project with a `project.expo` file

#### A4. Lexer port (validation milestone)

Write the Expo lexer in Expo, compiled by the Rust bootstrap. Validate by comparing token output against the Rust lexer for all test files. This exercises binary pattern matching, string processing, enums, pattern matching, lists, structs, and file I/O. This is validation, not self-hosting -- the Rust compiler remains the real compiler.

- **Done when**: the Expo-written lexer produces identical token output to the Rust lexer for all `.expo` test files

### Track B: Runtime maturity

The foundation for Expo's concurrency promise. B1–B3 form a dependency chain (all complete). B4 is fully independent and can be tackled at any time.

#### B1. Union types -- done

`A | B` as anonymous enums -- a general-purpose type system feature, not just for mailboxes.

- **Implemented**: parsing (`A | B | C` type expressions), `type Name = ...` declarations, `Type::Union` with canonical constructor (sorted, deduped, flattened), widening coercion (`Post` assignable to `Post | Comment | Ad`), exhaustiveness checking for match on union subjects, named union aliases with collision checking. All integrated into the formatter, LSP, and editor extensions.
- **Implemented**: codegen -- tagged union representation (`{ i8 tag, [N x i8] payload }`) reusing enum infrastructure, widening coercion at assignment/call/return sites via coercion map, `match` with wildcard/binding patterns on union-typed values.
- Use cases: process mailbox typing (`Process<ServerMsg | LibResult>`), heterogeneous collections (`List<Post | Comment | Ad>`), error type composition (`Result<User, ValidationError | DatabaseError>`)
- **Implemented**: typed binding patterns in match arms (`p: Post -> p.title`) -- matches a union member by type and binds the unwrapped value. Exhaustiveness checking counts typed bindings. LSP hover/go-to-definition works on type names in patterns.
- **Remaining**: variant name collision resolution, protocol interaction, struct destructuring in match arms (deferred to irrefutable destructuring milestone).
- **Numeric tower as first dogfood**: `Int` could be defined in Expo as `type Int = Int8 | Int16 | Int32 | Int64` rather than hardcoded as a compiler primitive. The compiler recognizes that all variants are same-category integers and optimizes to the widest representation (no tag, implicit widening) -- the same behavior as today, but derived from the union definition. Extends naturally to `Float = Float32 | Float64` and user-defined aliases like `type SmallInt = Int8 | Int16`. This validates that the union type implementation is correct and general enough to express the language's own numeric relationships.

#### B2. Protocol-based process model -- done

Replaces the old `Process<M>` handle struct and caller-side annotations. Processes are now structs implementing a `Process<C, M, R>` protocol. See `archive/20260323-CONCURRENCY.md` for full design exploration.

- **`Process<C, M, R>` protocol** -- three type params: C (config to construct), M (messages while running), R (replies sent back). Two required methods (`new`, `handle`), two default impls (`run` receive loop, `child_spec` supervision bridge).
  - **Implemented**: protocol declaration, `impl Process<C, M, R> for T`, type checker extracts C/M/R from `protocol_impls`, `process_msg_type` set to `Pair<M, Option<ReplyTo<R>>>` envelope type. Default `run` method synthesized from protocol. `cast` wraps in `Pair<msg, Option.None>`, `call` wraps in `Pair<msg, Option.Some(ReplyTo)>` with `expo_rt_self()` for caller PID.
- **Default protocol implementations** -- new language feature. Protocols can provide default method bodies (like Rust default trait methods, Swift protocol extensions). Motivated by the `run` loop but useful throughout the language (`Display` with default `to_string`, `Equality` with default `ne`).
  - **Implemented**: parser, AST (`ProtocolMethod.body`), type checker synthesis with full type parameter substitution (Self + protocol type params in body patterns and expressions), codegen declaration/definition, formatter, LSP traversal. Default `run` loop on `Process` receives `Pair<M, Option<ReplyTo<R>>>`, unpacks, calls `handle`, recurses. Post-merge synthesis pass for stdlib protocols.
  - **Known limitation**: protocol method name collisions -- if two protocols define a method with the same name, the second impl silently overwrites the first in the method table. Needs qualified dispatch or a diagnostic.
- **`Ref<M, R>`** -- the typed handle for `cast`/`call`. `spawn` returns `Ref<M, R>`. (`Pid` type-erased process ID deferred to Phase 4 supervision prerequisites.)
  - **Implemented**: `spawn T.new(config)` returns `Ref<M, R>` with M and R resolved from the Process impl. Bare function spawn (`spawn some_function`) is now a compile error. Runtime accepts initial process state via `expo_rt_spawn(fn_ptr, state_ptr, state_len)`.
  - **Implemented**: `cast`/`call` methods on `Ref<M, R>` -- declared in `std.kernel` as intrinsics, full codegen in `process.rs` (`cast` builds envelope and sends via `expo_rt_send`; `call` builds envelope with `ReplyTo<R>`, sends, calls `expo_rt_receive_timeout`, decodes reply into `Option<R>`).
- **`receive ... after` syntax** -- `receive` gains an optional `after timeout` clause for timed receives. No separate `receive_timeout` primitive. No arrow on the `after` clause. Used by `Ref.call` for call timeouts, and by processes needing periodic work (heartbeats, cache expiry).
  - **Implemented**: parser (`after` as receive-only stop token, not leaked into `match`/`cond`), type checker (timeout expression + body), codegen (calls `expo_rt_receive_timeout`, branches on null for timeout path), formatter, LSP traverse (patterns + guards in receive arms), grammar updated.
- **Done when**: a struct implementing `Process<C, M, R>` can be spawned, receive messages via `cast`/`call` with typed `Ref<M, R>`, and the default `run` loop works via default protocol impl. ✓ Complete -- default `run` works with pair envelope, `cast` and `call` codegen implemented end-to-end. `call` untested pending a proper runtime scheduler with blocking `receive`.

#### B3. Task (kernel struct) -- done

One-off async work using the existing process infrastructure. `Task` is a stdlib struct implementing `Process`, not a new language primitive.

- **`Task.async(fun: fn() -> R) -> Ref<(), R>`** -- spawns `Task.new(Task{work: fun})`, returns a typed `Ref` to the task process.
- **`Task.await(move reference: Ref<(), R>) -> Option<R>`** -- synchronous wait via `reference.call((), timeout_ms)` (timeout is fixed in the current stdlib impl).
- **`Task<R>`** implements `Process<Task<R>, (), R>` (config type is the same as process state) -- `run` executes the closure, `receive`s a `Pair<(), Option<ReplyTo<R>>>`, and replies with the result. `handle` is a no-op for the unit message.
- Fire-and-forget: call `Task.async` without awaiting.
- No new surface syntax -- uses `Process`, `spawn`, `Ref`, `call`, `receive`, and `match` only.
- **Done when**: `Task.async` + `Task.await` work for one-off async work ✓ (`tests/lang/task.expo`).

#### B4. Multi-threaded scheduler + I/O (independent)

Work-stealing M:N scheduler. I/O reactor (kqueue on macOS, epoll on Linux). Can start with a simple multi-threaded round-robin before optimizing to work-stealing.

**No dependencies on B1-B3 or Track A.** The `Process<C, M, R>` protocol and `spawn`/`receive` work identically regardless of how many OS threads the scheduler uses underneath.

- **Scheduler protocol** -- the runtime is defined as a protocol interface (`spawn_process`, `send_message`, `yield`, `park`/`wake`, `poll_io`), not a monolithic scheduler. The native runtime is one implementation; others (WASM, testing, embedded, debug) implement the same interface.
- **Container-aware thread count** -- detect cgroup CPU limits (`/sys/fs/cgroup/cpu.max` on cgroups v2) for scheduler thread count, not host CPU count. A pod with `resources.limits.cpu: 2` on a 96-core host should spawn 2 scheduler threads. Fall back to `available_parallelism` on bare metal.
- **Idle thread parking (default: no spin)** -- idle scheduler threads park on a condvar/futex when no work is available, consuming zero CPU. No busy-wait by default. Configurable via `EXPO_SCHEDULER_BUSYWAIT=none|short` environment variable: `none` (default, container-safe) parks immediately; `short` spins briefly before parking for ~1-5 microsecond lower steal latency on bare metal with dedicated cores. BEAM's `+sbwt short` default caused silent CFS quota burn in Kubernetes deployments -- Expo avoids this by defaulting to the container-safe behavior.
- **Graceful SIGTERM handling** -- K8s sends SIGTERM with a configurable grace period (default 30s). The scheduler stops accepting new spawns, drains in-flight processes, and exits cleanly. Processes that don't exit in time are killed on SIGKILL.
- Timer wheel for timeouts, intervals, and deadlines
- Process lifecycle manager (start, stop, crash detection)
- All functions can suspend; the runtime handles it -- no function coloring
- **System intrinsics via the runtime** -- `expo-runtime` is the gateway between Expo code and the OS. Beyond scheduling, it provides native functions for time (`expo_time_now_millis`), file I/O, random bytes, and other syscall-dependent operations. The compiler emits calls to these functions as intrinsics (same pattern as `spawn`/`send`/`receive`). Pure Expo types in the stdlib wrap them with ergonomic APIs (`DateTime.now()`, `File.read()`, etc.). This avoids a full C FFI while keeping system access centralized in one linked library. A general FFI for third-party native bindings is a later concern.
- **Done when**: 10,000 processes run concurrently with correct multi-threaded scheduling

### Key decisions

| Decision           | Recommendation                                                                                                                                                                                                                                     |
| ------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Distinct types     | `String`, `Binary`, and `Bits` are three distinct types with no subtype relationships. Explicit conversion between them: widening always succeeds (zero-cost), narrowing validates and returns `Result`. See `archive/20260323-BITSTRINGS.md`.     |
| No Char            | No dedicated character type. Single-codepoint `String` values serve the same purpose -- fractal design. String ranges (`"a".."z"`) and classification methods (`is_alpha?()`) work on String directly.                                             |
| Inclusive ranges   | One range operator (`..`), always inclusive on both ends. Pattern matching is the primary use case; numeric loops are rare in idiomatic Expo. `0..n-1` for the occasional exclusive case.                                                          |
| Erlang defaults    | Binary segments default to unsigned big-endian (network byte order). Matches Erlang and covers the primary use case: HTTP microservices and network protocol parsing.                                                                              |
| Bitwise protocol   | Bitwise operations are methods (`band`, `bor`, `bxor`, `bnot`, `bsl`, `bsr`) on a `Bitwise` protocol, not symbol operators. Frees `<<`/`>>` for binary literals and `&`/`\|`/`^` for other uses.                                                   |
| One primitive      | Processes are the sole concurrency primitive. `Task` is a kernel struct built on `Process<C, M, R>`, not a separate primitive. GenServer-like actor patterns are the `Process` protocol itself.                                                    |
| Process protocol   | `Process<C, M, R>` with three type params. Config (C) separates public args from private state via `new`. Fixed reply type (R) per process -- same as a service contract. Union types for heterogeneous replies.                                   |
| Scheduler protocol | Define the runtime as a protocol interface before implementing any backend. The native scheduler is the first implementation, not a special case. Enables WASM targets, test runtimes, and third-party custom runtimes without changing user code. |
| Native runtime     | A runtime library linked into the binary, not a VM. No bytecode, no GC. Similar to Go's runtime or Tokio, but with process lifecycle management.                                                                                                   |
| Typed mailboxes    | Processes declare message type M via protocol impl. `send` and `receive` are type-checked at compile time. Union types enable multi-source mailboxes (e.g., `PoolCmd \| ExitSignal`).                                                              |
| Validation target  | The lexer port (A4) validates the language surface without requiring external dependencies (no network, no database, no JSON). The Rust compiler remains authoritative; the Expo lexer is compiled by it.                                          |

### Risks

- **Union type complexity**: union types interact with generics, protocols, and pattern matching. Design carefully to avoid type system bloat.
- **Runtime complexity**: building a work-stealing scheduler with I/O integration is substantial engineering. Start with round-robin and single-threaded I/O, then scale up.
- **Scheduler protocol scope**: the protocol must be minimal enough that a single-threaded WASM backend can implement it, but expressive enough that the native M:N scheduler isn't constrained. Err on the side of too-minimal.
- **Default protocol impls**: new language feature with well-understood semantics but requires careful design of override rules, interaction with protocol conformance checking, and method resolution order.

---

## Phase 4: Reliability

Build on the working process runtime with production-grade reliability features. These are layered on top -- processes must work before they can be supervised or prioritized. Depends on Phase 3 Track B.

### Supervision prerequisites

Three features deferred from Phase 3 because their primary use cases are supervision constructs:

- **`Pid` type** -- type-erased process ID (raw integer). Used in `ExitSignal` (which carries the crashed process's pid), registries, and `Process.monitor`. Distinct from `Ref<M, R>` (typed handle).
- **Trait bounds on generics** -- `fn foo<T: Process<C, M, R>>(x: T)` needed for `child_spec` and generic process utilities. Parser currently only accepts bare `<T>`, needs `:` bound syntax. Touches parser, type checker, and codegen.
- **`copy` keyword** -- third parameter modifier alongside default borrow and `move`: `fn start(copy config: Config)`. Auto-clones at the call boundary. Primary use case: `child_spec` default impl captures `copy config` in a closure for supervisor restart. `PassMode::Copy` already exists for closure captures; this extends it to parameter declarations. See `archive/20260323-CONCURRENCY.md` for full design.

### Preemption and priority

- Compiler-inserted yield checks at function call preambles and loop back-edges
- Priority levels (`Low`, `Normal`, `High`) control process scheduling budget -- higher priority processes get more CPU time before yielding
- Processes default to `Normal` priority; configurable at spawn time
- **Done when**: a low-priority process yields to high-priority processes under load

### Supervision

The API design is largely settled (see `archive/20260323-CONCURRENCY.md`). Depends on the three prerequisites above. Implementation work:

- **`ExitSignal` struct** -- stdlib struct with `pid: Pid` and `reason: ExitReason`. Included in a process's M via union type (`type PoolMsg = PoolCmd | ExitSignal`). Type checker verifies M includes `ExitSignal` at `Process.monitor` call sites.
- **`Process.monitor(ref)`** -- static function, not a protocol method. Tells the runtime to send an `ExitSignal` to the caller's mailbox when the monitored process dies.
- **`ChildSpec` struct** -- holds `start: fn() -> Pid` and `strategy: RestartStrategy`. The closure captures `copy config` and calls `new` + `spawn`, enabling type-erased restart.
- **`child_spec` default impl on `Process`** -- produces a `ChildSpec` with `RestartStrategy.Permanent`. Processes override for custom restart strategies (transient, temporary).
- **`Supervisor` stdlib process** -- implements `Process`, holds `List<ChildSpec>`, monitors children via `ExitSignal`, restarts on death by re-calling the `start` closure. Restart strategies: `OneForOne`, `OneForAll`, `RestForOne`. Max-restarts-exceeded crashes the supervisor.
- **Application startup** -- `fn main` creates child specs via `Type.child_spec(config)`, passes to Supervisor, spawns it. No framework, no Application behavior -- main is just a function.
- Root supervisor crash exits the OS process (deterministic shutdown)
- Ownership ensures deterministic cleanup on process crash -- all owned memory is freed, no leaks
- **Done when**: a supervised process tree restarts crashed children correctly

### Process discovery

- **Runtime-level global registration** -- `Process.register(ref, "name")` and `Process.whereis<M, R>("name")` returning `Option<Ref<M, R>>`. Simple `name -> Pid` mapping. Good for well-known singletons.
- **Registry as stdlib process** -- typed `Registry` process for dynamic, scoped registries (worker pools, connection managers). Monitors entries via `ExitSignal`, auto-removes dead entries.
- **Done when**: processes can be registered by name and looked up with `Option<Ref<M, R>>`

### Shared data

- `shared_map` (stdlib concurrent hash map, needs a proper name) for shared caches across processes
- `put` moves values in (ownership transfer, no races)
- `get` borrows values out (zero-copy read access)
- `delete` removes and drops values
- Solves the two core problems of shared state: memory explosion from copying, and corruption from concurrent modification
- **Done when**: multiple processes read/write a `shared_map` without corruption

### Risks

- **Preemption yield-check overhead**: every function call and loop back-edge gets a yield check. Must be cheap (single counter decrement + branch). Profile to ensure overhead stays under 1-2%.
- **`shared_map` naming**: needs a proper name before 1.0. Candidates TBD.
- **Protocol naming for child specs**: config structs may implement a separate protocol (e.g., `Child`, `Service`) instead of `child_spec` living on `Process`. Naming unresolved -- mechanism settled.

---

## Phase 5: Stdlib + first-party packages

Build the remaining stdlib and the first-party package ecosystem. Binary/bitstring and string methods ship in Phase 3 A2. File I/O (`std.fd`, `std.file`, `std.io`) ships in Phase 3 A3a. This phase covers everything else needed to write real applications.

### Stdlib (ships with the compiler, always available)

Stdlib contains primitives that are as fundamental as integers — things the compiler or language runtime needs to function, or that virtually every program needs and whose API is stable for decades.

- `std.fd` -- shipped in Phase 3 A3a (basic `read`, `write`, `close`). Phase 5 adds any remaining operations.
- `std.file` -- shipped in Phase 3 A3a (basic `open`, `read`, `close`). Phase 5 adds `seek`, `write` to file, and other operations as needed.
- `std.mmap` -- `Mmap` struct for memory-mapped files. Wraps `mmap`/`munmap` syscalls. Maps a file directly into the process's address space — reads are pointer dereferences (zero copy), the OS manages paging data in/out. Essential for embedded databases (redb port), large file processing, and any workload where explicit `read` calls are too slow. `Mmap` is a move type; `close` unmaps. Runtime C shim wraps `mmap(fd, length, PROT_READ|PROT_WRITE, MAP_SHARED, ...)`.
- `std.io` -- `stdin`, `stdout`, `stderr` as `Fd` constants. `print` builtin dispatches through here.
- `time.DateTime`, `time.Duration` with `.now()`, `.timestamp_millis()`, `.from_secs()`
- `Display` protocol -- auto-derived string representations, `print()` dispatches through it

The litmus test: does the compiler or language runtime need it to function, or is it a stable capability every program needs with an API that won't evolve? If yes, stdlib. If the API surface will evolve (protocols, connection management, serialization formats), it's a first-party package.

- **Done when**: `config.expo` compiles (exercises strings, file reading, option handling, duration)

### Package manager

- `project.expo` extended with dependency declarations (minimal project system ships in Phase 3 A3)
- Dependency resolution: fetch from hosted sources (git URLs initially, registry/mirror possible long-term)
- Lock file generation for reproducible builds
- **Done when**: `project.expo` resolves dependencies and builds the project

### First-party packages (maintained by the Expo team, versioned independently)

These are high-quality, officially maintained, but not part of the compiler release cycle. Protocols and algorithms evolve on their own timeline. Networking lives here because the API surface evolves (QUIC, io_uring, TLS integration, connection pooling) — you don't want that locked into the stdlib release cycle.

- `net` -- networking primitives as submodules, one package, coordinated releases. Shared types (`IpAddr`, `SocketAddr`) used across submodules.
  - `net.tcp` -- `TcpListener` (bind + accept) and `TcpSocket` (connect + read + write + close). Both wrap `Fd` from stdlib. `TcpListener.accept()` returns a `TcpSocket` — same type for server and client connections. Socket setup uses C shims in the runtime (`socket`, `bind`, `listen`, `accept`, `connect`, `setsockopt`); read/write/close go through `std.fd`.
  - `net.udp` -- `UdpSocket` with `bind`, `send_to`, `recv_from`. Datagram-oriented, no connections. Independent from TCP — different semantics, different API shape.
  - `net.tls` -- `TlsSocket` wrapping a `TcpSocket` with encryption. `TlsSocket.wrap(move socket: TcpSocket, config: TlsConfig) -> Result<TlsSocket, TlsError>`. Same `read`/`write`/`close` interface. Thin wrapper over system TLS library (LibreSSL/OpenSSL/BoringSSL via C FFI). Programs that only import `net.tcp` don't pull in TLS dependencies.
- `http` -- HTTP server and client built on `net.tcp` / `net.tls`. Request parsing, routing, response building, middleware. Server spawns a process per connection using `Process<C, M, R>`. Binary pattern matching for protocol parsing. `http.client` for outbound requests.
- `websocket` -- WebSocket server and client built on `http` (upgrade handshake) and `net.tcp` (framed message transport). Each WebSocket connection is a process — natural fit for Expo's concurrency model. Frame parsing via binary pattern matching.
- `json` -- `JsonValue` enum, parser, serializer, convenience methods (`as_string()`, `as_int()`, etc.). No auto-derive or compiler magic; users write `from_json`/`to_json` functions in impl blocks. Decoder combinator API for API input boundaries with error accumulation (all field errors collected, not just the first).
- Crypto: hashing, random bytes (thin wrapper over libsodium or similar)
- Structured logging
- MessagePack serialization
- UUID generation, regex, URL parsing
- **Done when**: `handlers.expo` compiles using stdlib + first-party packages -- it exercises HTTP, JSON, crypto, logging, and UUID generation

### Approach

Implement natively in Expo wherever possible. Use thin C FFI only for security-critical crypto and performance-critical parsing.

---

## Phase 6: Tooling maturity

### Documentation -- started

- ~~`expo doc` -- generates static HTML documentation from `@doc` and `@moduledoc` annotations~~
- ~~Markdown rendering in doc strings (via pulldown-cmark)~~
- ~~`@doc false` and `@moduledoc false` to exclude items from docs~~
- ~~Recursive directory input with dotted module names (e.g. `src/what/util.expo` → `what.util`)~~
- ~~Global sidebar navigation across all module pages~~
- ~~Askama templates for HTML generation~~
- ~~Brand-themed output (burnt orange + warm charcoal, Source Sans 3 / Source Code Pro typography)~~
- Doctest support: code examples in `@doc` strings are compiled and run as tests
- Prose pages from `docs/*.md` alongside API reference
- Client-side fuzzy search
- Clickable type cross-references in signatures
- **Done when**: `expo doc src/` generates browsable, searchable HTML for a multi-module project

### Language server (LSP) -- started

- ~~Real-time diagnostics (parse errors + type-check warnings/errors) on every keystroke~~
- ~~Document formatting via LSP (`textDocument/formatting`)~~
- ~~VSCode/Cursor extension integration (LSP client over stdio)~~
- ~~Go-to-definition for functions, structs, enums, and imports (jumps to module file)~~
- ~~Hover showing type signatures, `@doc`, and `@moduledoc` for imports~~
- ~~Restart Language Server command~~
- Autocomplete for module names, function names, struct fields
- Inline type hints for inferred types
- Multi-module resolution (cross-file diagnostics)
- **Done when**: editing `.expo` files in Cursor shows real-time errors and supports go-to-definition

### Interactive shell (REPL)

- `expo shell` -- evaluate expressions and statements interactively, one at a time
- `expo shell -S .` -- load a project so you can call your functions, inspect types, and explore live
- Inline documentation: `h module.function` pulls from `@doc` annotations
- Tab completion for module names, functions, and variables in scope
- Backend: LLVM JIT (via inkwell `ExecutionEngine`) initially; Cranelift JIT long-term for faster response
- **Done when**: `expo shell -S .` loads a multi-module project and you can call functions, inspect results, and read docs interactively

---

## Phase 7: Self-hosting

Rewrite the Expo compiler in Expo. The lexer port from Phase 3 A4 (validation) provides a head start -- it was compiled by the Rust bootstrap to validate the language, but now gets promoted to the real compiler.

### Port the parser

- Rewrite the parser from Rust to Expo (the lexer is already ported from Phase 3)
- This is a stress test of the language for non-trivial recursive descent code
- Expect to discover language shortcomings -- feed them back into design
- **Done when**: the Expo-written parser can parse all `.expo` files identically to the Rust parser

### Introduce ExpoIR and the codegen backend protocol

- Split `expo-codegen` into two stages: lowering (TypedAST → ExpoIR) and emission (ExpoIR → target output)
- ExpoIR is a flat, lowered representation -- monomorphized, closures desugared, drops inserted. Simple enough that writing a new backend is a tractable project.
- Define `CodeEmitter` as an Expo protocol. The LLVM backend is `impl CodeEmitter for LlvmEmitter`. Cranelift, WASM, and C backends implement the same interface.
- Publish `expo-ir` and the backend protocol as packages so third parties can build custom backends.
- **Done when**: the LLVM backend works through ExpoIR with no regressions, and a second backend (Cranelift for the REPL) compiles a non-trivial program.

### Port type checking and codegen

- Rewrite semantic analysis, type checker, and LLVM codegen in Expo
- LLVM bindings via C FFI (Expo calling into the LLVM C API)
- **Done when**: the Expo-written compiler can compile itself (the compiler compiles itself)

### Retire the bootstrap

- Run the full test suite through the self-hosted compiler
- Fix any remaining differences between Rust bootstrap output and Expo self-hosted output
- The Rust bootstrap is now only needed for bootstrapping from scratch
- **Done when**: `expo build` using the self-hosted compiler produces identical binaries to the Rust bootstrap for all test programs

---

## Phase 8: Validation

### Compile auth-manager-expo for real

- Take the 17 `.expo` pseudocode files in this repo and make them compile and run as an actual service
- Fix any gaps between the pseudocode and what the compiler actually supports
- Run the auth-manager test suite (ported from the Rust version)
- Benchmark against the Rust original: binary size, memory usage, request latency, startup time
- **Done when**: auth-manager-expo runs in production handling real traffic

### Build a second project

- Build a CLI tool or a different microservice in Expo from scratch
- This validates that the language isn't just shaped around one project
- **Done when**: a second non-trivial project compiles, runs, and feels natural to write

---

## Future: `command` construct (post-v1)

A language-native `command` keyword for typed, composable pipelines -- inspired by the Commandex library pattern but with compile-time guarantees.

```expo
command RegisterUser
  param email: String
  param password: String

  step hash_password -> password_hash: String
    Crypto.hash_sha256(password)
  end

  step create_user -> user: User
    User.create(email: email, password_hash: password_hash)
  end
end
```

What the compiler provides that libraries can't:

- **Step-ordered type safety** -- `password_hash` only accessible in steps after `hash_password`
- **Exhaustive data flow** -- every data field verified set before read
- **Automatic error types** -- generated from `halt` calls
- **Composability** -- commands can be used as steps in other commands
- **Zero overhead** -- compiles to sequential function calls

Commands live inside modules alongside `fn`, `struct`, and `enum` -- not a separate paradigm, just another construct for a common shape of backend logic.

---

## Future: Arena blocks (post-v1)

Bulk-allocation regions where many objects share a lifetime and are freed together at scope exit. Useful for request-scoped allocation in web servers, parsing large documents, and other workloads with many short-lived allocations.

```expo
arena
  nodes = parse_document(input)
  result = transform(nodes)
  result.clone()
end
# everything allocated inside the arena is freed here
```

Deferred because the design depends on runtime decisions not yet made:

- **Per-process heaps**: if each process gets its own allocator, process exit is already a bulk-free boundary -- arenas may be redundant for the most common case.
- **Multi-threaded scheduler**: thread-local vs. shared arenas have very different implementation shapes.
- **LLVM allocation patterns**: real-world profiling may reveal that LLVM's optimizer eliminates enough allocations to reduce the need for manual arena control.

The `arena...end` syntax is already parsed and type-checked. Codegen is deferred until allocation pressure is observed in real Expo programs.

---

## Future: Folded multiline strings (post-v1)

A second multiline string type where newlines become spaces and blank lines become `\n` -- for long log messages, error messages, and other prose where source-level wrapping shouldn't produce newlines in the output.

```expo
log.info(???
  User #{user.id} authenticated
  via #{method} and was granted
  access to #{resource}
  ???)
```

Would produce: `User 42 authenticated via oauth and was granted access to /admin`

Syntax undecided -- candidates include `~"""`, `'''`, or something else entirely. The current workaround is a single-line string (the formatter leaves string literals alone regardless of length).

---

## Design exploration

Active design discussions about the type system, code organization, and functional programming patterns. These inform future work but are not committed changes.

### Ownership design decisions (decided, implemented)

- **`move self`**: `self` follows the same rules as any other parameter -- borrows by default (read-only), `move self` for ownership transfer. Mutating impl functions take `move self` and return the modified value: `list = list.append(42).append(37)`. No special "method" semantics -- impl functions with `self` are just functions with dot-call syntax.
- **Signature-only `move`**: `move` only appears in the function/closure signature, never at the call site. The compiler infers moves from the callee's signature. Consistent with "no magic" -- the function's type signature is the contract, and the compiler fully enforces use-after-move.
- **Functions = closures**: identical ownership rules. `fn (T) -> U` borrows T (default), `fn (move T) -> U` takes ownership. `map`/`then` use `fn (move T) -> U` since the closure receives the unwrapped owned value.
- **Closure captures**: closures capture variables from the enclosing scope. Copy types (primitives) are duplicated; non-copy types (structs, enums) are moved, making the original unusable. Captured closures use a `{fn_ptr, env_ptr}` fat pointer ABI with heap-allocated environment structs that are automatically freed when the closure goes out of scope.
- **`ref T` removed**: removed from the toolchain. Redundant with borrow-by-default params, unsafe in return position without lifetime tracking. Can be re-added if a concrete use case emerges.

### Short closures

- Short closure syntax (`x -> expr`) is parsed, type-checked with full capture analysis, compiled, and supports context-driven parameter type inference at inline call sites. `option.map(x -> x + 1)` infers `x: Int` from `Option<Int>`. Works for direct calls, method calls, and generic methods.
- Remaining gap: variable-assigned short closures (`add_ten = x -> x + 10`) don't receive an expected type at the assignment site, so params remain `Unknown`. Workaround: use inline at the call site, or add explicit type annotations.

### `Self` type expression (implemented)

- **`Self`** resolves to the concrete implementing type inside `protocol` declarations and `impl` blocks.
- In protocol declarations, `Self` is abstract -- it becomes a type variable that the compiler substitutes with the concrete type when checking `impl` blocks.
- In `impl` blocks, `Self` resolves to the target type (e.g., `impl ListLiteral<T> for List<T>` → `Self` = `List<T>`).
- Works with generics and monomorphization. Modeled after Rust's `Self` and Swift's `Self`.
- Syntax highlighting and formatter support included.

### `impl` and protocols (decided, implemented)

- **Decided**: `protocol` keyword defines behavioral contracts. `impl Protocol for Type` for conformance. Bare `impl Type` survives for direct method attachment.
- **Implemented**: protocol declarations with function signatures, `impl Protocol for Type` blocks with completeness and signature validation, `priv fn` helpers in impl blocks, `@doc` on protocol declarations.
- **Decided**: static dispatch via monomorphization -- no vtables, no dynamic dispatch. Consistent with the existing generic compilation model.
- **Implemented**: `impl` blocks on all types (structs, enums, primitives) -- functions stored in a unified `TypeContext.types` registry via `TypeInfo { functions, type_params, kind: TypeKind, span }`. `TypeKind` discriminates `Struct` (with fields), `Enum` (with variants), and `Primitive`. Previously, structs, enums, and primitives each had separate storage (`StructInfo`, `EnumInfo`, `primitive_methods`); the unified registry eliminates duplicated lookup/dispatch logic across the type checker and codegen. Used for conversion intrinsics, protocol methods, user-defined functions, and static/instance dispatch.
- **Open**: trait bounds on generic type parameters (`fn foo<T: Display>(x: T)`) -- requires protocols, now unblocked.
- **Open**: whether bare `impl Type` eventually migrates to inline functions in type bodies, or both coexist permanently.

### Struct field defaults and trailing keyword syntax (open)

- **Open**: struct fields with default values (`struct Opts timeout: Int = 5000 end`).
  Enables partial construction — only override the fields you care about.
- **Open**: trailing keyword syntax at call sites as sugar for opts struct
  construction. `pid.call(msg, timeout: 30000)` desugars to
  `pid.call(msg, CallOpts{timeout: 30000})`. Combined with struct field defaults,
  this gives typed, compile-checked keyword arguments — Elixir's `Keyword.t()`
  opts pattern but with type safety (invalid keys and wrong value types are
  compile errors).
- **Motivating use case**: `Ref.call` needs an optional timeout with a sensible
  default. Without this feature, every call site must specify the timeout
  explicitly. Useful far beyond concurrency — any function with optional
  configuration benefits (HTTP clients, query builders, formatters).

### Type system philosophy

- **Decided**: enums and structs have equal capabilities -- fractal design where the same features available to `Option<T>` (a built-in enum) are available to any user-defined enum. No two-tier type system.
- **Decided**: if types get inline functions, both structs and enums support them. An enum is semantically a one-field struct with a tagged union type -- the distinction is surface syntax, not fundamental.
- **Open**: whether inline functions in type bodies are restricted to `self`-taking functions only (instance methods), or also allow non-`self` functions (static/factory -- which makes the type act as a namespace).

### Namespace unification: modules as pseudotypes (exploration)

- **Context**: the `TypeInfo` refactor unified struct/enum/primitive function storage into a single `ctx.types` registry. This eliminates duplicated dispatch logic but still treats module-qualified calls (`Module.function()`) differently from type-qualified calls (`Type.function()`).
- **Observation**: a module file is structurally similar to a type -- it defines imports, structs, enums, and functions. A type (struct/enum/primitive) defines functions (and possibly nested types in the future). Both are namespaces that own functions.
- **Design exploration**: treat modules as pseudotypes in the same `TypeInfo` registry, with a new `TypeKind::Module` variant. Module-qualified calls (`Http.get(url)`) and static type calls (`Option.some(x)`) would resolve through the same lookup path: `ctx.types.get(qualifier).and_then(|ti| ti.functions.get(name))`.
- **Benefits**: single resolution path for all qualified calls, recursive namespace model (modules contain types, types contain functions), forward-compatible with nested types or module re-exports.
- **Risks**: modules currently carry full `TypeContext` in `imported_modules` (with their own types, functions, etc.), which is richer than `TypeInfo`. Flattening this into `TypeInfo` may lose expressiveness. The `imported_modules` map also handles transitive imports and visibility scoping.
- **Status**: not planned for immediate implementation. The current `TypeInfo` registry is designed to be forward-compatible -- adding `TypeKind::Module` later would not require re-architecture. Document here for future reference.

### FP and chaining vs `?` operator (decided, implemented)

- **Decided**: no `?` operator -- removed from the toolchain. Hidden control flow violates the "no magic" principle -- the reader can't see that a function might return early without inspecting every line for `?`. Error handling uses explicit functions instead.
- **Decided**: no `?.` optional chaining (Swift-style). Would make `Option` a privileged type, breaking fractal design -- user-defined sum types wouldn't get the same syntax.
- **Decided**: `map`, `then`, `or` as the chaining API for `Option` and `Result`. `map` transforms the inner value (closure returns plain value). `then` chains fallible operations (closure returns `Option`/`Result`). `or` provides a lazy fallback. Approachable naming -- plain English, no `and_then`/`flat_map`/`unwrap_or`.
- `or` is implicitly lazy (compiler evaluates the argument only if needed, like `||`). No separate `or_else`.
- Compiler guidance when `map` is used where `then` is needed (or vice versa).
- **Decided**: no pipe operator (`|>`). Dot-call chaining with `move self` functions covers the same use case. The `command` construct (post-v1) will handle complex sequential data flow with stronger guarantees.
- `map`/`then` ship in the stdlib using block closures or short closures. Context-driven param inference allows `opt.map(x -> x + 1)` without type annotations at inline call sites.

### Stdlib design (implemented)

- **Done**: `std.kernel` for core types (`Option<T>`, `Result<T, E>`, `Pair<A, B>`), auto-imported into every module. Embedded in the compiler via `include_str!`, parsed at startup, types merged into every module's context before type checking.
- Rule: "stdlib = always available, packages = explicit import." All `std.*` modules are auto-imported. As the stdlib grows, types split into separate modules (`std.option`, `std.string`, `std.list`) for documentation and organization, but all remain auto-imported.
- Option/Result API: `unwrap` (panics on failure), `or` (lazy fallback), `some?`/`none?` (Option), `ok?`/`err?` (Result), `map` (transform inner value), `then` (flat map / chain fallible operations).
- No `map_err` yet. No `or_else` -- `or` is implicitly lazy.
- Monomorphization ensures zero binary bloat for unused stdlib types. Only instantiations that are actually called get compiled.

### `Display` protocol and `print`

- **Planned**: a `Display` protocol that types implement to provide a string representation. `print()` dispatches through `Display` rather than hardcoding printf format specifiers per LLVM type.
- **Auto-derived**: all structs and enums get a default `impl Display` generated by the compiler. Enums print as `VariantName` (unit) or `VariantName(value)` (tuple payload). Structs print as `TypeName{field: value, ...}`. Users can override with their own `impl Display for MyType`.
- **Unblocked**: protocol system is now implemented -- `Display` can be built.
- **Current limitation**: `print()` only supports primitives (`Int`, `Float`, `Bool`, `String`). Printing a struct or enum value is a compile error. Workaround: match on enum variants and print primitive values, or use string interpolation with primitive fields.

### ExpoIR and codegen backend protocol

- **Planned**: introduce an intermediate representation (`expo-ir`) between the type checker and codegen. The IR is a lowered, flat representation -- no generics (already monomorphized), no closures (already desugared to structs + function pointers), no high-level control flow (already lowered to branches). Just functions, calls, loads, stores, branches.
- **Motivation**: the current `expo-codegen` crate mixes two concerns -- lowering (closure desugaring, monomorphization, drop insertion) and emission (inkwell LLVM calls). Separating them creates a clean interface for multiple codegen backends.
- **Backend protocol**: codegen backends implement a `CodeEmitter` protocol against ExpoIR. The LLVM backend (current) is the first implementation, not a special case. Other backends become possible: Cranelift (fast compilation for the REPL), direct WASM emission (smaller output for edge), C emission (maximum portability), or an interpreter (scripting, hot-reload).
- **Compiler pipeline**: `Source → AST → TypedAST → ExpoIR → [CodeEmitter backend] → output`. Lowering happens once; backends only handle "emit a function call" and "emit a branch," not "figure out how closures capture variables."
- **Public API**: ExpoIR and the backend protocol would be published as packages after self-hosting, enabling third-party codegen backends. During bootstrap, they're Rust crates wrapping inkwell.
- **Build-time selection**: `project.expo` or `expo build --backend cranelift` selects the backend. One backend per binary. The compiler monomorphizes all emitter calls against the selected implementation -- no vtable overhead.
- **Timing**: the IR split is Phase 7 (self-hosting) work. The current crate boundaries (codegen depends on ast + typecheck, clean downward dependencies) already support this separation. Keeping `expo-codegen` internals organized now avoids a painful refactor later.

### Literal protocols

- **Concept**: all literal syntax (`42`, `"hello"`, `[...]`, `[k:v]`, `(a,b)`) backed by protocols, not special-cased types. Any type can opt into literal construction by implementing the protocol.
- **Protocol family**: `IntLiteral`, `FloatLiteral`, `StringLiteral`, `ListLiteral<T>`, `MapLiteral<K,V>`, `PairLiteral<A,B>`.
- **Default types**: `Int`, `Float`, `String`, `List<T>`, `Map<K,V>`, `Pair<A,B>` when no type annotation is present.
- **Infallible**: literal protocols return `Self`, not `Result`. Fallible parsing (e.g. from untrusted input) uses regular functions that return `Result`.
- **Pair syntax**: `(a, b)` may return via `PairLiteral<A, B>` -- only pairs (arity 2). 3+ values use named structs.
- **Implemented**: `ListLiteral<T>` with `from_list(move list: List<T>) -> Self` -- `List<T>` and `Set<T>` implement it. Defined in `std.kernel`.
- **Implemented**: `MapLiteral<K, V>` with `from_map(move map: Map<K, V>) -> Self` -- `Map<K, V>` implements it as identity. `[key: value]` syntax and `[:]` for empty maps.
- **Planned**: `IntLiteral`, `FloatLiteral` (enables custom `Decimal` type from float literals), `StringLiteral`, `PairLiteral<A,B>`.
- **Fractal design**: user-defined types and built-in types have identical access to literal syntax. No two-tier system.

---

## Summary

### Done

| Phase     | Milestone                                                                    |
| --------- | ---------------------------------------------------------------------------- |
| Bootstrap | Lexer, parser, type system, LLVM codegen -- native binaries from Expo source |
| Tooling   | Formatter, `expo run`, VSCode extension, LSP, documentation generator        |
| Core      | Generics, ownership, protocols, closures, collections, processes             |

For detailed build history, see [archive/20260318-ROADMAP.md](archive/20260318-ROADMAP.md).

### Remaining

| Phase        | Milestone                                                                                                                                                                                                                                                                                             |
| ------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Surface (3A) | ~~A1a lexer/parser/AST~~, ~~A1b types+checker~~, ~~A1c codegen construction~~, ~~A1d codegen patterns~~, ~~A1e concat+bitwise~~, ~~A2a type conversion~~, ~~A2b string stdlib~~, ~~A2c OR patterns~~, A2d ranges (deferred), ~~A3a file I/O~~, ~~A3b project system~~, A3c test runner, A4 lexer port |
| Runtime (3B) | ~~Union types~~, ~~`Process<C,M,R>` protocol~~, ~~`Ref<M,R>`~~, ~~`receive...after`~~, ~~default impls~~, ~~`cast`/`call` pair envelope~~, ~~`Task`~~, scheduler + I/O                                                                                                                                |
| Reliability  | `Pid`, trait bounds, `copy` keyword, supervision (`ChildSpec`, `ExitSignal`, `Process.monitor`), process discovery, preemption, `shared_map`                                                                                                                                                          |
| Stdlib       | File I/O, time, `Display` protocol, package manager, first-party packages                                                                                                                                                                                                                             |
| Tooling      | Documentation (doctests, search), LSP (autocomplete, type hints), REPL                                                                                                                                                                                                                                |
| Self-host    | Parser in Expo, ExpoIR + backend protocol, full compiler, retire bootstrap                                                                                                                                                                                                                            |
| Validation   | auth-manager-expo runs for real, second project                                                                                                                                                                                                                                                       |

---

## Guiding principles

- **Readability over cleverness.** Every language feature decision is judged by: "can a reader understand this line without reading any other line?"
- **Error messages are a feature.** Invest in them from month 1. A confusing error message is a bug.
- **The example codebase is the test suite.** Every phase targets compiling a specific `.expo` file from this repo. The language grows toward real code, not toy examples.
- **AI writes, humans read.** The language is concise and readable because that's good design -- Ruby over Java, signal density over ceremony. Every line should carry meaning without boilerplate. AI-friendliness is a natural consequence of those values, not the driver.
- **No magic.** Explicit is better than implicit. If a feature requires the reader to know something they can't see on screen, it's wrong for Expo.
- **No macros.** Bake common patterns into the language as native constructs instead. Macros create invisible control flow, fragment the language per-codebase, and are hostile to AI tooling. Every Expo codebase should read the same way.
- **Approachable by default.** A beginner should be able to write their first program without knowing about ownership, processes, or type annotations. Advanced features reveal themselves as you grow -- you learn `move` when you hit a performance problem, processes when you need concurrency, supervision when you build a stateful service. The language has a Ruby-shaped learning curve backed by Rust-grade safety.
- **Built to last.** Every design decision passes the decades test -- will this still make sense in 20 years? Features tied to today's trends are packages, not language constructs. The stdlib only contains primitives that are as fundamental as integers and files.
- **Stable after 1.0.** The language spec locks at 1.0. Post-1.0 changes are additive only -- new features, never removals or breaking changes. No edition system. If something truly needs to break (hopefully a decade+ out), it's a clean 2.0 with migration tooling -- one decisive move, not death by a thousand editions.
