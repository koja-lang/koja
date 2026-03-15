# Expo Language Roadmap

Solo developer + AI assistance. Bootstrap in Rust, self-host in Expo.

---

## Current state

### Compiler

A 9-crate Rust workspace that compiles Expo source to native binaries via LLVM:

- `expo-ast` -- tokens, spans, AST node definitions
- `expo-lexer` -- custom tokenizer
- `expo-parser` -- recursive descent parser (Pratt precedence for expressions)
- `expo-typecheck` -- type inference and semantic analysis
- `expo-codegen` -- LLVM IR generation via `inkwell`
- `expo-fmt` -- opinionated code formatter
- `expo-doc` -- HTML documentation generator (askama templates, pulldown-cmark)
- `expo-driver` -- CLI binary (`expo`)
- `expo-lsp` -- language server (diagnostics, formatting, hover, go-to-definition)

### CLI

Seven commands: `expo build`, `expo run`, `expo check`, `expo format`, `expo doc`, `expo lex`, `expo parse`. All commands support multi-module projects.

### What compiles to native binaries today

- Multi-module imports (including qualified calls like `math.add()`)
- Functions (`fn`/`priv fn`)
- Constants (`const`)
- Structs
- Enums
- Impl blocks and methods (`self`)
- Generic functions with monomorphization
- Generic structs with monomorphization
- `if`/`else`
- `while`
- `loop` and `break`
- `return`
- `match`
- `cond`
- Ternary (`? :`)
- Pipe operator (`|>`)
- Compound assignment (`+=`, `-=`, `*=`, `/=`)
- String interpolation
- Closures (non-capturing, block form)
- `print` builtin
- Primitives: `i32`, `i64`, `f32`, `f64`, `bool`, `string`

### Parsed and type-checked but NOT yet in codegen

- `for` loops
- `arena` blocks
- `await`/`receive`/`spawn`
- Try operator (`?`)
- `ref<T>`
- Lists
- Generic enums, methods on generic types, trait bounds

### Design notes

- **No tuples**: Expo does not have anonymous tuple syntax. `(a, b)` is grouping only. For multiple return values, use a struct. After generics land, `Pair<A, B>` (with `.first` / `.second`) will be available in the stdlib for lightweight two-value cases. 3+ values should always be a struct.
- **`()` as the unit expression**: `()` is a "do-nothing" expression (empty closure that runs and returns nothing). Use `else -> ()` in `cond` for side-effect-only fallthrough.
- **Closures (Phase 1)**: Block closures only with explicit types and parens: `fn (a: i32, b: i32) -> i32 ... end`. Mirrors function signature syntax. Inline closures (`x -> expr`) are parsed but not compiled -- they land in Phase 2 with type inference and generics.
- **No private modules**: Files are modules, and all modules are importable. Access control lives at the function level (`priv fn`), not the module level. Use `@moduledoc false` to signal "internal, don't depend on this" -- a documentation-level convention, not a compiler wall. This matches Elixir's approach and avoids the complexity of Rust's `pub(crate)` or Go's `internal/` directory enforcement.
- **Planned: PascalCase primitives and type simplification**: Primitives will be renamed from `i32`/`i64`/`f32`/`f64`/`bool`/`string` to PascalCase: `Int` (64-bit default), `Int32`, `Float` (64-bit IEEE default), `Float32`, `Bool`, `String`. This makes user-defined types (`Pair`, `User`) and language types (`Int`, `String`) visually uniform. `Decimal` will ship in the stdlib as an exact-arithmetic type for financial/business logic, sitting alongside the primitives with no visual distinction. Rename should happen before `Option<T>`/`Result<T,E>`/`Pair<A,B>` land in the stdlib.

### Known gaps

- **Generics**: generic enums not yet monomorphized (blocks `Option<T>`, `Result<T,E>`), impl blocks on generic types not monomorphized (blocks methods like `Pair<A,B>.first()`)
- **Type checker**: `ref<T>` unresolved (Phase 2)
- **Codegen**: closures compile as function pointers (non-capturing only; capture analysis is Phase 2)

### Design artifacts

- **Language design** -- syntax decisions, memory model, async model, module system, all finalized through iterative design sessions
- **EBNF grammar** -- `grammar.ebnf`, 426 lines covering all syntax constructs
- **Example codebase** -- 17 `.expo` files porting `auth-manager` (a real Rust microservice) into Expo pseudocode, validating the language feels right
- **Memory strategy** -- documented in `MEMORY.md` (stack, ownership+move, explicit arena)
- **Concurrency model** -- documented in `CONCURRENCY.md` (tasks, actors, native runtime, supervision)
- **Project config format** -- `project.expo` replacing `Cargo.toml`

### Tooling (pulled forward)

- **Formatter** -- `expo format --write` / `--check`, opinionated and zero-config, handles escape re-encoding for round-trip correctness, preserves `@moduledoc`/`@doc` annotations
- **LSP** -- `expo-lsp` binary providing real-time diagnostics, document formatting, hover (Markdown-rendered type signatures + `@doc`/`@moduledoc`), and go-to-definition (including qualified module calls) over stdio, integrated with the VSCode/Cursor extension
- **VSCode extension** -- syntax highlighting and LSP client for `.expo` files

---

## Phase 1: Bootstrap compiler -- COMPLETE

Build a minimal Expo compiler in Rust that can compile trivial programs to native binaries via LLVM.

### Month 1 -- Lexer and parser (complete)

- ~~Custom recursive descent parser (not a generator -- easier to produce good error messages, and the grammar is simple enough)~~
- ~~Lex all tokens defined in `grammar.ebnf` section 18 (identifiers, keywords, literals, operators)~~
- ~~Parse into a typed AST covering: imports, structs, enums, functions, `if`/`match`/`cond`, `for`/`loop`, expressions, assignments~~
- ~~Closures and annotations can be parsed but don't need to do anything yet~~
- ~~**Deliverable**: `expo parse file.expo` prints the AST~~

**Status**: All grammar constructs parse correctly. Pratt parser handles operator precedence. `expo parse` and `expo lex` commands work. String interpolation (`#{}`) and escape sequences (`\"`, `\\`, `\n`, `\t`, `\#`) fully implemented in the lexer with a mode stack for nested interpolation. Multiline strings (`"""`) support the same escapes as single-line strings and are automatically dedented based on the closing delimiter's column position.

### Month 2 -- Type system and semantic analysis (complete)

- ~~Type checking: primitives, structs, enums~~
- ~~Type inference for local variables (explicit types on function signatures, inferred inside bodies)~~
- ~~Method resolution for `impl` blocks~~
- ~~Name resolution across modules (file = module, import-driven discovery)~~
- ~~`priv fn` visibility enforcement~~
- ~~Circular import detection~~
- ~~Match exhaustiveness checking, unused variable warnings~~
- ~~Import conflict detection, qualified imports (`math.add()`)~~
- ~~**Deliverable**: `expo check file.expo` reports type errors with clear messages~~

Remaining work (generics, `ref<T>`, trait impls) is Phase 2 scope.

### Month 3 -- LLVM codegen (complete)

- ~~Integrate LLVM via `inkwell` (Rust LLVM bindings)~~
- ~~Code generation for: functions, structs, enums, impl methods, if/else, while, loop, break, return, compound assignment, cond, match, string interpolation, closures (non-capturing block form), pipe operator~~
- ~~Stack allocation for primitives and small structs~~
- ~~Link against libc for `main` entry point and basic I/O~~
- ~~Enums as tagged unions, full pattern matching (wildcard, literal, binding, nested, `when` guards)~~
- ~~Multi-module compilation to a single native binary~~
- ~~**Deliverable**: `expo build hello.expo` produces a native binary that runs~~

Remaining work (inline closures, for loops, try `?`, closure capture analysis) is Phase 2 scope.

### Key decisions

| Decision              | Recommendation                                                                                                                                                                                                                                                                                                                |
| --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Parser strategy       | Custom recursive descent. Better error messages, full control, no external dependency. The grammar is small enough.                                                                                                                                                                                                           |
| LLVM bindings         | `inkwell`. Mature, well-documented, widely used in Rust compiler projects. Cranelift is faster to compile but has a less mature API for a full language.                                                                                                                                                                      |
| Error message quality | Invest early. Elm proved this matters more than features for adoption. Every error should show the source line, point to the problem, and suggest a fix. Errors should be self-contained and unambiguous -- the same quality bar that helps junior developers also helps AI fix its own generated code without extra context. |

---

## Phase 2: Core language

Make the compiler powerful enough to compile non-trivial programs with Expo's generics, ownership model, and structured concurrency.

**Note**: The parser and AST already handle all Phase 2 constructs (for, closures, arena, spawn/await, generics). The work here is wiring up type checking and codegen, not design or parsing. Generics are the gate to Phase 2 -- `Option<T>`, `Result<T,E>`, collections, `Pair<A,B>`, try (`?`), and `ref<T>` all depend on them.

**Implementation order**: Generics first (the shared gate -- unlocks everything). After generics, two independent tracks can proceed in parallel:

- **Track A -- Ownership/borrowing**: move semantics, borrow checking, drop insertion (pure compile-time flow analysis, no dependency on collections)
- **Track B -- Collections/closures**: `List<T>`, `Map<K,V>`, inline closure codegen, iterators, `for` loops (no dependency on ownership)

Tasks require both tracks to converge (borrow safety across spawn boundaries + practical collection passing). Closure capture analysis also sits at the intersection (move vs. borrow into closures).

### Generics and monomorphization

- ~~Type parameter syntax already parsed: `struct Pair<A, B>`, `fn identity<T>(x: T) -> T`~~
- ~~Monomorphization: generate specialized LLVM IR for each concrete instantiation~~
- ~~Type variable unification across call sites~~
- ~~Generic structs and functions~~
- Generic enums (required for `Option<T>` and `Result<T,E>`)
- Monomorphization of impl blocks on generic types (required for methods like `.first()`, `.map()`)
- `Option<T>` and `Result<T,E>` as built-in enum types with `Some`/`None`/`Ok`/`Err`
- `Pair<A, B>` stdlib struct (with `.first` / `.second`)
- The `?` operator for error propagation (desugars to early `return Err(...)`)
- **Done when**: `Option<T>`, `Result<T,E>`, and `Pair<A,B>` compile and work in match expressions

### Ownership and borrowing

- Implement move semantics: assignment moves, use-after-move is a compile error
- Borrow-by-default: function parameters are read-only borrows unless marked `move`
- Borrows are always read-only -- no `&mut T`, ever (see `MEMORY.md`)
- `move` keyword on parameters for explicit ownership transfer
- `ref<T>` syntax for reference types in return positions and generics
- For loops iterate by reference by default (no annotation needed)
- No lifetime annotations -- borrows are scoped to the function call
- Implement `clone()` as the explicit escape hatch
- Drop insertion at scope boundaries (deterministic destruction)
- The `&` symbol does not exist in Expo -- borrowing is implicit, references use `ref<T>`
- **Done when**: programs that move, borrow, and clone compile correctly, and use-after-move is caught

### Collections, closures, and iteration

- `List<T>`, `Map<K,V>`, `Set<T>` as built-in generic types backed by native implementations
- Closure capture analysis (move vs. borrow) -- non-capturing block closures (`fn (params) -> type ... end`) land in Phase 1, capturing closures need ownership semantics from this phase
- Inline closure codegen (`x -> expr`) -- requires type inference and generics to compile without explicit annotations
- Bare function names as references (no sigil -- `foo` references, `foo()` calls)
- Iterator methods: `.map()`, `.filter()`, `.any?()`, `.all?()`, `.retain()`, `.iter()`
- Ownership splitting for concurrent mutation patterns (tasks receive owned, non-overlapping chunks -- specific API designed during stdlib phase)
- `for` loops over iterables
- `arena...end` blocks with bulk-free semantics
- **Done when**: `ua_parser.expo` compiles -- it exercises structs, enums, match, closures, method chaining, and returns

### Tasks and structured concurrency

- `spawn fn -> ... end` creates a stackless task (compiler transforms to a state machine), returns `Handle<T>`
- `await handle` returns `Result<T, TaskError>`
- Tasks can borrow (read-only) from the parent scope -- structured concurrency guarantees the parent outlives the task
- `task.async_stream` for bounded concurrent enumeration with back-pressure
- Cooperative yielding at `await` and I/O points
- No preemption for tasks -- they're short-lived computations, not long-running entities
- Tasks are cancelled if the parent returns or crashes (structured lifetime)
- **Done when**: a program that spawns tasks, borrows parent data, and awaits results compiles correctly

See `CONCURRENCY.md` "Tasks" section and `MEMORY.md` "At concurrency boundaries" for full design details.

### Risks

- **Generic monomorphization**: generics like `Patch<T>` need to be monomorphized at compile time. This is well-understood (Rust, C++ do it) but adds compiler complexity. Start with concrete types, then generalize.
- **Borrow checker complexity**: Expo's model is simpler than Rust's (no lifetimes, no mutable borrows), but still requires flow analysis. Start with a conservative checker that rejects some valid programs rather than accepting invalid ones. Loosen over time.
- **Task borrow safety**: structured concurrency simplifies this (parent outlives tasks by construction), but the compiler must still prove that borrowed data isn't moved while tasks hold references. Flow analysis required.

---

## Phase 3: Actors and runtime

Build the actor primitive and the native runtime that schedules actors and tasks. This phase gets actors running -- the raw construct and the infrastructure that supports them. Supervision, preemption, and priority come in Phase 3b.

Expo has two concurrency primitives (tasks in Phase 2, actors here) because in native compiled code without a VM, the cost difference between a short-lived computation and a long-lived stateful entity is significant. See `CONCURRENCY.md` for the full design rationale.

### Actors

- `actor` keyword defines a long-lived stateful entity with typed mailboxes
- `receive` with compile-time exhaustiveness checking on message enum variants
- Actor memory is fully isolated -- data crosses boundaries via `move` or `clone`
- Fire-and-forget `send` for one-way messages
- Request/reply `call` with default 5-second timeout, returns `Result<T, CallError>`
- Explicit `reply(from, value)` with compiler warning for `call`-pattern messages that never reply
- Starting actors: handle-based (anonymous) and named (string registration)
- Stopping actors: graceful shutdown with deterministic cleanup via ownership
- **Done when**: a counter actor with typed messages compiles and runs

### Runtime

- Work-stealing scheduler (M:N -- many actors/tasks on few OS threads)
- I/O reactor (epoll on Linux, kqueue on macOS) -- the user sees blocking calls, the runtime suspends transparently
- Timer wheel for timeouts, intervals, and `call` deadlines
- Actor lifecycle manager (start, stop, crash detection)
- All functions can suspend; the runtime handles it -- no function coloring
- **Done when**: 10,000 actors run concurrently with correct scheduling

### Key decisions

| Decision         | Recommendation                                                                                                                                     |
| ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| Two primitives   | Tasks (stackless, structured, short-lived) and actors (stacked, isolated, long-lived). Different costs, different guarantees, both first-class.    |
| Native runtime   | A runtime library linked into the binary, not a VM. No bytecode, no GC. Similar to Go's runtime or Tokio, but with actor lifecycle management.     |
| Scheduler model  | Work-stealing, similar to Tokio/Go. M:N threading. Start with a simple round-robin scheduler, upgrade to work-stealing once correctness is proven. |
| I/O model        | epoll/kqueue-backed async I/O under the hood. The user sees blocking calls; the runtime suspends the actor/task.                                   |
| Actor stack size | Real stacks for actors (4-8KB). Tasks are stackless state machines (zero stack overhead).                                                          |
| Typed mailboxes  | Each actor declares a message enum. `send` and `receive` are type-checked at compile time. Exhaustiveness checking catches unhandled messages.     |

### Risks

- **Runtime complexity**: building a work-stealing scheduler with I/O integration is substantial engineering. Start with round-robin and single-threaded I/O, then scale up.
- **Typed mailbox ergonomics**: forcing every actor to declare a message enum adds boilerplate. Monitor whether this feels natural or burdensome in practice.

---

## Phase 3b: Reliability

Build on the working actor runtime with production-grade reliability features. These are layered on top -- actors must work before they can be supervised or prioritized.

### Preemption and priority

- Compiler-inserted yield checks at function call preambles and loop back-edges
- Priority levels (`Low`, `Normal`, `High`) control actor scheduling budget -- higher priority actors get more CPU time before yielding
- Actors default to `Normal` priority; configurable at spawn time
- Tasks are not preempted (they're short-lived and yield cooperatively at `await`)
- **Done when**: a low-priority actor yields to high-priority actors under load

### Supervision

- Supervisors are stdlib actors -- not a language primitive, but a standard library pattern
- Restart strategies: `OneForOne`, `OneForAll`, `RestForOne`
- Max-restarts-exceeded crashes the supervisor
- Root supervisor crash exits the process (deterministic shutdown)
- Ownership ensures deterministic cleanup on actor crash -- all owned memory is freed, no leaks
- **Done when**: a supervised actor tree restarts crashed children correctly

### Shared data

- `shared_map` (stdlib concurrent hash map, needs a proper name) for shared caches across actors
- `put` moves values in (ownership transfer, no races)
- `get` borrows values out (zero-copy read access)
- `delete` removes and drops values
- Solves the two core problems of shared state: memory explosion from copying, and corruption from concurrent modification
- **Done when**: multiple actors read/write a `shared_map` without corruption

### Risks

- **Preemption yield-check overhead**: every function call and loop back-edge gets a yield check. Must be cheap (single counter decrement + branch). Profile to ensure overhead stays under 1-2%.
- **Supervision ergonomics**: defining child specs and restart strategies should feel lightweight, not XML-configuration-heavy. Design the API carefully.
- **`shared_map` naming**: needs a proper name before 1.0. Candidates TBD.

---

## Phase 4: Standard library

Build the minimal stdlib -- only primitives that will still be relevant in 20 years. Everything else ships as first-party packages that version independently of the compiler.

Concurrency primitives (tasks, actors, `shared_map`, supervisors) already ship in Phases 2, 3, and 3b.

### Stdlib (ships with the compiler, always available)

- `String` with UTF-8 internals, interpolation (`#{}` with format specs), `.trim()`, `.split()`, `.starts_with?()`, `.empty?()`, `.contains?()`
- `List<T>`, `Map<K,V>`, and `Set<T>` with full method sets
- `Option<T>` and `Result<T,E>` methods (`.map()`, `.unwrap_or()`, `.ok?()`)
- File I/O: `file.read()`, `file.write()`, `file.exists?()`
- `time.DateTime`, `time.Duration` with `.now()`, `.timestamp_millis()`, `.from_secs()`
- Serialization trait/interface that packages can implement
- **Done when**: `config.expo` compiles (exercises strings, file reading, option handling, duration)

### First-party packages (maintained by the Expo team, versioned independently)

These need the package manager (Phase 5) to exist first. They are high-quality, officially maintained, but not part of the compiler release cycle. Protocols and algorithms evolve on their own timeline.

- HTTP server and client
- JSON serialization/deserialization
- TLS (thin wrapper over system TLS library)
- Crypto: hashing, random bytes (thin wrapper over libsodium or similar)
- Structured logging
- MessagePack serialization
- UUID generation, regex, URL parsing
- **Done when**: `handlers.expo` compiles using stdlib + first-party packages -- it exercises HTTP, JSON, crypto, logging, and UUID generation

### Approach

Implement natively in Expo (or Rust for the bootstrap) wherever possible. Use thin C FFI only for security-critical crypto and performance-critical parsing. The stdlib provides traits/interfaces (e.g., serialization) that first-party packages implement, so formats can be added or replaced without touching the compiler.

---

## Phase 5: Tooling

### Already done

- ~~`expo run` for development (compile + execute)~~ -- implemented during Phase 1
- ~~`expo fmt` -- opinionated, zero-config code formatter~~ -- `expo format --write` / `--check` implemented during Phase 1
- ~~VS Code extension~~ -- syntax highlighting for `.expo` files implemented during Phase 1

### Package manager and project system

- `expo build` compiles a project based on `project.expo`
- `expo test` discovers and runs `@test` annotated functions
- Dependency resolution: fetch from hosted sources (git URLs initially, registry/mirror possible long-term)
- Lock file generation for reproducible builds
- **Done when**: `project.expo` from this repo resolves its three dependencies and builds the project

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
- **Done when**: `expo shell -S .` loads a multi-module project and you can call functions, pipe results, and read docs interactively

---

## Phase 6: Self-hosting

Rewrite the Expo compiler in Expo.

### Port the lexer and parser

- Rewrite the lexer and parser from Rust to Expo
- This is the first real stress test of the language for non-trivial code
- Expect to discover language shortcomings -- feed them back into design
- **Done when**: the Expo-written parser can parse all `.expo` files identically to the Rust parser

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

## Phase 7: Validation

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

## Summary timeline

Phase 1 infrastructure stood up in ~36 hours with AI assistance. The original 18-month estimate assumed a slower pace. The timeline below reflects actual velocity for scaffolding while staying conservative on genuinely hard problems (borrow checker, async runtime, self-hosting).

### Done

| Phase     | Milestone                                                                                | Status |
| --------- | ---------------------------------------------------------------------------------------- | ------ |
| Bootstrap | Lexer + parser -- all grammar constructs parse, string interpolation + escapes           | Done   |
| Bootstrap | Type system -- multi-module, `priv fn`, enums, match exhaustiveness, unused var warnings | Done   |
| Bootstrap | LLVM codegen -- native binaries, enums, match, cond, string interpolation                | Done   |
| Tooling   | Formatter (`expo format --write`/`--check`)                                              | Done   |
| Tooling   | `expo run` (compile + execute)                                                           | Done   |
| Tooling   | VSCode extension (syntax highlighting)                                                   | Done   |
| Tooling   | LSP -- diagnostics, formatting, hover, go-to-definition                                  | Done   |
| Tooling   | Documentation generator (`expo doc`) -- HTML output, sidebar nav, brand theme            | Done   |
| Core      | Generics -- monomorphization of generic functions and structs, type unification          | Done   |

### Remaining

| Phase       | Milestone                                                     |
| ----------- | ------------------------------------------------------------- |
| Core        | Generics -- generic enums, impl monomorphization, `Option<T>`, `Result<T,E>`, `?` operator |
| Core        | Ownership + borrow checker + tasks (structured concurrency)   |
| Core        | Collections, closures, arena, `ua_parser.expo` compiles       |
| Actors      | Actor primitive, typed mailboxes, runtime (scheduler, I/O)    |
| Reliability | Preemption/priority, supervision, `shared_map`                |
| Stdlib      | Core types, I/O, time, `config.expo` compiles                 |
| Stdlib      | First-party packages (HTTP, JSON, crypto, logging)            |
| Tooling     | Package manager, test runner                                  |
| Tooling     | Documentation generator (doctests, search, prose pages)       |
| Tooling     | LSP -- autocomplete, inline type hints, multi-module          |
| Tooling     | Interactive shell (`expo shell`) -- REPL with project loading |
| Self-host   | Lexer + parser in Expo                                        |
| Self-host   | Full compiler in Expo                                         |
| Self-host   | Retire Rust bootstrap                                         |
| Validation  | auth-manager-expo runs for real                               |

---

## Guiding principles

- **Readability over cleverness.** Every language feature decision is judged by: "can a reader understand this line without reading any other line?"
- **Error messages are a feature.** Invest in them from month 1. A confusing error message is a bug.
- **The example codebase is the test suite.** Every phase targets compiling a specific `.expo` file from this repo. The language grows toward real code, not toy examples.
- **AI writes, humans read.** The language is concise and readable because that's good design -- Ruby over Java, signal density over ceremony. Every line should carry meaning without boilerplate. AI-friendliness is a natural consequence of those values, not the driver.
- **No magic.** Explicit is better than implicit. If a feature requires the reader to know something they can't see on screen, it's wrong for Expo.
- **No macros.** Bake common patterns into the language as native constructs instead. Macros create invisible control flow, fragment the language per-codebase, and are hostile to AI tooling. Every Expo codebase should read the same way.
- **Approachable by default.** A beginner should be able to write their first program without knowing about ownership, actors, or type annotations. Advanced features reveal themselves as you grow -- you learn `move` when you hit a performance problem, tasks when you need concurrency, actors when you build a stateful service. The language has a Ruby-shaped learning curve backed by Rust-grade safety.
- **Built to last.** Every design decision passes the decades test -- will this still make sense in 20 years? Features tied to today's trends are packages, not language constructs. The stdlib only contains primitives that are as fundamental as integers and files.
- **Stable after 1.0.** The language spec locks at 1.0. Post-1.0 changes are additive only -- new features, never removals or breaking changes. No edition system. If something truly needs to break (hopefully a decade+ out), it's a clean 2.0 with migration tooling -- one decisive move, not death by a thousand editions.
