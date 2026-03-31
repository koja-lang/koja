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

Eight commands: `expo build`, `expo run`, `expo check`, `expo test`, `expo format`, `expo doc`, `expo lex`, `expo parse`. All commands support multi-module projects.

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
- Compound assignment (`+=`, `-=`, `*=`, `/=`) on variables and struct fields (e.g., `self.pos += 1`)
- String interpolation
- Protocols (`protocol` keyword, `impl Protocol for Type` conformance)
- Closures (block form, with variable capture -- copy for primitives, move for structs/enums)
- Function type syntax (`fn(T) -> U`) for closure-accepting parameters
- `print` builtin (dispatches through `Debug.format()` for all types)
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
- **Closure `move` params**: `ClosureParam` has no `PassMode` field -- `fn (move x: T) -> U ... end` doesn't parse. `Type::Function` also doesn't carry param modes, so the type checker can't enforce `fn(move T) -> U` vs `fn(T) -> U` contracts. Both need fixing: add `mode` to `ClosureParam`, parse `move` in closure params, and add param modes to `Type::Function` for type-level enforcement.
- ~~**Tail call optimization**~~: **Done.** Self-recursive `move self` methods are rewritten as loops when a self-call appears in tail position (implicit returns and explicit `return`). Covers both `-> Self` and void-returning methods (e.g., the default `Process.run` server loop). Eliminates stack growth for the language's core recursive idiom. General TCO (mutual recursion, arbitrary tail calls) remains future work.
- **Identifier priming for keyword/builtin collisions**: (for self-hosting) `IDENT` and `TYPE_IDENT` cannot use reserved words or built-in type names as identifiers. Trailing prime notation (`'`) would allow `end'` as a field name and `Self'` or `String'` as enum variant names without ambiguity. Grammar change: append `[ "'" ]` to both `IDENT` and `TYPE_IDENT` rules (trailing-only, single prime). Surfaced by the `expo-ast` self-hosting port: `Span.end` had to become `Span.stop`, and enum variants like `Self`, `String`, `Bool`, `Int`, `Float` needed descriptive renames (`SelfReceiver`, `StringVal`, `BoolLit`, etc.). Leading `'` stays invalid, so `'wrongstring'` is always a syntax error.
- **Nested enum pattern matching with literal payloads**: matching a nested variant with a literal payload (e.g., `Some(TokenKind.Ident("and"))`) causes a segfault at runtime. The workaround is to bind the payload and check it in the body: `Some(TokenKind.Ident(name)) -> name == "and"`. Surfaced during the self-hosted lexer port (`continues_line?`).
- **Nested enum equality codegen**: comparing `Option<SomeEnum>` with `==` generates invalid LLVM IR (phi node predecessors mismatch) when the inner enum has many variants. The workaround is to use `match` instead of `==` for `Option<Enum>` comparisons. Surfaced during the self-hosted lexer port (`lex_newline` duplicate newline check).
- **Integer literal type coercion at call sites**: integer literals in function call arguments default to `i64` and are not coerced to match the parameter type (e.g., passing `1` to a `UInt32` parameter generates `i64 1` in LLVM IR, causing a type mismatch verification error). Variable annotations work (`x: UInt32 = 1`), but call-site coercion does not. Workaround: avoid small-integer parameters with literal arguments, or bind to an annotated variable first. Surfaced during the self-hosted lexer port (`Cursor.peek_at`).
- **`match` inside `while`/`loop` with `return` causes codegen crash**: when a `match` expression appears inside a `while` or `loop` body and any arm contains a `return` statement, the generated binary segfaults on startup (before `main` runs). The crash is in LLVM codegen -- likely incorrect basic block wiring for the match's phi nodes when nested inside a loop's back-edge structure. Workaround: use recursion instead of loops with `match`. Since Expo is FP-oriented, recursive helpers with `move` parameters are idiomatic and avoid the bug entirely. Surfaced during the `json` package decoder (recursive descent parser for arrays and objects).

### Design artifacts

- **Language design** -- syntax decisions, memory model, async model, module system, all finalized through iterative design sessions
- **EBNF grammar** -- `grammar.ebnf`, ~460 lines covering all syntax constructs
- **Example codebase** -- 17 `.expo` files porting `auth-manager` (a real Rust microservice) into Expo pseudocode, validating the language feels right
- **Memory strategy** -- documented in `archive/20260323-MEMORY.md` (stack, ownership+move, explicit arena)
- **Concurrency model** -- documented in `archive/20260313-CONCURRENCY.md` and `archive/20260323-CONCURRENCY.md` (processes, native runtime, supervision)
- **Project config format** -- `project.expo` replacing `Cargo.toml`
- **Module system redesign** -- documented in `IMPORT.md` (files as transparent, types as namespaces, no intra-project imports, qualified package access, `import` keyword removed)

### Tooling (pulled forward)

- **Formatter** -- `expo format --write` / `--check`, opinionated and zero-config, handles escape re-encoding for round-trip correctness, preserves `@moduledoc`/`@doc` annotations
- **LSP** -- `expo-lsp` binary providing real-time diagnostics, document formatting, hover (Markdown-rendered type signatures + `@doc`/`@moduledoc`), and go-to-definition (including qualified module calls) over stdio, integrated with the VSCode/Cursor extension
- **VSCode extension** -- syntax highlighting and LSP client for `.expo` files

### Build history

Phase 1 (bootstrap compiler) and Phase 2 (core language) are complete. Phase 3 (language surface + runtime maturity) is complete. The full build history with detailed implementation notes is preserved in [archive/20260318-ROADMAP.md](archive/20260318-ROADMAP.md) and [archive/20260330-ROADMAP.md](archive/20260330-ROADMAP.md).

---

## Phase 3: Language surface + Runtime maturity -- done

Phase 3 made the language real on two fronts: language surface (Track A) and runtime maturity (Track B). Both tracks are complete.

### Track A: Language surface -- done

Binary/bitstring system (A1a-A1e), string stdlib and type conversions (A2a-A2c), file I/O (A3a), project system (A3b), and the self-hosted lexer validation milestone (A4). Ranges (A2d) deferred -- not blocking.

### Track B: Runtime maturity -- done

Union types (B1), protocol-based process model with `Process<C, M, R>`, `Ref<M, R>`, `cast`/`call`, default protocol implementations, `receive...after` (B2), and `Task` (B3).

### Key decisions (Phase 3)

| Decision           | Recommendation                                                                                                                                                                                                                                               |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Distinct types     | `String`, `Binary`, and `Bits` are three distinct types with no subtype relationships. Explicit conversion between them: widening always succeeds (zero-cost), narrowing validates and returns `Result`. See `archive/20260323-BITSTRINGS.md`.               |
| No Char            | No dedicated character type. Single-codepoint `String` values serve the same purpose -- fractal design. String ranges (`"a".."z"`) and classification methods (`is_alpha?()`) work on String directly.                                                       |
| Inclusive ranges   | One range operator (`..`), always inclusive on both ends. Pattern matching is the primary use case; numeric loops are rare in idiomatic Expo. `0..n-1` for the occasional exclusive case.                                                                    |
| Erlang defaults    | Binary segments default to unsigned big-endian (network byte order). Matches Erlang and covers the primary use case: HTTP microservices and network protocol parsing.                                                                                        |
| Bitwise protocol   | Bitwise operations are methods (`band`, `bor`, `bxor`, `bnot`, `bsl`, `bsr`) on a `Bitwise` protocol, not symbol operators. Frees `<<`/`>>` for binary literals and `&`/`\|`/`^` for other uses.                                                             |
| One primitive      | Processes are the sole concurrency primitive. `Task` is a kernel struct built on `Process<C, M, R>`, not a separate primitive. GenServer-like actor patterns are the `Process` protocol itself.                                                              |
| Process protocol   | `Process<C, M, R>` with three type params. Config (C) separates public args from private state via `new`. Fixed reply type (R) per process -- same as a service contract. Union types for heterogeneous replies.                                             |
| Scheduler protocol | Define the runtime as a protocol interface before implementing any backend. The native scheduler is the first implementation, not a special case. Enables WASM targets, test runtimes, and third-party custom runtimes without changing user code.           |
| Native runtime     | A runtime library linked into the binary, not a VM. No bytecode, no GC. Similar to Go's runtime or Tokio, but with process lifecycle management.                                                                                                             |
| Typed mailboxes    | Processes declare message type M via protocol impl. `send` and `receive` are type-checked at compile time. Union types enable multi-source mailboxes (e.g., `PoolCmd \| ExitSignal`).                                                                        |
| Validation target  | The lexer port (A4) validated the language surface without requiring external dependencies (no network, no database, no JSON). The Rust compiler remains authoritative; the Expo lexer is compiled by it. ✓ Complete -- token output matches the Rust lexer. |

For detailed sub-milestone breakdowns (A1a-A1e, A2a-A2c, A3a-A3b, A4, B1-B3), see [archive/20260330-ROADMAP.md](archive/20260330-ROADMAP.md).

---

## Phase 4: Stdlib + Ecosystem / Runtime + Reliability

Two independent tracks. Track A makes the language useful for real programs. Track B makes concurrency production-grade. The convergence point is an HTTP server handling real traffic -- it needs both `net.tcp` (Track A) and the multi-threaded scheduler (Track B).

### Track A: Stdlib + Ecosystem

#### Test runner -- **Done**

`expo test` discovers `@test`-annotated functions across `src/` and `test/` directories, generates a synthetic test harness, compiles and runs it. `@test` accepts an optional string description (`@test "adds two numbers"`). Abort-on-first-failure -- the test name is printed before each call so you always know which one failed. `project.expo` gains an optional `test` field (default `["test"]`). Validated with 17 tests in the `json` package (encoder and decoder coverage).

#### Stdlib

Stdlib contains primitives that are as fundamental as integers -- things the compiler or language runtime needs to function, or that virtually every program needs and whose API is stable for decades.

- `std.fd` -- shipped in Phase 3 A3a (basic `read`, `write`, `close`). Extended with `Socket` type for TCP networking (`create`, `bind`, `listen`, `accept`, `set_reuse_addr`, `close`) via POSIX socket syscall shims in the runtime.
- `std.file` -- **DONE** `FileMode` enum (`Read`, `Write`, `Append`). `File.open(path, mode)` opens with explicit mode. `File.write(path, content)` for one-shot writes. `File.exists?(path)`, `File.delete(path)`, `File.rename(src, dst)` for path-based operations. Handle-level writes via `file.fd.write(data)`. `seek` deferred until embedded database work.
- `std.mmap` -- `Mmap` struct for memory-mapped files. Wraps `mmap`/`munmap` syscalls. Maps a file directly into the process's address space -- reads are pointer dereferences (zero copy), the OS manages paging data in/out. Essential for embedded databases, large file processing, and any workload where explicit `read` calls are too slow. `Mmap` is a move type; `close` unmaps. Runtime C shim wraps `mmap(fd, length, PROT_READ|PROT_WRITE, MAP_SHARED, ...)`.
- `std.io` -- **DONE** `IO.puts`, `IO.warn`, `IO.write`, `IO.gets` for ergonomic console I/O. `STDIN`, `STDOUT`, `STDERR` as `Fd` constants. `IO.gets` implemented in pure Expo via recursive `STDIN.read(1)`.
- `std.debug` -- **DONE** `Debug` protocol with `format(self) -> String` and `inspect(move self) -> Self` (tap-style). Compiler-derived implementations for all types: primitives via intrinsics, enums as `VariantName` / `VariantName(payload)`, structs as `StructName{field: value, ...}`. `print` and string interpolation dispatch through `Debug.format()`.
- `std.system` -- **DONE** `System.get_env(key) -> Option<String>`, `System.set_env(key, value)`, `System.cwd() -> Result<String, String>`, `System.hostname() -> String`. Genuinely global OS state operations not tied to any process's C/M/R types. Thin wrappers over C stdlib calls via runtime intrinsics.
- `std.time` -- **DONE** `DateTime.now() -> DateTime`, `DateTime.timestamp_millis(self) -> Int` for wall-clock time. `Duration.from_secs(Int) -> Duration`, `Duration.from_millis(Int) -> Duration`, `Duration.millis(self) -> Int` for time spans. `Duration` is pure Expo (no intrinsics). Only `DateTime.now()` requires a runtime shim.

The litmus test: does the compiler or language runtime need it to function, or is it a stable capability every program needs with an API that won't evolve? If yes, stdlib. If the API surface will evolve (protocols, connection management, serialization formats), it's a first-party package.

- **Done when**: `config.expo` compiles (exercises strings, file reading, option handling, duration)

#### Package manager

- `project.expo` extended with dependency declarations via git URLs with user-controlled local names
- Dependency resolution: fetch from git, lock file generation for reproducible builds
- **Done when**: `project.expo` resolves dependencies and builds the project

#### First-party packages

High-quality, officially maintained, but not part of the compiler release cycle. Protocols and algorithms evolve on their own timeline. Networking lives here because the API surface evolves (QUIC, io_uring, TLS integration, connection pooling) -- you don't want that locked into the stdlib release cycle.

- `net` -- networking primitives as submodules, one package, coordinated releases. Shared types (`IpAddr`, `SocketAddr`) used across submodules. Low-level `Socket` type already exists in `std.fd` with POSIX syscall shims (`socket`, `bind`, `listen`, `accept`, `setsockopt`) -- `net.tcp` wraps it with a higher-level API.
  - `net.tcp` -- `TcpListener` (bind + accept) and `TcpSocket` (connect + read + write + close). Both wrap `Fd` from stdlib. `TcpListener.accept()` returns a `TcpSocket` -- same type for server and client connections. Socket setup uses C shims in the runtime (already implemented); read/write/close go through `std.fd`.
  - `net.udp` -- `UdpSocket` with `bind`, `send_to`, `recv_from`. Datagram-oriented, no connections. Independent from TCP -- different semantics, different API shape.
  - `net.tls` -- `TlsSocket` wrapping a `TcpSocket` with encryption. `TlsSocket.wrap(move socket: TcpSocket, config: TlsConfig) -> Result<TlsSocket, TlsError>`. Same `read`/`write`/`close` interface. Thin wrapper over system TLS library (LibreSSL/OpenSSL/BoringSSL via C FFI). Programs that only import `net.tcp` don't pull in TLS dependencies.
- `http` -- HTTP server and client built on `net.tcp` / `net.tls`. Request parsing, routing, response building, middleware. Server spawns a process per connection using `Process<C, M, R>`. Binary pattern matching for protocol parsing. `http.client` for outbound requests.
- `websocket` -- WebSocket server and client built on `http` (upgrade handshake) and `net.tcp` (framed message transport). Each WebSocket connection is a process -- natural fit for Expo's concurrency model. Frame parsing via binary pattern matching.
- `json` -- `JSONValue` enum, recursive descent parser, encoder (compact and pretty-printed). Already implemented as a standalone `json` package in pure Expo, validated with 17 tests. Remaining: convenience methods (`as_string()`, `as_int()`), decoder combinator API for API input boundaries with error accumulation.
- Crypto: hashing, random bytes (thin wrapper over libsodium or similar)
- Structured logging
- MessagePack serialization
- UUID generation, regex, URL parsing
- **Done when**: `handlers.expo` compiles using stdlib + first-party packages -- it exercises HTTP, JSON, crypto, logging, and UUID generation

#### Approach

Implement natively in Expo wherever possible. Use thin C FFI only for security-critical crypto and performance-critical parsing.

### Track B: Runtime + Reliability

#### Multi-threaded scheduler + I/O

Work-stealing M:N scheduler. I/O reactor (kqueue on macOS, epoll on Linux). Can start with a simple multi-threaded round-robin before optimizing to work-stealing.

**No dependencies on Phase 3 B1-B3 or Track A.** The `Process<C, M, R>` protocol and `spawn`/`receive` work identically regardless of how many OS threads the scheduler uses underneath.

- **Scheduler protocol** -- the runtime is defined as a protocol interface (`spawn_process`, `send_message`, `yield`, `park`/`wake`, `poll_io`), not a monolithic scheduler. The native runtime is one implementation; others (WASM, testing, embedded, debug) implement the same interface.
- **Container-aware thread count** -- detect cgroup CPU limits (`/sys/fs/cgroup/cpu.max` on cgroups v2) for scheduler thread count, not host CPU count. A pod with `resources.limits.cpu: 2` on a 96-core host should spawn 2 scheduler threads. Fall back to `available_parallelism` on bare metal.
- **Idle thread parking (default: no spin)** -- idle scheduler threads park on a condvar/futex when no work is available, consuming zero CPU. No busy-wait by default. Configurable via `EXPO_SCHEDULER_BUSYWAIT=none|short` environment variable: `none` (default, container-safe) parks immediately; `short` spins briefly before parking for ~1-5 microsecond lower steal latency on bare metal with dedicated cores. BEAM's `+sbwt short` default caused silent CFS quota burn in Kubernetes deployments -- Expo avoids this by defaulting to the container-safe behavior.
- **Graceful SIGTERM handling** -- K8s sends SIGTERM with a configurable grace period (default 30s). The scheduler stops accepting new spawns, drains in-flight processes, and exits cleanly. Processes that don't exit in time are killed on SIGKILL.
- Timer wheel for timeouts, intervals, and deadlines
- Process lifecycle manager (start, stop, crash detection)
- All functions can suspend; the runtime handles it -- no function coloring
- **System intrinsics via the runtime** -- `expo-runtime` is the gateway between Expo code and the OS. Beyond scheduling, it provides native functions for time (`expo_time_now_millis`), file I/O, random bytes, and other syscall-dependent operations. The compiler emits calls to these functions as intrinsics (same pattern as `spawn`/`send`/`receive`). Pure Expo types in the stdlib wrap them with ergonomic APIs (`DateTime.now()`, `File.read()`, etc.). This avoids a full C FFI while keeping system access centralized in one linked library. A general FFI for third-party native bindings is a later concern.
- **Done when**: 10,000 processes run concurrently with correct multi-threaded scheduling

#### Supervision prerequisites

Three features whose primary use cases are supervision constructs:

- **`Pid` type** -- type-erased process ID (raw integer). Used in `ExitSignal` (which carries the crashed process's pid), registries, and `Process.monitor`. Distinct from `Ref<M, R>` (typed handle).
- **Trait bounds on generics** -- `fn foo<T: Process<C, M, R>>(x: T)` needed for `child_spec` and generic process utilities. Parser currently only accepts bare `<T>`, needs `:` bound syntax. Touches parser, type checker, and codegen.
- **`copy` keyword** -- third parameter modifier alongside default borrow and `move`: `fn start(copy config: Config)`. Auto-clones at the call boundary. Primary use case: `child_spec` default impl captures `copy config` in a closure for supervisor restart. `PassMode::Copy` already exists for closure captures; this extends it to parameter declarations. See `archive/20260323-CONCURRENCY.md` for full design.

#### Preemption and priority

- Compiler-inserted yield checks at function call preambles and loop back-edges
- Priority levels (`Low`, `Normal`, `High`) control process scheduling budget -- higher priority processes get more CPU time before yielding
- Processes default to `Normal` priority; configurable at spawn time
- **Done when**: a low-priority process yields to high-priority processes under load

#### Supervision

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

#### Process discovery

- **Runtime-level global registration** -- `Process.register(ref, "name")` and `Process.whereis<M, R>("name")` returning `Option<Ref<M, R>>`. Simple `name -> Pid` mapping. Good for well-known singletons.
- **Registry as stdlib process** -- typed `Registry` process for dynamic, scoped registries (worker pools, connection managers). Monitors entries via `ExitSignal`, auto-removes dead entries.
- **Done when**: processes can be registered by name and looked up with `Option<Ref<M, R>>`

#### Shared data

- `shared_map` (stdlib concurrent hash map, needs a proper name) for shared caches across processes
- `put` moves values in (ownership transfer, no races)
- `get` borrows values out (zero-copy read access)
- `delete` removes and drops values
- Solves the two core problems of shared state: memory explosion from copying, and corruption from concurrent modification
- **Done when**: multiple processes read/write a `shared_map` without corruption

### `fn main` as `Process<C, M, R>`

Design area needing its own doc. `fn main` fully embodies the Process protocol:

- **C = `List<String>`** -- argv, the config passed to construct main
- **M = OS signals** -- `Signal.Term`, `Signal.Int`, etc. delivered as typed messages to main's receive loop, enabling graceful shutdown propagation to child processes
- **R = exit code** -- the integer returned to the OS when main finishes

This means no special `System.argv()`, `System.exit()`, or signal handler APIs. Argv is main's config, exit codes are main's return, signals are messages in main's receive loop. The OS/process boundary maps onto the same `Process<C, M, R>` protocol that every other process uses.

### Risks

- **Preemption yield-check overhead**: every function call and loop back-edge gets a yield check. Must be cheap (single counter decrement + branch). Profile to ensure overhead stays under 1-2%.
- **Runtime complexity**: building a work-stealing scheduler with I/O integration is substantial engineering. Start with round-robin and single-threaded I/O, then scale up.
- **Scheduler protocol scope**: the protocol must be minimal enough that a single-threaded WASM backend can implement it, but expressive enough that the native M:N scheduler isn't constrained. Err on the side of too-minimal.
- **`shared_map` naming**: needs a proper name before 1.0. Candidates TBD.

---

## Phase 5: Tooling maturity

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

## Phase 6: Self-hosting + Validation

Two independent tracks. The compiler can be self-hosted without building a web service. auth-manager-expo can be built with the Rust bootstrap. Feedback from validation surfaces language gaps that feed back into the compiler, but that's true at every phase.

### Track A: Self-hosting

Rewrite the Expo compiler in Expo. The lexer port from Phase 3 A4 (validation) provides a head start -- it was compiled by the Rust bootstrap to validate the language, but now gets promoted to the real compiler.

#### Port the parser

- Rewrite the parser from Rust to Expo (the lexer is already ported from Phase 3)
- This is a stress test of the language for non-trivial recursive descent code
- Expect to discover language shortcomings -- feed them back into design
- **Done when**: the Expo-written parser can parse all `.expo` files identically to the Rust parser

#### Introduce ExpoIR and the codegen backend protocol

- Split `expo-codegen` into two stages: lowering (TypedAST → ExpoIR) and emission (ExpoIR → target output)
- ExpoIR is a flat, lowered representation -- monomorphized, closures desugared, drops inserted. Simple enough that writing a new backend is a tractable project.
- Define `CodeEmitter` as an Expo protocol. The LLVM backend is `impl CodeEmitter for LlvmEmitter`. Cranelift, WASM, and C backends implement the same interface.
- Publish `expo-ir` and the backend protocol as packages so third parties can build custom backends.
- **Done when**: the LLVM backend works through ExpoIR with no regressions, and a second backend (Cranelift for the REPL) compiles a non-trivial program.

#### Port type checking and codegen

- Rewrite semantic analysis, type checker, and LLVM codegen in Expo
- LLVM bindings via C FFI (Expo calling into the LLVM C API)
- **Done when**: the Expo-written compiler can compile itself (the compiler compiles itself)

#### Retire the bootstrap

- Run the full test suite through the self-hosted compiler
- Fix any remaining differences between Rust bootstrap output and Expo self-hosted output
- The Rust bootstrap is now only needed for bootstrapping from scratch
- **Done when**: `expo build` using the self-hosted compiler produces identical binaries to the Rust bootstrap for all test programs

### Track B: Validation

#### Compile auth-manager-expo for real

- Take the 17 `.expo` pseudocode files in this repo and make them compile and run as an actual service
- Fix any gaps between the pseudocode and what the compiler actually supports
- Run the auth-manager test suite (ported from the Rust version)
- Benchmark against the Rust original: binary size, memory usage, request latency, startup time
- **Done when**: auth-manager-expo runs in production handling real traffic

#### Build a second project

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

### `Debug` protocol and `print` (decided, implemented)

- **Done**: `Debug` protocol in `std.debug` with `format(self) -> String` (required) and `inspect(move self) -> Self` (default impl, tap-style debugging). Named `Debug` rather than `Display` to reflect developer-facing output (not user-facing presentation).
- **Auto-derived**: all structs and enums get a compiler-synthesized `format` implementation. Enums print as `VariantName` (unit) or `VariantName(value)` (tuple payload). Structs print as `TypeName{field: value, ...}`. Primitives use codegen intrinsics. Users can override with their own `impl Debug for MyType`.
- **`print` and interpolation**: `print(value)` and `"#{value}"` dispatch through `Debug.format()` instead of hardcoded printf format specifiers. Any type can be printed or interpolated.
- **`std.io`**: `IO.puts`, `IO.warn`, `IO.write` accept `String` only -- callers use interpolation or `.format()` for non-string types. `IO.gets` reads a line from stdin.

### ExpoIR and codegen backend protocol

- **Planned**: introduce an intermediate representation (`expo-ir`) between the type checker and codegen. The IR is a lowered, flat representation -- no generics (already monomorphized), no closures (already desugared to structs + function pointers), no high-level control flow (already lowered to branches). Just functions, calls, loads, stores, branches.
- **Motivation**: the current `expo-codegen` crate mixes two concerns -- lowering (closure desugaring, monomorphization, drop insertion) and emission (inkwell LLVM calls). Separating them creates a clean interface for multiple codegen backends.
- **Backend protocol**: codegen backends implement a `CodeEmitter` protocol against ExpoIR. The LLVM backend (current) is the first implementation, not a special case. Other backends become possible: Cranelift (fast compilation for the REPL), direct WASM emission (smaller output for edge), C emission (maximum portability), or an interpreter (scripting, hot-reload).
- **Compiler pipeline**: `Source → AST → TypedAST → ExpoIR → [CodeEmitter backend] → output`. Lowering happens once; backends only handle "emit a function call" and "emit a branch," not "figure out how closures capture variables."
- **Public API**: ExpoIR and the backend protocol would be published as packages after self-hosting, enabling third-party codegen backends. During bootstrap, they're Rust crates wrapping inkwell.
- **Build-time selection**: `project.expo` or `expo build --backend cranelift` selects the backend. One backend per binary. The compiler monomorphizes all emitter calls against the selected implementation -- no vtable overhead.
- **Timing**: the IR split is Phase 6 (self-hosting) work. The current crate boundaries (codegen depends on ast + typecheck, clean downward dependencies) already support this separation. Keeping `expo-codegen` internals organized now avoids a painful refactor later.

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

| Phase     | Milestone                                                                                                             |
| --------- | --------------------------------------------------------------------------------------------------------------------- |
| Bootstrap | Lexer, parser, type system, LLVM codegen -- native binaries from Expo source                                          |
| Tooling   | Formatter, `expo run`, VSCode extension, LSP, documentation generator                                                 |
| Core      | Generics, ownership, protocols, closures, collections, processes                                                      |
| Phase 3   | Binary/bitstring system, string stdlib, file I/O, project system, unions, `Process<C,M,R>`, `Task`, self-hosted lexer |
| Phase 4A  | Test runner, TCP socket support, `Debug` protocol, `std.io`, `std.file` (FileMode, write, exists?, delete, rename)    |

For detailed build history, see [archive/20260318-ROADMAP.md](archive/20260318-ROADMAP.md) and [archive/20260330-ROADMAP.md](archive/20260330-ROADMAP.md).

### Remaining

| Phase | Milestone                                                                                                                               |
| ----- | --------------------------------------------------------------------------------------------------------------------------------------- |
| 4A    | ~~Test runner~~, ~~`Debug` protocol~~, ~~`std.io`~~, ~~`std.file`~~, ~~`System` type~~, ~~time~~, package manager, first-party packages |
| 4B    | Multi-threaded scheduler, preemption, supervision, process discovery, `shared_map`                                                      |
| 5     | Documentation (doctests, search), LSP (autocomplete, type hints), REPL                                                                  |
| 6A    | Parser in Expo, ExpoIR + backend protocol, full compiler, retire bootstrap                                                              |
| 6B    | auth-manager-expo runs for real, second project                                                                                         |

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
