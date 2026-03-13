# Expo Language Roadmap

Solo developer + AI assistance. Bootstrap in Rust, self-host in Expo.

---

## Current state

What exists today:

- **Language design** -- syntax decisions, memory model, async model, module system, all finalized through iterative design sessions
- **EBNF grammar** -- `grammar.ebnf`, 418 lines covering all syntax constructs
- **Example codebase** -- 17 `.expo` files porting `auth-manager` (a real Rust microservice) into Expo pseudocode, validating the language feels right
- **Memory strategy** -- documented in `MEMORY.md` (stack, ownership+move, explicit arena)
- **Project config format** -- `project.expo` replacing `Cargo.toml`

What does NOT exist yet: a compiler, a runtime, a standard library, or any tooling.

---

## Phase 1: Bootstrap compiler (months 1-3)

Build a minimal Expo compiler in Rust that can compile trivial programs to native binaries via LLVM.

### Month 1 -- Lexer and parser

- Hand-written recursive descent parser (not a generator -- easier to produce good error messages, and the grammar is simple enough)
- Lex all tokens defined in `grammar.ebnf` section 18 (identifiers, keywords, literals, operators)
- Parse into a typed AST covering: imports, structs, enums, functions, `if`/`match`/`cond`, `for`/`loop`, expressions, assignments
- Closures and annotations can be parsed but don't need to do anything yet
- **Deliverable**: `expo parse file.expo` prints the AST
- **Done when**: all 17 `.expo` files in this repo parse without errors

### Month 2 -- Type system and semantic analysis

- Type checking: primitives, structs, enums, generics, `Option<T>`, `Result<T,E>`, `Vec<T>`, `HashMap<K,V>`
- Type inference for local variables (explicit types on function signatures, inferred inside bodies)
- Method resolution for `impl` blocks and trait impls
- Name resolution across modules (file = module, auto-discovered)
- `priv fn` visibility enforcement
- **Deliverable**: `expo check file.expo` reports type errors with clear messages
- **Done when**: a hello-world program and a simple struct program pass type checking

### Month 3 -- LLVM codegen (hello world)

- Integrate LLVM via `inkwell` (Rust LLVM bindings)
- Code generation for: function calls, arithmetic, string literals, `if`/`else`, `match` (simple cases), `return`
- Stack allocation for primitives and small structs
- Link against libc for `main` entry point and basic I/O
- **Deliverable**: `expo build hello.expo` produces a native binary that runs
- **Done when**: a program that prints output, does arithmetic, and uses `if`/`match` compiles and runs correctly

### Key decisions

| Decision              | Recommendation                                                                                                                                           |
| --------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Parser strategy       | Hand-written recursive descent. Better error messages, full control, no external dependency. The grammar is small enough.                                |
| LLVM bindings         | `inkwell`. Mature, well-documented, widely used in Rust compiler projects. Cranelift is faster to compile but has a less mature API for a full language. |
| Error message quality | Invest early. Elm proved this matters more than features for adoption. Every error should show the source line, point to the problem, and suggest a fix. |

---

## Phase 2: Core language (months 4-6)

Make the compiler powerful enough to compile non-trivial programs with Expo's ownership model.

### Month 4 -- Ownership and borrowing

- Implement move semantics: assignment moves, use-after-move is a compile error
- Borrow-by-default: function parameters are read-only borrows unless marked `move`
- `move` keyword on parameters for explicit ownership transfer
- `ref<T>` syntax for reference types in return positions and generics
- For loops iterate by reference by default (no annotation needed)
- No lifetime annotations -- borrows are scoped to the function call
- Implement `clone()` as the explicit escape hatch
- Drop insertion at scope boundaries (deterministic destruction)
- The `&` symbol does not exist in Expo -- borrowing is implicit, references use `ref<T>`
- **Done when**: programs that move, borrow, and clone compile correctly, and use-after-move is caught

### Month 5 -- Pattern matching and enums

- Full pattern matching: destructuring, `when` guards, nested patterns, wildcard `_`
- Enum variants: unit, tuple, and struct forms
- `Option<T>` and `Result<T,E>` as built-in enum types with `Some`/`None`/`Ok`/`Err`
- The `?` operator for error propagation (desugars to early `return Err(...)`)
- Exhaustiveness checking on `match`
- **Done when**: the `WriteOp` enum from `state_machine.expo` compiles and pattern-matches correctly

### Month 6 -- Collections and closures

- `Vec<T>`, `HashMap<K,V>` as built-in generic types backed by native implementations
- Both closure forms: `(args -> expr)`, `fn args -> body end`
- Bare function names as references (no sigil -- `foo` references, `foo()` calls)
- Closure capture analysis (move vs. borrow)
- Iterator methods: `.map()`, `.filter()`, `.any?()`, `.all?()`, `.retain()`, `.iter()`
- `for` loops over iterables
- `arena...end` blocks with bulk-free semantics
- **Done when**: `ua_parser.expo` compiles -- it exercises structs, enums, match, closures, method chaining, and returns

### Risks

- **Borrow checker complexity**: Expo's model is simpler than Rust's (no lifetimes), but still requires flow analysis. Start with a conservative checker that rejects some valid programs rather than accepting invalid ones. Loosen over time.
- **Generic monomorphization**: generics like `Patch<T>` need to be monomorphized at compile time. This is well-understood (Rust, C++ do it) but adds compiler complexity. Implement for concrete types first, generics second.

---

## Phase 3: Async runtime (months 7-8)

Build the green thread runtime that makes `spawn`/`await`/`receive` work.

### Month 7 -- Green threads and scheduler

- Implement a work-stealing scheduler (N green threads on M OS threads)
- `spawn(fn -> ... end)` creates a new green thread, returns `Handle<T>`
- `await handle` blocks the current green thread until the handle resolves
- Cooperative yielding at I/O boundaries
- No function coloring -- every function is the same, the runtime handles suspension
- **Done when**: a program that spawns 10,000 green threads that each sleep and return a value runs correctly

### Month 8 -- Channels and receive

- `receive...end` block that waits for the first of multiple async sources
- Basic channel/message-passing primitives
- `interval()` and `timer` support for periodic tasks
- **Done when**: `cleaner.expo` compiles and runs (spawns tasks, awaits handles, uses interval timer)

### Key decisions

| Decision        | Recommendation                                                                                                                                     |
| --------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| Scheduler model | Work-stealing, similar to Tokio/Go. M:N threading. Start with a simple round-robin scheduler, upgrade to work-stealing once correctness is proven. |
| I/O model       | epoll/kqueue-backed async I/O under the hood. The user sees blocking calls; the runtime suspends the green thread.                                 |
| Stack size      | Segmented or growable stacks for green threads. Start with a fixed 8KB stack, add growable stacks later.                                           |

---

## Phase 4: Standard library (months 9-11)

Build the stdlib modules that the example codebase imports.

### Month 9 -- Core types and I/O

- `string` with UTF-8 internals, interpolation (`#{}` with format specs), `.trim()`, `.split()`, `.starts_with?()`, `.empty?()`, `.contains?()`
- `Vec<T>` and `HashMap<K,V>` with full method sets
- `Option<T>` and `Result<T,E>` methods (`.map()`, `.unwrap_or()`, `.ok?()`)
- File I/O: `file.read()`, `file.write()`, `file.exists?()`
- `time.DateTime`, `time.Duration` with `.now()`, `.timestamp_millis()`, `.from_secs()`
- **Done when**: `config.expo` compiles (exercises strings, file reading, option handling, duration)

### Month 10 -- HTTP and networking

- HTTP server: listener, routing, request/response types, middleware pattern
- HTTP client: `Req`-style interface for making outbound requests
- JSON: serialization/deserialization via `@json` annotation (compile-time codegen, no reflection)
- URL parsing, query string handling
- TLS support (link against a system TLS library or bundle)
- **Done when**: a basic HTTP server that handles JSON requests and responds compiles and runs

### Month 11 -- Crypto, logging, serialization

- `crypto.random_hex()`, `crypto.sha256()` (native or thin C wrapper over libsodium)
- `log` module with structured logging (the `key: value` syntax in log calls)
- MessagePack serialization (for the database layer in auth-manager)
- UUID generation
- Regular expressions (RE2 or similar)
- User-agent parsing (`woothee` or native port)
- **Done when**: `handlers.expo` compiles -- it's the richest file, exercising HTTP, JSON, crypto, logging, and UUID generation

### Approach

Implement natively in Expo (or Rust for the bootstrap) wherever possible. Use thin C FFI only for security-critical crypto (libsodium) and performance-critical parsing (JSON via yyjson, HTTP via llhttp). Over time, replace C wrappers with native implementations.

---

## Phase 5: Tooling (months 12-14)

### Month 12 -- Package manager and project system

- `expo build` compiles a project based on `project.expo`
- `expo test` discovers and runs `@test` annotated functions
- Dependency resolution: fetch from git URLs and a package registry
- Lock file generation for reproducible builds
- `expo run` for development (compile + execute)
- **Done when**: `project.expo` from this repo resolves its three dependencies and builds the project

### Month 13 -- Formatter and documentation

- `expo fmt` -- opinionated, zero-config code formatter (like `gofmt`)
- `expo doc` -- generates HTML documentation from `@doc` annotations, similar to HexDocs
- Doctest support: code examples in `@doc` strings are compiled and run as tests
- **Done when**: running `expo fmt` on all `.expo` files produces consistent output, and `expo doc` generates browsable HTML

### Month 14 -- Language server (LSP)

- Basic LSP: go-to-definition, hover for types, diagnostics (errors/warnings)
- Autocomplete for module names, function names, struct fields
- Inline type hints for inferred types
- Integration with VS Code / Cursor via an extension
- **Done when**: editing `.expo` files in Cursor shows real-time errors and supports go-to-definition

---

## Phase 6: Self-hosting (months 15-17)

Rewrite the Expo compiler in Expo.

### Month 15 -- Port the lexer and parser

- Rewrite the hand-written lexer and parser from Rust to Expo
- This is the first real stress test of the language for non-trivial code
- Expect to discover language shortcomings -- feed them back into design
- **Done when**: the Expo-written parser can parse all `.expo` files identically to the Rust parser

### Month 16 -- Port type checking and codegen

- Rewrite semantic analysis, type checker, and LLVM codegen in Expo
- LLVM bindings via C FFI (Expo calling into the LLVM C API)
- **Done when**: the Expo-written compiler can compile itself (the compiler compiles itself)

### Month 17 -- Retire the bootstrap

- Run the full test suite through the self-hosted compiler
- Fix any remaining differences between Rust bootstrap output and Expo self-hosted output
- The Rust bootstrap is now only needed for bootstrapping from scratch
- **Done when**: `expo build` using the self-hosted compiler produces identical binaries to the Rust bootstrap for all test programs

---

## Phase 7: Validation (month 18)

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
  param email: string
  param password: string

  step hash_password -> password_hash: string
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

## Summary timeline

| Month | Phase      | Milestone                                               |
| ----- | ---------- | ------------------------------------------------------- |
| 1     | Bootstrap  | Lexer + parser, all `.expo` files parse                 |
| 2     | Bootstrap  | Type system, `expo check` works                         |
| 3     | Bootstrap  | LLVM codegen, first native binary                       |
| 4     | Core       | Ownership + borrow checker                              |
| 5     | Core       | Pattern matching + enums + `?` operator                 |
| 6     | Core       | Collections, closures, arena, `ua_parser.expo` compiles |
| 7     | Async      | Green thread scheduler, `spawn`/`await`                 |
| 8     | Async      | Channels, `receive`, `cleaner.expo` compiles            |
| 9     | Stdlib     | Core types, I/O, time, `config.expo` compiles           |
| 10    | Stdlib     | HTTP server/client, JSON                                |
| 11    | Stdlib     | Crypto, logging, `handlers.expo` compiles               |
| 12    | Tooling    | Package manager, test runner                            |
| 13    | Tooling    | Formatter, doc generator                                |
| 14    | Tooling    | LSP for Cursor/VS Code                                  |
| 15    | Self-host  | Lexer + parser in Expo                                  |
| 16    | Self-host  | Full compiler in Expo                                   |
| 17    | Self-host  | Retire Rust bootstrap                                   |
| 18    | Validation | auth-manager-expo runs for real                         |

---

## Guiding principles

- **Readability over cleverness.** Every language feature decision is judged by: "can a reader understand this line without reading any other line?"
- **Error messages are a feature.** Invest in them from month 1. A confusing error message is a bug.
- **The example codebase is the test suite.** Every phase targets compiling a specific `.expo` file from this repo. The language grows toward real code, not toy examples.
- **AI writes, humans read.** The language is optimized for reading comprehension and signal density, not keystroke reduction.
- **No magic.** Explicit is better than implicit. If a feature requires the reader to know something they can't see on screen, it's wrong for Expo.
- **No macros.** Bake common patterns into the language as native constructs instead. Macros create invisible control flow, fragment the language per-codebase, and are hostile to AI tooling. Every Expo codebase should read the same way.
