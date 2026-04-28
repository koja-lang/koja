# Expo Language Roadmap

Solo developer + AI assistance. Bootstrap in Rust, self-host in Expo.

---

## Current state

### Compiler

An 11-crate Rust workspace that compiles Expo source to native binaries via LLVM:

- `expo-ast` -- tokens, spans, AST node definitions, type representations (`Type`, `Primitive`, `FnParam`). Every `Expr` node carries a `resolved_type: Option<Type>` populated by the type checker.
- `expo-lexer` -- custom tokenizer
- `expo-parser` -- recursive descent parser (Pratt precedence for expressions)
- `expo-typecheck` -- type inference and semantic analysis
- `expo-codegen` -- LLVM IR generation via `inkwell`
- `expo-stdlib` -- build script auto-discovers `.expo` sources under `expo/lib/` and embeds them via `include_str!`
- `expo-fmt` -- opinionated code formatter
- `expo-doc` -- HTML documentation generator (askama templates, pulldown-cmark)
- `expo-runtime` -- multi-threaded process scheduler (C ABI static library linked into compiled binaries)
- `expo-driver` -- CLI binary (`expo`); builds BoringSSL via `boring-sys` and embeds `libcrypto.a` for `@link "crypto"` resolution
- `expo-lsp` -- language server (diagnostics, formatting, hover with inferred types, go-to-definition, AST-based dot completion and signature help)

### CLI

Nine commands: `expo new`, `expo build`, `expo run`, `expo check`, `expo test`, `expo format`, `expo doc`, `expo lex`, `expo parse`. All commands support multi-module projects.

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
- Lightweight processes -- structs implement `Process<C, M, R>` protocol. `spawn T.start(config)` creates a process and returns `Ref<M, R>`. `receive` blocks for messages with optional `after timeout` clause. Message type `M` can be any type (primitives, structs, enums). Backed by `expo-runtime` cooperative scheduler.
- **`Task<R>`** -- `Task.async(fn () -> R)` / `Task.await` for one-off async work on top of processes (`Ref<(), R>` + `call`); see `std.kernel` and `tests/lang/task.expo`.
- Primitives: `Int`, `Int8`, `Int16`, `Int32`, `UInt8`, `UInt16`, `UInt32`, `UInt64`, `Float`, `Float32`, `Bool`, `String`
- List literal syntax (`[1, 2, 3]`) backed by `ListLiteral<T>` protocol
- Map literal syntax (`["key": value]`, `[:]` empty) backed by `MapLiteral<K, V>` protocol
- `Self` type expression in `protocol` and `impl` blocks
- `Hash` and `Equality` protocols with intrinsic implementations for all primitives
- Stdlib types: `Option<T>`, `Result<T, E>`, `Pair<A, B>`, `Map<K, V>`, `Set<T>` (auto-imported from `std.kernel`); `Process<C, M, R>`, `Ref<M, R>`, `ReplyTo<R>`, `Task<R>`, `Step<S>`, `Lifecycle`, `StopReason`, `ExitStatus`, `ExitReason` (auto-imported from `std.process`); `CPtr<T>`, `CString` (auto-imported from `std.cptr`, `std.cstring`)
- `List<T>` iterator functions (`map`, `filter`, `any?`, `all?`) implemented as pure Expo code in `std.kernel`
- Bare function names as references (`f = double; f(5)`, `list.map(double)`) -- top-level functions produce closure-compatible fat pointers via thunk wrappers
- Union types (`A | B | C`) -- anonymous tagged unions with widening coercion, exhaustiveness checking, and `match` support with typed binding patterns (`p: Post -> p.title`)
- Named union aliases (`type FeedItem = Post | Comment | Ad`) -- `type` keyword declarations resolved in the type context
- Trait bounds on generic type parameters (`<T: Protocol>`, `<T: Proto1 & Proto2>`) -- bounds verified at call sites, protocol method resolution on bounded type vars in function bodies, `&` as protocol composition operator
- `alias` keyword for file-private package type shorthands (`alias json.Decoder`, `alias json.Decoder as JSONDecoder`) -- qualified type resolution via `package.Type` syntax
- C FFI Phase 1 (primitives) and Phase 2 (pointers and strings): `@extern "C"` and `@link "libname"` annotations for calling C functions. `CPtr<T>` raw pointer type (`Copy`, backed by `malloc`/`free`). `CString` null-terminated string type with `String.to_cstring()` and `CString.to_string()` conversions. `CPtr<T>` accepted in `@extern "C"` signatures for pointer-passing FFI.

### Parsed and type-checked but NOT yet in codegen

- `arena` blocks (deferred post-v1)

### Design notes

- **No tuples**: Expo does not have anonymous tuple syntax. `(a, b)` is grouping only. For multiple return values, use a struct. `Pair<A, B>` (with `.first` / `.second`) is available in the stdlib for lightweight two-value cases. 3+ values should always be a struct. Note: `(a, b)` pair syntax may return once protocols land via a `PairLiteral<A, B>` literal protocol -- this would be protocol-backed syntax, not a built-in tuple type, and is limited to arity 2.
- **`()` as the unit expression**: `()` is a "do-nothing" expression (empty closure that runs and returns nothing). Use `else -> ()` in `cond` for side-effect-only fallthrough.
- **Closures**: Block closures with explicit types and parens: `fn (a: Int32, b: Int32) -> Int32 ... end`. Mirrors function signature syntax. Short closures (`x -> expr`) with full capture support and context-driven parameter type inference at inline call sites. Used by `map`/`then` on `Option` and `Result`.
- **No private modules**: Files are modules, and all modules are importable. Access control lives at the function level (`priv fn`), not the module level. Use `@doc false` on types to signal "internal, don't depend on this" -- a documentation-level convention, not a compiler wall. This matches Elixir's approach and avoids the complexity of Rust's `pub(crate)` or Go's `internal/` directory enforcement.
- **PascalCase primitives and type simplification** (done): Primitives renamed from `i32`/`i64`/`f32`/`f64`/`bool`/`string` to PascalCase: `Int` (64-bit default), `Int32`, `Float` (64-bit IEEE default), `Float32`, `Bool`, `String`. User-defined types (`Pair`, `User`) and language types (`Int`, `String`) are now visually uniform. `Decimal` will ship in the stdlib as an exact-arithmetic type for financial/business logic, sitting alongside the primitives with no visual distinction.
- **`ref T` syntax** (parsed, deferred): Reference types use `ref T` (space, no angle brackets) instead of `ref<T>`. `ref` is a lowercase keyword modifier, consistent with the modifier pattern (`const`, `priv`, `move`, `ref`): lowercase keywords modify the thing that follows them, PascalCase names are always types. However, `ref T` is redundant in parameter position (borrow-by-default) and unsafe in return position without lifetime tracking. Deferred until a concrete use case emerges.
- **Map literal syntax** (decided): `[key: value, key: value]` with `[:]` for empty maps. Maps are collections (like `List<T>`), not struct-like, so they share the bracket family rather than curly braces. The parser disambiguates list vs. map by peeking for `:` after the first expression. Curly braces remain exclusive to struct construction (`Config{name: "yo"}`).
- **Subscript syntax** (deferred): `map["key"]` / `list[0]` as sugar for `.get()`. Would be backed by a protocol (e.g., `Subscript<K, V>`). Not needed yet -- method access (`map.get(key)`, `list.get(0)`) works. Can be added later without grammar conflicts since `[` after an expression is a different parse context than `[` at expression start.
- **Planned: Irrefutable struct destructuring**: `Config{name, port} = load_config()` as syntactic sugar for pulling struct fields into local variables. Compile-time verified exhaustive -- only works for structs (single shape), not enums. Enum destructuring uses `match`.

### Known gaps

See [GAPS.md](GAPS.md) for known compiler limitations and workarounds.

### Design artifacts

- **Language design** -- syntax decisions, memory model, async model, module system, all finalized through iterative design sessions
- **EBNF grammar** -- `grammar.ebnf`, ~460 lines covering all syntax constructs
- **Example codebase** -- 17 `.expo` files porting `auth-manager` (a real Rust microservice) into Expo pseudocode, validating the language feels right
- **Memory strategy** -- documented in `archive/20260323-MEMORY.md` (stack, ownership+move, explicit arena)
- **Concurrency model** -- documented in `archive/20260313-CONCURRENCY.md` and `archive/20260323-CONCURRENCY.md` (processes, native runtime, supervision)
- **Project config format** -- `expo.toml` (TOML-based, replacing `project.expo`)
- **Module system redesign** -- documented in `archive/20260403-IMPORT.md` (files as transparent, types as namespaces, no intra-project imports, qualified package access, `import` keyword removed)
- **ExpoIR design** -- live roadmap in [EXPOIR-ROADMAP.md](EXPOIR-ROADMAP.md); original SIL-style design and Wave 1-17 history in [archive/20260427-EXPOIR.md](archive/20260427-EXPOIR.md)

### Tooling (pulled forward)

- **Project scaffolding** -- `expo new <name>` creates a project directory with `expo.toml` and `src/main.expo`
- **Formatter** -- `expo format --write` / `--check`, opinionated and zero-config, handles escape re-encoding for round-trip correctness, preserves annotations (including multiple annotations per declaration -- stacked or inline)
- **LSP** -- `expo-lsp` binary providing real-time diagnostics, document formatting, hover (Markdown-rendered type signatures + `@doc` + inferred variable types), go-to-definition (including qualified module calls), AST-based dot completion, and signature help over stdio, integrated with the VSCode/Cursor extension
- **VSCode extension** -- syntax highlighting and LSP client for `.expo` files
- **Vim plugin** -- syntax highlighting, auto-indentation (`indentexpr`), `matchit` block matching (`fn`/`end`, `if`/`end`), and `:make` compiler integration with `expo check` and `errorformat` for quickfix
- **DWARF debug info** -- always-on source-level debug metadata. Every function and statement carries file/line/column metadata. `.dSYM` bundles generated on macOS. Enables debugger attachment and runtime stacktraces.
- **Runtime stacktraces** -- Elixir-style panic output with `(appname)`/`(stdlib)` frame labels, relative paths, demangled function names, generic parameter stripping, ANSI color (respects `NO_COLOR`), and contextual hints for known panic patterns

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
| Process protocol   | `Process<C, M, R>` with three type params. Config (C) separates public args from private state via `start`. `start` runs in the child process context, returns `Result<Self, StopReason>`. Handlers return `Step<Self>` for explicit control flow. Fixed reply type (R) per process -- same as a service contract. Union types for heterogeneous replies.                                             |
| Scheduler protocol | Define the runtime as a protocol interface before implementing any backend. The native scheduler is the first implementation, not a special case. Enables WASM targets, test runtimes, and third-party custom runtimes without changing user code.           |
| Native runtime     | A runtime library linked into the binary, not a VM. No bytecode, no GC. Similar to Go's runtime or Tokio, but with process lifecycle management.                                                                                                             |
| Typed mailboxes    | Processes declare message type M via protocol impl. `send` and `receive` are type-checked at compile time. Union types enable multi-source mailboxes (e.g., `PoolCmd \| ExitSignal`).                                                                        |
| Validation target  | The lexer port (A4) validated the language surface without requiring external dependencies (no network, no database, no JSON). The Rust compiler remains authoritative; the Expo lexer is compiled by it. ✓ Complete -- token output matches the Rust lexer. |

For detailed sub-milestone breakdowns (A1a-A1e, A2a-A2c, A3a-A3b, A4, B1-B3), see [archive/20260330-ROADMAP.md](archive/20260330-ROADMAP.md).

---

## Phase 4: Stdlib + Ecosystem / Runtime + Reliability

Two independent tracks. Track A makes the language useful for real programs. Track B makes concurrency production-grade. The convergence point is an HTTP server handling real traffic -- it needs both `net.tcp` (Track A) and the multi-threaded scheduler (Track B).

### Track A: Stdlib + Ecosystem

#### Test runner -- **DONE**

`expo test` discovers `@test`-annotated functions across `src/` and `test/` directories, generates a synthetic test harness, compiles and runs it. `@test` accepts an optional string description (`@test "adds two numbers"`). Abort-on-first-failure -- the test name is printed before each call so you always know which one failed. `expo.toml` has an optional `test` field (default `["test"]`). Validated with 17 tests in the `json` package (encoder and decoder coverage).

#### Stdlib -- **DONE**

Stdlib contains primitives that are as fundamental as integers -- things the compiler or language runtime needs to function, or that virtually every program needs and whose API is stable for decades.

- `std.fd` -- **DONE** shipped in Phase 3 A3a (basic `read`, `write`, `close`). `Fd`, `FileMode`, `File` remain here.
- `net` -- **DONE** (qualified stdlib package). POSIX socket primitives. `SocketKind` enum (`Stream`, `Datagram`), `IPAddress` struct (Binary-backed with `v4`, `loopback`, `any`, `v4?`, `v6?` helpers), `SocketAddress` struct. `Socket` with full POSIX surface: `create`, `bind`, `connect`, `resolve` (DNS via `getaddrinfo`), `send_to`, `recv_from`, `listen`, `accept`, `set_reuse_addr`, `close`. Ergonomic wrappers: `TCPSocket` (client connections with DNS resolution), `TCPListener` (server-side bind/accept returning `TCPSocket`), `UDPSocket` (connectionless datagram I/O). All wrappers are pure Expo on top of `Socket` -- no additional intrinsics. Accessed via `alias net.TCPSocket` or `net.TCPSocket.connect(...)`.
- `std.file` -- **DONE** `FileMode` enum (`Read`, `Write`, `Append`). `File.open(path, mode)` opens with explicit mode. `File.write(path, content)` for one-shot writes. `File.exists?(path)`, `File.delete(path)`, `File.rename(src, dst)` for path-based operations. Handle-level writes via `file.fd.write(data)`. `seek` deferred until embedded database work.
- `std.mmap` -- `Mmap` struct for memory-mapped files. Wraps `mmap`/`munmap` syscalls. Maps a file directly into the process's address space -- reads are pointer dereferences (zero copy), the OS manages paging data in/out. Essential for embedded databases, large file processing, and any workload where explicit `read` calls are too slow. `Mmap` is a move type; `close` unmaps. Runtime C shim wraps `mmap(fd, length, PROT_READ|PROT_WRITE, MAP_SHARED, ...)`.
- `std.io` -- **DONE** `IO.puts`, `IO.warn`, `IO.write`, `IO.gets` for ergonomic console I/O. `STDIN`, `STDOUT`, `STDERR` as `Fd` constants. `IO.gets` implemented in pure Expo via recursive `STDIN.read(1)`.
- `std.process` -- **DONE** process lifecycle types and the `Process` protocol. `ReplyTo<R>`, `Ref<M, R>`, `Task<R>` (moved from `std.kernel`). New types: `Step<S>` (`Continue(S)`, `Done(StopReason)`), `Lifecycle` (`Shutdown`, `Interrupt`, `Reload`), `StopReason` (`Normal`, `Shutdown`), `ExitStatus` protocol, `ExitReason` (`Normal`, `Shutdown`, `Crashed(String)`). `Process<C, M, R>` protocol: `start(move config) -> Result<Self, StopReason>` runs in child context, `handle` and `handle_signal` return `Step<Self>`, `run` returns `StopReason`, `handle_signal` has a default implementation.
- `std.debug` -- **DONE** `Debug` protocol with `format(self) -> String` and `inspect(move self) -> Self` (tap-style). Compiler-derived implementations for all types: primitives via intrinsics, enums as `VariantName` / `VariantName(payload)`, structs as `StructName{field: value, ...}`. `print` and string interpolation dispatch through `Debug.format()`.
- `std.system` -- **DONE** `System.get_env(key) -> Option<String>`, `System.set_env(key, value)`, `System.cwd() -> Result<String, String>`, `System.hostname() -> String`. Genuinely global OS state operations not tied to any process's C/M/R types. Thin wrappers over C stdlib calls via runtime intrinsics.
- `std.time` -- **DONE** `DateTime.now() -> DateTime`, `DateTime.timestamp_millis(self) -> Int` for wall-clock time. `Duration.from_secs(Int) -> Duration`, `Duration.from_millis(Int) -> Duration`, `Duration.millis(self) -> Int` for time spans. `Duration` is pure Expo (no intrinsics). Only `DateTime.now()` requires a runtime shim.

The litmus test: does the compiler or language runtime need it to function, or is it a stable capability every program needs with an API that won't evolve? If yes, stdlib. If the API surface will evolve (protocols, connection management, serialization formats), it's a first-party package.

- **Done when**: `config.expo` compiles (exercises strings, file reading, option handling, duration)

#### Package manager

- ~~`expo.toml` extended with dependency declarations~~ **Done.** `[dependencies]` table with local path support (`json = { path = "../json" }`). Dependency sources are scanned and merged into the module graph.
- Git dependencies: `expo.toml` extended with git URLs, tags, and branches. Dependency resolution: fetch from git, lock file generation for reproducible builds.
- ~~`alias` keyword~~ -- **Done.** File-private shorthand for qualified package types. `alias json.Encoder` or `alias json.Decoder as JSONDecoder`. Scoped to the declaring file, doesn't pollute the flat project namespace. Parser, type checker, formatter, LSP, and doc extractor all handle `Item::Alias`. Package types tracked via `ModuleGraph.dep_packages` and `TypeContext.package_types`.
- **Done when**: `expo.toml` resolves git dependencies and builds the project

#### C FFI -- started

User-facing foreign function interface for calling C libraries. Expo already calls C internally (the runtime is a C library, and codegen emits calls to it via intrinsics). The FFI exposes this capability to user code. See [FFI.md](FFI.md) for the full design.

- ~~`@extern "C"` annotation on structs (pure binding namespace) or individual functions (mixed with Expo code). No new keywords -- uses existing annotation system. Multiple annotations per declaration supported (`@link "argon2" @extern "C"`).~~ **Done** (Phase 1)
- ~~`@link "libname"` annotation on structs or functions; `[link]` table in `expo.toml` for search paths~~ **Done** (Phase 1)
- ~~`CPtr<T>` type for raw pointers (`CPtr.null()`, `CPtr.alloc()`, `ptr.free()`, `ptr.offset()`, `ptr.read()`, `ptr.write()`, `ptr.null?()`). `CPtr<T>` is `Copy` (just a machine word).~~ **Done** (Phase 2)
- ~~`CString` for null-terminated C string interop (`string.to_cstring()`, `cstring.to_string()`)~~ **Done** (Phase 2)
- ~~Codegen: extern functions emit LLVM `declare` with C calling convention. Same pattern the compiler already uses for runtime intrinsics. Extern function names are unmangled.~~ **Done** (Phase 1)
- ~~Linker: `-l` flags for system libraries, static archive paths for vendored libraries. Already works for `libc` and `expo-runtime`.~~ **Done** (Phase 1)
- `@compat "C"` on structs for C-compatible memory layout
- Phased: ~~primitives-only first~~ **Done**, ~~then `CPtr<T>`/`CString`~~ **Done**, then `@compat "C"` structs and callbacks
- **Done when**: an `argon2` wrapper package calls `libargon2` to hash and verify passwords

#### Standard library packages

`net`, `http`, `json`, `random`, and `crypto` are stdlib packages -- they ship with the compiler, are always available, and use qualified imports (`net.TCPSocket`, `http.Request`). See [STDLIB.md](STDLIB.md) for the full package hierarchy design.

- `net` -- **DONE** `TCPSocket`, `TCPListener`, `UDPSocket` implemented as the `net` qualified stdlib package (`net.expo`). TLS support (`upgrade_tls`, `TLSConfig`) pending.
- `http` -- shared vocabulary types (`Request`, `Response`, `Method`, `Status`, `Headers`), HTTP/1.1 parser, one-shot client, spawn-per-connection server.
- `json` -- **DONE** (qualified stdlib package). `json.Value` (renamed from `JSONValue`), `json.Encoder`, `json.Decoder`, `json.StringBuilder`. Pure Expo, 17 tests. Accessed via `alias json.Value` or `json.Value.object(...)`.
- ~~`random`~~ -- **Done.** `Random.bytes(n)` and `Random.int(min, max)` landed in `std.kernel` (auto-imported, no package qualifier needed). OS entropy via `getrandom(2)` / `getentropy(2)`, no OpenSSL dependency. Decided against a separate package -- too small, too fundamental.
- `crypto` -- **DONE** (qualified stdlib package). Direct BoringSSL C FFI via `@extern "C"` + `@link "crypto:symbol"`. Full SHA family: `crypto.SHA1`, `crypto.SHA256`, `crypto.SHA384`, `crypto.SHA512` -- each with one-shot `digest(data)` and streaming (`new`, `update`, `finalize`) APIs. `crypto.HMAC` with `sha1`, `sha256`, `sha384`, `sha512` methods. All functions accept and return `Binary`. Expo function names follow `snake_case`; C symbols specified via `@link "crypto:EVP_sha256"` convention. `libcrypto.a` is embedded in the compiler and written to a temp dir at link time -- zero user setup. Accessed via `alias crypto.SHA256` or `crypto.SHA256.digest(...)`.

#### First-party packages

High-quality, officially maintained, but not part of the compiler release cycle. Protocols and algorithms evolve on their own timeline.

- `websocket` -- WebSocket server and client built on `http` (upgrade handshake) and `net` (framed message transport). Each WebSocket connection is a process.
- `http2` -- HTTP/2 transport package reusing stdlib `http.Request`/`http.Response` types.
- `argon2` -- password hashing via libargon2 C FFI. `Argon2.hash(password)`, `Argon2.verify(password, hash)`.
- `bcrypt` -- password hashing via bcrypt C FFI. Same API shape as `argon2`.
- Structured logging
- MessagePack serialization
- UUID generation, regex, URL parsing
- **Done when**: `handlers.expo` compiles using stdlib + first-party packages -- it exercises HTTP, JSON, argon2, logging, and UUID generation

#### Approach

Implement natively in Expo wherever possible. Use C FFI for security-critical crypto (don't roll your own) and system library bindings (TLS, databases). Wrapper packages provide safe Expo APIs so end users never touch pointers.

### Track B: Runtime + Reliability

#### Multi-threaded scheduler + I/O -- started

Multi-threaded round-robin scheduling and I/O reactor are implemented. Work-stealing is next.

**No dependencies on Phase 3 B1-B3 or Track A.** The `Process<C, M, R>` protocol and `spawn`/`receive` work identically regardless of how many OS threads the scheduler uses underneath.

**Done:**

- ~~**Multi-threaded round-robin**~~ -- N worker OS threads share a `Mutex`-protected process list. Each worker runs a scheduling loop: lock, find a runnable process, unlock, context-switch, persist saved SP on return. Per-worker thread-local state (`CURRENT_PID`, `SCHED_SP`, `YIELD_SP`) via `thread_local!`. C ABI unchanged -- existing programs gain multi-core scheduling with no code changes.
- ~~**Container-aware thread count**~~ -- reads cgroup v2 CPU quota (`/sys/fs/cgroup/cpu.max`) on Linux; falls back to `available_parallelism` on macOS and bare-metal Linux.
- ~~**Idle thread parking (no spin)**~~ -- idle workers park on a `Condvar` when no work is available, consuming zero CPU. Woken by `send`, `spawn`, or deadline expiry.
- ~~**Graceful shutdown**~~ -- `AtomicBool` shutdown flag set when the main process (PID 1) dies. All workers exit their loops and join. Deadlock detection for all-blocked-without-timeout scenarios.
- ~~**I/O reactor**~~ -- non-blocking socket I/O via the `polling` crate (kqueue on macOS, epoll on Linux). Sockets are `O_NONBLOCK`; on `EAGAIN`, processes enter `WaitingIo` state and the reactor wakes them on readiness. Dedicated reactor thread alongside workers. Covers `accept`, `connect`, `recv_from`, `send_to`, `fd_read`, `fd_write`. DNS and file I/O stay blocking.

**Remaining:**

- **Scheduler protocol** -- the runtime is defined as a protocol interface (`spawn_process`, `send_message`, `yield`, `park`/`wake`, `poll_io`), not a monolithic scheduler. The native runtime is one implementation; others (WASM, testing, embedded, debug) implement the same interface.
- **Work-stealing** -- upgrade from round-robin to per-thread run queues with lock-free work-stealing for lower contention under high process counts.
- **Graceful SIGTERM handling** -- K8s sends SIGTERM with a configurable grace period (default 30s). The scheduler stops accepting new spawns, drains in-flight processes, and exits cleanly. Processes that don't exit in time are killed on SIGKILL.
- Timer wheel for timeouts, intervals, and deadlines
- Process lifecycle manager (start, stop, crash detection)
- All functions can suspend; the runtime handles it -- no function coloring
- **System intrinsics via the runtime** -- `expo-runtime` is the gateway between Expo code and the OS. Beyond scheduling, it provides native functions for time (`expo_time_now_millis`), file I/O, random bytes (`expo_random_bytes`, `expo_random_int`), and other syscall-dependent operations. The compiler emits calls to these functions as intrinsics (same pattern as `spawn`/`send`/`receive`). Pure Expo types in the stdlib wrap them with ergonomic APIs (`DateTime.now()`, `File.read()`, etc.). The user-facing C FFI (Track A) generalizes this pattern for third-party native bindings.
- **Done when**: 10,000 processes run concurrently with correct multi-threaded scheduling

#### Supervision prerequisites

Three features whose primary use cases are supervision constructs:

- **`Pid` type** -- type-erased process ID (raw integer). Used in `ExitSignal` (which carries the crashed process's pid), registries, and `Process.monitor`. Distinct from `Ref<M, R>` (typed handle).
- ~~**Trait bounds on generics**~~ -- **Done.** `<T: Protocol>` and `<T: Proto1 & Proto2>` syntax. Bounds verified at call sites, protocol method calls resolved on bounded type vars in function bodies. `&` is the protocol composition operator (complement to `|` for unions).
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
- **`ChildSpec` struct** -- holds `start: fn() -> Pid` and `strategy: RestartStrategy`. The closure captures `copy config` and calls `start` + `spawn`, enabling type-erased restart.
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

### `fn main` as `Process<C, M, R>` -- started

Dual entry mode is implemented. `expo.toml` `entry` field determines behavior by casing: lowercase names a module with `fn main`, PascalCase names a `Process` type. Codegen generates a C `main` that constructs the entry type, spawns it, and starts the scheduler. See [FNMAIN.md](FNMAIN.md) for full design.

**Done:**

- ~~Dual entry mode~~ -- `entry = "App"` (PascalCase) spawns a Process impl; `entry = "main"` (lowercase) retains `fn main`. Both coexist.
- ~~Exit code mapping~~ -- `StopReason` returned by `run` is captured via a `@__expo_exit_code` global, mapped through `ExitStatus.code()`, and returned from C `main`. `Normal -> 0`, `Shutdown -> 1`.
- ~~argv passing~~ -- when `C = List<String>`, the entry's C `main(argc, argv)` calls `expo_rt_build_argv` to construct an Expo `List<String>` (skipping `argv[0]`). Other config types remain zero-initialized.

**Remaining:**

- **Lifecycle event delivery** -- OS signals (`SIGTERM`, `SIGINT`, `SIGHUP`) need to be captured by the runtime and dispatched as `Lifecycle` events to the entry process's `handle_signal`.
- **`expo new` scaffolding** -- update to generate Process entry instead of `fn main`.
- **Remove `fn main`** -- once `.exps` scripts are implemented, `fn main` becomes unnecessary and can be removed.

### Risks

- **Preemption yield-check overhead**: every function call and loop back-edge gets a yield check. Must be cheap (single counter decrement + branch). Profile to ensure overhead stays under 1-2%.
- **Runtime complexity**: building a work-stealing scheduler with I/O integration is substantial engineering. Start with round-robin and single-threaded I/O, then scale up.
- **Scheduler protocol scope**: the protocol must be minimal enough that a single-threaded WASM backend can implement it, but expressive enough that the native M:N scheduler isn't constrained. Err on the side of too-minimal.
- **`shared_map` naming**: needs a proper name before 1.0. Candidates TBD.

---

## Phase 5: Tooling maturity

### Documentation -- started

- ~~`expo doc` -- generates static HTML documentation from `@doc` annotations~~
- ~~Markdown rendering in doc strings (via pulldown-cmark)~~
- ~~`@doc false` to exclude items from docs~~
- ~~Flat namespace sidebar navigation across all items~~
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
- ~~Hover showing type signatures and `@doc`~~
- ~~Restart Language Server command~~
- ~~Autocomplete for struct fields and methods (AST-based dot completion using `resolved_type`)~~
- ~~Signature help for function and method calls (AST-based `find_enclosing_call`)~~
- ~~Hover shows inferred types for variables (e.g. `x: Int32`)~~
- Inline type hints for inferred types (inlay hints)
- Multi-module resolution (cross-file diagnostics)
- **Done when**: editing `.expo` files in Cursor shows real-time errors and supports go-to-definition

### Interactive shell (REPL)

- `expo shell` -- evaluate expressions and statements interactively, one at a time
- `expo shell -S .` -- load a project so you can call your functions, inspect types, and explore live
- Inline documentation: `h module.function` pulls from `@doc` annotations
- Tab completion for module names, functions, and variables in scope
- Backend: LLVM JIT (via inkwell `ExecutionEngine`) initially; Cranelift JIT long-term for faster response
- **Done when**: `expo shell -S .` loads a multi-module project and you can call functions, inspect results, and read docs interactively

### CLI query and guide system

Machine-readable access to language knowledge, serving humans, AI agents, and tooling with one investment.

- `expo query type <Type>` -- print a type's signature, fields/variants, functions, and `@doc` strings. Data comes from the type checker (always correct). Output is plain text, structured for both human reading and AI consumption.
- `expo query module <package>` -- list all types and public functions in a package.
- `expo query protocol <Protocol>` -- print protocol methods and known implementors.
- `expo guide <topic>` -- print a prose guide on a language concept. Guides are markdown files shipped with the compiler (embedded like stdlib sources), versioned with the language. Topics: `ownership`, `concurrency`, `ffi`, `protocols`, `testing`, etc.
- Each `expo doc` HTML page corresponds to one type (already the plan). `expo query` is the CLI equivalent -- same data, text output instead of HTML.
- Long-term, AI tooling rules reduce to "use `expo query` and `expo guide` to look up types, APIs, and language concepts." The CLI is the single source of truth.
- **Done when**: `expo query type TCPSocket` prints the full public API with doc strings, and `expo guide ownership` prints a readable explanation of the ownership model

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
- **Status note**: foundation work has been pulled forward into Phase 4 -- `expo-ir` crate exists with decision types and semantic lowering helpers in active use; the IR instruction containers themselves will be designed bottom-up during the lowering/emission split. See [EXPOIR-ROADMAP.md](EXPOIR-ROADMAP.md) for current state and remaining slices ([archive/20260427-EXPOIR.md](archive/20260427-EXPOIR.md) preserves the Wave 1-17 narrative).
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
- `Self` works inside union types (e.g., `-> Self | StopReason`) in both type checker and codegen.
- **Implementation**: centralized resolution via `fn_state.self_type_name` (codegen) and a shared `resolve` closure (type checker). No special-case methods -- `Self` is injected into the standard type resolution path as a type parameter with automatic substitution.
- Syntax highlighting and formatter support included.

### `impl` and protocols (decided, implemented)

- **Decided**: `protocol` keyword defines behavioral contracts. `impl Protocol for Type` for conformance. Bare `impl Type` survives for direct method attachment.
- **Implemented**: protocol declarations with function signatures, `impl Protocol for Type` blocks with completeness and signature validation, `priv fn` helpers in impl blocks, `@doc` on protocol declarations.
- **Decided**: static dispatch via monomorphization -- no vtables, no dynamic dispatch. Consistent with the existing generic compilation model.
- **Implemented**: `impl` blocks on all types (structs, enums, primitives) -- functions stored in a unified `TypeContext.types` registry via `TypeInfo { functions, type_params, kind: TypeKind, span }`. `TypeKind` discriminates `Struct` (with fields), `Enum` (with variants), and `Primitive`. Previously, structs, enums, and primitives each had separate storage (`StructInfo`, `EnumInfo`, `primitive_methods`); the unified registry eliminates duplicated lookup/dispatch logic across the type checker and codegen. Used for conversion intrinsics, protocol methods, user-defined functions, and static/instance dispatch.
- ~~**Open**: trait bounds on generic type parameters (`fn foo<T: Display>(x: T)`)~~ -- **Done.** `<T: Protocol>` and `<T: Proto1 & Proto2>` syntax with `&` as the protocol composition operator.
- ~~**Open**: whether bare `impl Type` eventually migrates to inline functions in type bodies, or both coexist permanently.~~ -- **Done.** Inline functions are now supported in both `struct` and `enum` bodies. Both `fn` and `priv fn` can appear alongside fields/variants. `impl` remains for external extensions and protocol conformance (analogous to Swift extensions). Static functions (no `self`) are also allowed, making types act as namespaces.

### Concrete impl specialization (implemented)

- **Decided**: `impl Type<ConcreteArg>` blocks define methods that only exist for a specific type argument. Methods in specialized impls are not available on other instantiations of the generic type.
- **Implemented**: `impl CPtr<UInt8>` adds `strlen` and `to_cstring` methods that only exist on `CPtr<UInt8>`, not on `CPtr<Int32>` or other instantiations. The type checker emits a targeted error with a hint when a specialized method is called on the wrong type argument.
- **Storage**: specialized impls are stored separately from generic impls in `TypeContext` (`specialized_impl_asts` and `specialized_methods`, keyed by `TypeIdentifier`). This avoids incorrect generic substitution that would occur if concrete type arguments were treated as type parameters.
- **Codegen**: `monomorphize_impl_method` checks specialized impl ASTs before falling back to generic impl ASTs. Self-type resolution handles non-struct types like `CPtr<T>` (which maps to `Type::Pointer`).
- **Constraint**: mixing concrete types and type parameters in the same impl block is a compile error (e.g., `impl Map<String, V>` is rejected).

### Struct field defaults and trailing keyword syntax (open)

Open design for default field values and trailing keyword call syntax. See [TYPES.md](TYPES.md) for full details.

### Type system philosophy

Enums and structs have equal capabilities (fractal design). See [TYPES.md](TYPES.md) for full details.

### Identifier priming for keyword/builtin collisions (future)

Trailing `'` notation for field/variant name collisions with keywords. Lexer-only change. See [TYPES.md](TYPES.md) for full details.

### Namespace unification: modules as pseudotypes (exploration)

Exploration of treating modules as `TypeKind::Module` in the unified registry. Not planned for immediate implementation. See [TYPES.md](TYPES.md) for full details.

### FP and chaining vs `?` operator (decided, implemented)

- **Decided**: no `?` operator -- removed from the toolchain. Hidden control flow violates the "no magic" principle -- the reader can't see that a function might return early without inspecting every line for `?`. Error handling uses explicit functions instead.
- **Decided**: no `?.` optional chaining (Swift-style). Would make `Option` a privileged type, breaking fractal design -- user-defined sum types wouldn't get the same syntax.
- **Decided**: `map`, `then`, `or` as the chaining API for `Option` and `Result`. `map` transforms the inner value (closure returns plain value). `then` chains fallible operations (closure returns `Option`/`Result`). `or` provides a lazy fallback. Approachable naming -- plain English, no `and_then`/`flat_map`/`unwrap_or`.
- `or` is implicitly lazy (compiler evaluates the argument only if needed, like `||`). No separate `or_else`.
- Compiler guidance when `map` is used where `then` is needed (or vice versa).
- **Decided**: no pipe operator (`|>`). Dot-call chaining with `move self` functions covers the same use case. The `Command` stdlib type (see [TYPES.md](TYPES.md)) will handle complex sequential data flow with stronger guarantees.
- `map`/`then` ship in the stdlib using block closures or short closures. Context-driven param inference allows `opt.map(x -> x + 1)` without type annotations at inline call sites.

### Stdlib design (implemented + planned)

- **Done**: `std.kernel` for core types (`Option<T>`, `Result<T, E>`, `Pair<A, B>`), auto-imported into every module. `std.process` for process lifecycle types. Both embedded in the compiler via `include_str!`, parsed at startup.
- **Planned**: two-tier stdlib with auto-imported core types and qualified standard packages (`net`, `http`, `json`). See [STDLIB.md](STDLIB.md) for the package hierarchy design.
- Monomorphization ensures zero binary bloat for unused stdlib types. Only instantiations that are actually called get compiled.

### `Debug` protocol and `print` (decided, implemented)

- **Done**: `Debug` protocol in `std.debug` with `format(self) -> String` (required) and `inspect(move self) -> Self` (default impl, tap-style debugging). Named `Debug` rather than `Display` to reflect developer-facing output (not user-facing presentation).
- **Auto-derived**: all structs and enums get a compiler-synthesized `format` implementation. Enums print as `VariantName` (unit) or `VariantName(value)` (tuple payload). Structs print as `TypeName{field: value, ...}`. Primitives use codegen intrinsics. Users can override with their own `impl Debug for MyType`.
- **`print` and interpolation**: `print(value)` and `"#{value}"` dispatch through `Debug.format()` instead of hardcoded printf format specifiers. Any type can be printed or interpolated.
- **`std.io`**: `IO.puts`, `IO.warn`, `IO.write` accept `String` only -- callers use interpolation or `.format()` for non-string types. `IO.gets` reads a line from stdin.

### ExpoIR and codegen backend protocol

See [EXPOIR-ROADMAP.md](EXPOIR-ROADMAP.md) for the live roadmap (current phase status, remaining slices, design invariants); [archive/20260427-EXPOIR.md](archive/20260427-EXPOIR.md) preserves the original SIL-style design (instruction set, ownership operations, shared type ARC, data structures, incremental self-hosting strategy).

- **Planned**: introduce an intermediate representation (`expo-ir`) between the type checker and codegen. The IR is a lowered, flat representation -- no generics (already monomorphized), no closures (already desugared to structs + function pointers), no high-level control flow (already lowered to branches). Just functions, calls, loads, stores, branches.
- **Motivation**: the current `expo-codegen` crate mixes two concerns -- lowering (closure desugaring, monomorphization, drop insertion) and emission (inkwell LLVM calls). Separating them creates a clean interface for multiple codegen backends.
- **Backend protocol**: codegen backends implement a `CodeEmitter` protocol against ExpoIR. The LLVM backend (current) is the first implementation, not a special case. Other backends become possible: Cranelift (fast compilation for the REPL), direct WASM emission (smaller output for edge), C emission (maximum portability), or an interpreter (scripting, hot-reload).
- **Compiler pipeline**: `Source → AST (with resolved_type) → ExpoIR → [CodeEmitter backend] → output`. The AST carries resolved types on every expression after typechecking -- there is no separate TypedAST struct. Lowering happens once; backends only handle "emit a function call" and "emit a branch," not "figure out how closures capture variables."
- **Public API**: ExpoIR and the backend protocol would be published as packages after self-hosting, enabling third-party codegen backends. During bootstrap, they're Rust crates wrapping inkwell.
- **Build-time selection**: `expo.toml` or `expo build --backend cranelift` selects the backend. One backend per binary. The compiler monomorphizes all emitter calls against the selected implementation -- no vtable overhead.
- **Timing**: the IR split is Phase 6 (self-hosting) work. The current crate boundaries (codegen depends on ast + typecheck, clean downward dependencies) already support this separation. Keeping `expo-codegen` internals organized now avoids a painful refactor later.

### Literal protocols

`ListLiteral<T>` and `MapLiteral<K,V>` are implemented. Remaining: `IntLiteral`, `FloatLiteral`, `StringLiteral`, `PairLiteral<A,B>`. See [TYPES.md](TYPES.md) for full details.

---

## Summary

### Done

| Phase     | Milestone                                                                                                             |
| --------- | --------------------------------------------------------------------------------------------------------------------- |
| Bootstrap | Lexer, parser, type system, LLVM codegen -- native binaries from Expo source                                          |
| Tooling   | Formatter, `expo new`, `expo run`, VSCode extension, LSP, documentation generator                                     |
| Core      | Generics, ownership, protocols, closures, collections, processes                                                      |
| Phase 3   | Binary/bitstring system, string stdlib, file I/O, project system, unions, `Process<C,M,R>`, `Task`, self-hosted lexer |
| Phase 4A  | Test runner, `net` package (POSIX surface), `Debug` protocol, `std.io`, `std.file`, `std.system`, `std.time`           |
| Tooling   | DWARF debug info, `--release` flag, runtime stacktraces, Vim plugin (indent, matchit, compiler)                       |
| Phase 4B  | Multi-threaded scheduler, cgroup-aware thread count, Condvar parking, graceful shutdown, I/O reactor                  |

For detailed build history, see [archive/20260318-ROADMAP.md](archive/20260318-ROADMAP.md) and [archive/20260330-ROADMAP.md](archive/20260330-ROADMAP.md).

### Remaining

| Phase | Milestone                                                                                                                                                                                                       |
| ----- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 4A    | ~~Test runner~~, ~~`Debug` protocol~~, ~~`std.io`~~, ~~`std.file`~~, ~~`System` type~~, ~~time~~, ~~`random`~~, package manager, ~~C FFI Phase 1-2~~, C FFI Phase 3, stdlib packages (`net`, `http`, ~~`json`~~, ~~`crypto`~~), first-party packages |
| 4B    | ~~Multi-threaded scheduler~~, work-stealing, ~~I/O reactor~~, preemption, supervision, process discovery, `shared_map`                                                                                          |
| 5     | Documentation (doctests, search), LSP (~~autocomplete~~, ~~signature help~~, inlay hints), REPL, CLI query/guide system                                                                                           |
| 6A    | Parser in Expo, ExpoIR + backend protocol, full compiler, retire bootstrap                                                                                                                                      |
| 6B    | auth-manager-expo runs for real, second project                                                                                                                                                                 |

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
