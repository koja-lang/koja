# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- Package private functions declared with top-level `priv fn` in files.
- `koja doc` now includes all stdlib and dependency docs in local files.
- Serve documentation locally with `koja doc serve`.
- `koja new` now scaffolds a `.gitignore`, a `test/` directory, and a `test/main_test.koja` placeholder.

### Changed

- **Breaking**: Project build output moved from `<project>/target/<profile>/` to `<project>/build/<profile>/`. Update any tooling or `.gitignore` entries referencing `<project>/target/`.
- **Breaking**: HTTP client methods moved from the `Http` struct to top-level functions in the `HTTP` package. `Http.get(url)` becomes `HTTP.get(url)`; same for `delete`, `head`, `options`, `patch`, `post`, `put`, and `request`. Use `alias HTTP` for unqualified access, or call qualified (`HTTP.post(url, body, headers)`). The `Http` struct is removed.
- **Breaking**: Bare `impl Type` blocks are renamed to `extend Type`. `impl` is now reserved for protocol conformance (`impl Protocol for Type`); writing `impl Point` produces a compile error pointing to `extend Point`. Methods declared in `extend` blocks have ambient visibility (callable from any package that can name the target type), and collisions on the same method name across `extend` blocks targeting the same type are a hard compile error. Update every inherent `impl Type ... end` to `extend Type ... end`; protocol implementations are unchanged.

## [0.10.0] - 2026-05-15

### Added

- Compiler rewritten around a four-phase sealed pipeline -- `koja-parser → koja-typecheck → koja-ir → koja-ir-llvm` / `koja-ir-eval`. The new pipeline unlocks the interpreter backend, the REPL, script mode, and faster `koja run` iteration; the LLVM backend continues to produce native binaries with the same feature surface.
- `koja shell` -- interactive REPL backed by the new interpreter. Multi-line input for blocks (`struct`, `enum`, `fn`, ...), up-arrow history within the session, and `:help` / `:quit` / `:reset` / `:state` commands. The trailing value of each input is rendered via `Debug.format()`, so `42`, `[1, 2, 3]`, and `User{name: "alice"}` all print without ceremony. No imports needed for stdlib calls (`Duration.from_secs(3).millis()` works at the prompt).
- `.kojs` script files -- top-level expressions and statements, no `fn main` required. `.koja` remains the project / module file extension; `.kojs` is the script-only sibling that targets the interpreter by default.
- `koja eval <script>` -- one-shot script evaluation. Equivalent to `koja run --backend=interpreter` on a `.kojs` file.
- `--backend={interpreter, llvm}` on `koja run` and `koja build`. `koja run` now defaults to the interpreter for millisecond startup; pass `--backend=llvm` for native execution. `koja build` defaults to and requires `llvm` (the interpreter does not produce a binary).
- `JSON` promoted to qualified stdlib package. Ships with the compiler -- no `[dependencies]` entry needed. `JSON.Value` (renamed from `JSONValue`), `JSON.Encoder`, `JSON.Decoder`, `JSON.StringBuilder`. Use `alias JSON.Value` or `JSON.Value.object(...)` to access.
- `Crypto` qualified stdlib package. Full SHA family via direct BoringSSL C FFI (`@extern "C"` + `@link "crypto"`). `SHA1`, `SHA256`, `SHA384`, `SHA512` -- each with one-shot `digest(data)` and streaming (`new`, `update`, `finalize`) APIs. `HMAC` with `sha1`, `sha256`, `sha384`, `sha512` methods. All functions accept and return `Binary`. No Rust shims -- Koja calls BoringSSL's C API directly. `libcrypto.a` is embedded in the compiler binary and written to the link temp dir alongside `libkoja_runtime.a`.
- `TCPSocket` -- ergonomic TCP client. `connect(host, port)` resolves DNS and establishes a connection. `connect_addr(addr)` for direct address connections. `read(count)`, `write(data)`, `close()`.
- `TCPListener` -- TCP server listener. `bind(port)` listens on all interfaces, `bind_addr(addr)` for specific addresses. `accept()` returns a `TCPSocket` for each incoming connection.
- `UDPSocket` -- connectionless UDP socket. `bind(port)` or `bind_addr(addr)` to receive, `send_to(data, addr)` and `recv_from(count)` for datagram I/O. All three types are pure Koja wrappers over `Socket` -- no new intrinsics.
- `CPtr<T>` -- raw C pointer type for FFI interop. `Copy` semantics (just a machine word). Methods: `CPtr.null()`, `CPtr.alloc(count)`, `ptr.free()`, `ptr.offset(n)`, `ptr.read()`, `ptr.write(value)`, `ptr.null?()`. Backed by `malloc`/`free`. All methods are compiler intrinsics.
- `CString` -- null-terminated C string type with `ptr: CPtr<UInt8>` and `len: Int` fields. `String.to_cstring()` allocates a null-terminated copy via `malloc`. `CString.to_string()` copies bytes back into an Koja `String`. `CString.free()` releases the underlying memory.
- `CPtr<T>` accepted in `@extern "C"` signatures, enabling pointer-passing FFI with C libraries. Koja-allocated buffers can be passed to C functions that read or write through pointers.
- Concrete impl specialization -- `impl Type<ConcreteArg>` blocks define methods only available for a specific type argument. `impl CPtr<UInt8>` adds `to_cstring()` without affecting other `CPtr<T>` instantiations. Targeted error messages when specialized methods are called on the wrong type argument.
- Bare function calls within a type now resolve same-type methods. `@extern "C"` private functions declared inline in a struct can be called by name from sibling methods in the same type.
- Plain struct patterns in `match` arms: `match p  Point{x: 5, y: 2} -> ...  end`. Unlisted fields are implicit wildcards (`Point{x: 5}` matches any `y`); empty `Point{}` matches any value of that struct type. Composes with existing patterns, including nested constructors (`Some(Point{x: 5})`) and the Wave 27 enum-pattern CFG-gating that already protects payload projections.
- Multiple annotations per declaration. Annotations can be stacked on separate lines or placed inline separated by whitespace: `@link "argon2" @extern "C"`. The formatter preserves the author's stacked/inline layout and normalizes spacing.
- `@intrinsic` annotation. Marks a function whose body is hand-emitted by the backend (no source body), mirroring the `@extern "C"` declaration shape. Replaces the `panic("intrinsic")` placeholder convention previously used in stdlib for compiler-implemented methods.
- `Step<S>` enum -- process control flow type with `Continue(S)` and `Done(StopReason)` variants. Replaces ad-hoc `Self | StopReason` unions in process handlers.
- `CallError` enum -- `Timeout` and `ProcessDown` variants. Distinguishes a timed-out call from a dead process.
- `Kernel.panic(message: String)` -- the canonical way to abort with a message and a symbolicated stack trace. Lives on the `Kernel` struct alongside `Kernel.exit`, declared `@intrinsic`. Used internally by `Option.unwrap` and `Result.unwrap`.
- `Debug.print(self)` -- default method on the `Debug` protocol that writes `self.format()` to stdout via `IO.puts`. Auto-derived for every type with a `Debug` impl, so any value gets `.print()` for free (e.g. `42.print()`, `user.print()`, `[1, 2].print()`). Borrows the receiver and returns `()` -- the simple-debug-output companion to the existing `Debug.inspect(move self) -> Self` (which prints and returns `self` for chainable tap-style debugging).
- `Ref.signal(event: Lifecycle)` -- sends a lifecycle signal to a process for cooperative shutdown.
- `Ref.kill()` -- immediately terminates a process without sending a signal.
- `Ref.alive?()` -- returns `true` if the process is still running.
- `Binary.ptr()` intrinsic. Returns a `CPtr<UInt8>` to the underlying byte data, enabling zero-copy pointer passing to C FFI functions.
- `CPtr<UInt8>.to_binary(len)` intrinsic. Creates a `Binary` by copying `len` bytes from a raw pointer. Allocates the 8-byte bit-length header and payload, matching the internal Binary/String layout.
- `String.escape_debug(self) -> String` -- escapes backslashes, double quotes, and the `\n`/`\r`/`\t` whitespace escapes so the result is round-trippable as a quoted string literal. Used by `Debug.format` for `String` and available standalone.
- `@link "lib:symbol"` convention for C symbol naming. When the C symbol differs from the Koja function name, append `:symbol` to the `@link` string (e.g. `@link "crypto:SHA256"` for an Koja function named `sha256_raw`). Keeps all Koja function names in `snake_case` regardless of the C library's conventions. The part before the colon is the library name for `-l` flags; the part after is the LLVM function declaration name.
- `--emit-ast` on `koja parse` and `koja check`. Dumps the raw AST (`parse`) or the sealed, type-annotated AST (`check`) to stdout for tooling and debugging. Pairs with the existing `--emit-llvm` on `koja build`.
- BoringSSL integration in `koja-driver`. Builds BoringSSL via `boring-sys` directly in the driver's build script. `libcrypto.a` is located, embedded via `include_bytes!`, and written to the link temp dir at compile time.
- `koja-stdlib` build script auto-discovers `.koja` files under `expo/lib/`. Adding a new stdlib package is just creating a directory with an `koja.toml` and `src/` -- no Rust code changes needed.

### Changed

- **Breaking**: `Process<C, M, R>` protocol redesigned. `new(config) -> Self` replaced by `start(move config) -> Result<Self, StopReason>` -- initialization now runs in the child process context after spawn. `handle` and `handle_signal` return `Step<Self>` instead of `Self | StopReason`. `spawn T.new(config)` becomes `spawn T.start(config)`.
- **Breaking**: Package names are now PascalCase with uppercased acronyms, matching the casing of the types they hold. The auto-imported `std` package is renamed to `Global`; qualified packages move from lowercase to PascalCase (`net` → `Net`, `http` → `HTTP`, `json` → `JSON`, `crypto` → `Crypto`). Update `koja.toml` `name = "..."`, all `alias <pkg>.<Type>` declarations (`alias Net.TCPSocket`, `alias HTTP.Request`, `alias JSON.Decoder`), and any qualified references in expressions and types (`Net.TCPSocket.connect(...)`, `fn build -> HTTP.Headers`). Directory names on disk stay snake_case (`lib/net/`, `lib/http/`); only `lib/std/` is renamed to `lib/global/` to match its new package name.
- **Breaking**: Networking types (`TCPSocket`, `TCPListener`, `UDPSocket`, `Socket`, `IPAddress`, `SocketAddress`, `SocketKind`) moved from the auto-imported `Global` package to the qualified `Net` package. Use `alias Net.TCPSocket` or `Net.TCPSocket.connect(...)` to access them.
- **Breaking**: `JSONValue` renamed to `Value` in the `JSON` package. `JSON.JSONValue` becomes `JSON.Value`. All constructor methods updated (`Value.string(...)`, `Value.object(...)`, etc.).
- **Breaking**: Struct field patterns are always `name: pattern` -- the shorthand `Point{x, y}` (binding under the field's own name) is gone. Write `Point{x: x, y: y}` to bind under the same name, or `Point{x: _}` / omit the field for "don't care". This applies to both plain struct patterns and enum-struct variant patterns (`Shape.Rect{width: width, height: height}`); the explicit form mirrors construction syntax (`Point{x: 5, y: 2}` was already named-only).
- **Breaking**: `Ref.call` now returns `Result<R, CallError>` instead of `Option<R>`. `Task.await` follows suit.
- **Breaking**: `panic(message)` builtin removed -- use `Kernel.panic(message)` instead.
- **Breaking**: `print(value)` builtin removed -- use `value.print()` instead. Same single-call ergonomics, but routed through the `Debug` protocol so it's an ordinary method dispatch with no compiler builtins involved.
- **Breaking**: `Debug.format` for `String` now returns the round-trippable quoted-and-escaped form: `"hello".format()` returns `"\"hello\""` (six characters), and aggregates render their `String` fields quoted (`User{name: "alice"}` instead of `User{name: alice}`). `IO.puts` is unchanged: it still writes its argument verbatim, so `IO.puts("hello")` prints `hello` without quotes.
- **Breaking**: `handle_lifecycle` renamed to `handle_signal` on the `Process` protocol.
- LSP dot-completion rewritten to use the typed AST instead of source-text scanning. Now works for `self.`, chained method calls (`foo.bar().`), and expressions with inferred types.
- LSP signature help rewritten to use AST-based call-site detection instead of backward parenthesis scanning.
- LSP hover for variables now shows inferred types (e.g. `x: Int32` instead of just `x`).
- LSP go-to-definition on methods uses the typed AST instead of name-suffix matching, so dispatch on the actual receiver type now resolves correctly.
- Parser emits partial `FieldAccess` nodes for incomplete `foo.` expressions, improving editor completion at the dot position.
- The parser now accepts PascalCase segments anywhere in `alias` paths, not just for the trailing type name. `alias Net.Socket` parses as expected.
- `koja-stdlib`'s build script reads each package's canonical `name` from its `koja.toml` instead of inferring it from the directory name. Package directory names are now incidental; only `name = "..."` matters.

### Fixed

- Multi-process setups no longer trigger LLVM union type mismatches when two process types coexist. `Step<T>` is a normal generic enum instantiation, avoiding the ad-hoc union codegen path.
- Process initialization (socket binding, timer setup) now runs in the child process context, fixing `EINVAL` errors from resources created in the parent before spawn.
- `match` arms with nested constructor patterns containing literal payloads (e.g. `Some(TokenKind.Ident("and"))`) no longer segfault when the subject's outer tag doesn't match.
- `move self` methods returning `()` on non-Copy structs no longer produce a false "use of moved value" error. The ownership checker's discarded-return-value warning logic was re-inferring the receiver after it had already been marked as moved, triggering a spurious error.

## [0.9.0] - 2026-04-06

### Added

- Inline functions in struct/enum bodies -- `fn` and `priv fn` can now appear directly inside `struct` and `enum` bodies alongside fields and variants. `impl` blocks remain for extensions and protocol conformance.
- Process lifecycle -- new `Lifecycle` enum (`Shutdown`, `Interrupt`, `Reload`), `StopReason`, `ExitStatus` protocol, and `handle_lifecycle` method on the `Process` protocol. OS signals (`SIGTERM`, `SIGINT`, `SIGHUP`) are delivered as typed messages to the entry process.
- Process entry point -- `entry = "App"` in `koja.toml` designates a `Process<C, M, R>` implementation as the program entry point. Exit codes map through `ExitStatus` (`Normal -> 0`, `Shutdown -> 1`). When `C` is `List<String>`, command-line arguments are passed automatically.
- Multi-threaded scheduler -- the runtime now runs N worker threads (cgroup-aware on Linux). Existing programs gain multi-core scheduling with no code changes. Graceful shutdown and deadlock detection included.
- I/O reactor -- non-blocking socket I/O via kqueue (macOS) and epoll (Linux). Processes suspend on `EAGAIN` and resume when the fd is ready.
- Trait bounds -- `<T: Protocol>` constrains type parameters. Multiple bounds use `&` (`<T: Debug & Hash>`). Bounds are verified at call sites with clear error messages.
- `Random` type -- `Random.bytes(count)` for cryptographically secure random bytes, `Random.int(min, max)` for a random integer in an inclusive range. Auto-imported from `std.kernel`.
- Closure `move` params -- closures now support `move` on parameters (`fn (move x: T) -> U ... end`), matching regular function syntax.
- `alias` keyword -- file-private shorthands for package types. `alias json.Decoder` or `alias json.Decoder as JSONDecoder`.
- Local path dependencies -- `[dependencies]` in `koja.toml` with `path` support for local packages.
- Test runner -- `koja test` discovers and runs `@test`-annotated functions. Optional description: `@test "adds two numbers"`.
- `koja new <name>` -- scaffolds a new project with `koja.toml` and `src/main.koja`.
- Debug and release builds -- DWARF debug info in all builds. `koja build --release` enables LLVM `-O3` optimizations. `.dSYM` bundles on macOS.
- Stacktraces on panic -- formatted stacktraces with source locations, demangled names, and contextual hints (e.g., "use `.or(default)` or pattern match to handle None safely").
- `std.process` module -- process types (`Ref`, `ReplyTo`, `Task`, `Process` protocol) moved from `std.kernel` to a dedicated module.
- `std.socket` module -- `Socket`, `IPAddress`, `SocketAddress`, `SocketKind`. TCP and UDP socket creation, bind, connect, listen, accept, send/receive, and DNS resolution.
- `Debug` protocol -- `format(self) -> String` and `inspect(move self) -> Self`. Compiler-derived for all types. `print()` and string interpolation now dispatch through `Debug.format()`.
- `std.io` module -- `IO.puts`, `IO.warn`, `IO.write`, `IO.gets`. `STDIN`, `STDOUT`, `STDERR` as `Fd` constants.
- File operations -- `FileMode` enum, `File.write(path, content)`, `File.exists?(path)`, `File.delete(path)`, `File.rename(src, dst)`.
- `System` type -- `System.get_env`, `System.set_env`, `System.cwd`, `System.hostname`.
- `DateTime` and `Duration` types -- `DateTime.now()`, `Duration.from_secs()`, `Duration.from_millis()`.
- `Binary.byte_size()` and debug formatting for `Binary` and `Bits`.
- Vim plugin -- auto-indentation, `matchit` support, and `:make` integration with `koja check`.

### Changed

- Project config is now `koja.toml` (TOML-based), replacing `project.koja`.

### Removed

- Windows support -- the runtime now targets Unix only (macOS + Linux).
- `import` keyword -- all types and public functions are visible in every file. The transparent file model replaces imports.
- `@moduledoc` annotation -- use `@doc` on individual types instead.
- Module-grouped doc output -- `koja doc` now produces a flat type namespace.

### Fixed

- `Self` inside union return types (e.g., `Self | StopReason`) now resolves correctly in all positions.
- Generic enum variants with partially inferrable type parameters (e.g., `Result.Ok(value)`) no longer fail with an LLVM type mismatch.
- Static method calls as bare statements no longer emit a spurious "unknown variable" error.

## [0.8.0] - 2026-03-30

### Added

- Project system -- `project.koja` config file defines `name`, `version`, source dirs, and entry module. `koja build`, `koja run`, and `koja check` detect it automatically. The project name is the module namespace (`import my_app.server` resolves `src/server.koja`).
- File I/O -- `File.read(path)` reads an entire file, `File.open(path, mode)` returns a handle for streaming access, `File.close(move self)` releases it. Lower-level `Fd` type for raw descriptor operations. All return `Result<T, String>`.
- String standard library -- `alpha?`, `at`, `codepoints`, `contains?`, `downcase`, `empty?`, `ends_with?`, `graphemes`, `join`, `replace`, `reverse`, `split`, `starts_with?`, `to_float`, `to_int`, `trim`, `trim_end`, `trim_start`, `upcase`, `whitespace?`.
- Binary and bitstring literals -- `Binary` and `Bits` types with `<<>>` syntax for construction and pattern matching. Segment modifiers for bit-width, endianness, signedness, and type annotations. String segments in binary literals (`<<"HTTP/1.1 ", rest: Binary>>`). `<>` concatenation for `Binary`, `Bits`, and `String`. `Bitwise` protocol (`band`, `bor`, `bxor`, `bnot`, `bsl`, `bsr`) on all integer types.
- String/Binary/Bits conversions -- `to_binary()`, `to_bits()`, `to_string()` with validation where needed.
- OR patterns in match arms -- `1 | 2 | 3 -> "small"` combines multiple patterns sharing one body. Works in `match` and `receive`.
- Enum `==` and `!=` -- structural equality for enum values, including generic enums like `Option<String>`. Enables `peek() == Option.Some(".")` as an alternative to `match`.
- Tail call optimization -- self-recursive `move self` methods are rewritten as loops, eliminating stack growth. The `move self` recursive idiom is now safe for unbounded iteration.
- Closure type inference -- `opt.map(v -> v * 10)` infers parameter types from context. Short closures (`x -> expr`) compile to native code with variable capture.
- `List<T>` methods -- `first`, `pop`, `replace_at`, `slice`, `concat`, `reverse`, `find`, `reduce`.
- Struct and enum constants -- `const HEADING = Direction.North` and `const ORIGIN = Point{x: 0, y: 0}` work as constant initializers.
- `@doc` on type aliases -- `@doc` annotations on `type Name = ...` declarations, with formatter and LSP support.
- `List.last()` -- returns `Option<T>`.
- Methods on primitive types -- `impl` blocks on built-in types (`String`, `Int`, etc.).
- Warning when the return value of a `move self` method is discarded. Suggests reassignment to capture the result.
- `koja build --emit-llvm` -- dumps LLVM IR to stdout instead of producing an executable.
- Self-hosted lexer -- the Koja lexer rewritten in Koja, compiled by the Rust bootstrap. Produces identical token output to the Rust lexer, validating the language for real-world use.

### Changed

- `List.push` renamed to `List.append` -- better reflects functional semantics (returns a new list).
- `List.get` and `String.get` now return `Option<T>` instead of panicking on out-of-bounds.
- `Eof` token renamed to `EndOfFile` in `koja lex` output.

### Fixed

- Block expressions (`match`, `if/else`, `cond`) now correctly return values and type-check against declared return types.
- Collections (`List`, `Map`, `Set`) of structs larger than 8 bytes no longer corrupt memory.
- Enum payloads containing nested structs are correctly sized with proper alignment.
- `return` from inside `if` blocks no longer causes a use-after-free for heap-owning types (`List`, `String`, etc.).
- Struct construction inside tail-recursive loops no longer grows the stack on each iteration.
- Assigning to smaller-than-64-bit struct fields (e.g. `UInt32`) no longer clobbers adjacent fields.
- Compound assignments on struct fields (`self.pos += 1`) now work correctly.
- Constants referenced by name carry their correct type through compilation.
- Functions where all branches explicitly `return` no longer produce invalid code at the end of the function body.
- Formatter: OR patterns in match arms wrap cleanly across lines instead of overflowing.
- Formatter: `or`/`and` chains in cond conditions pack densely instead of cascading one-per-line.

## [0.7.0] - 2026-03-22

### Added

- Union types -- `Post | Comment | Ad` as anonymous unions, `type Pet = Cat | Dog | Fish` as named aliases. Automatic widening at assignment/call/return sites. `match` with typed binding patterns (`p: Post -> p.title`).
- `Process<C, M, R>` protocol -- structs implement `Process<C, M, R>` to become processes. `spawn T.new(config)` returns a typed `Ref<M, R>` handle. `Ref.cast` for fire-and-forget, `Ref.call` for synchronous request/reply with timeout.
- Default protocol implementations -- protocols can provide method bodies as defaults. Types inherit defaults automatically or override with their own implementation.
- `Task<R>` -- `Task.async(fn () -> R)` / `Task.await` for one-off async work, built on `Process` / `Ref` / `call`.
- Cooperative process runtime -- processes block on `receive` and resume on message arrival. `receive ... after` timeout clause for timed receives. Supported architectures: aarch64, x86_64.
- Recursive types -- structs and enums can reference themselves (linked lists, trees). Automatic cycle detection, heap indirection, and cleanup on drop.
- Typed constants -- `const NAME: Type = expr` with optional type annotations for generic inference.
- LSP: autocomplete (keywords, symbols, imports), signature help, document symbols, hover/go-to-definition on type names in match patterns and constant annotations.
- VS Code: "Koja: Run File" and "Koja: Build File" commands, `koja.path` setting.

### Changed

- **Breaking**: `spawn` requires `T.new(config)` form and returns `Ref<M, R>` (typed handle). Bare function spawn is a compile error. Replaces the old `Process<M>` model.
- **Breaking**: `fn main` runs as a process -- `main` can use `receive`, `call`, and other blocking operations directly alongside spawned processes.
- **Breaking**: `await` keyword removed; task completion uses `Task.await(ref)`.

### Fixed

- `receive` blocks until a message arrives instead of crashing on empty mailbox. Clean exit when `main` finishes (no false deadlock for background processes). `Ref.call` correctly delivers `ReplyTo` and returns the reply value.
- Integer literals in binary operations coerce to match the other operand's width. Method arguments on monomorphized generic types are properly coerced.
- Generic struct literals infer type arguments from field-access types. Generic enum unit variants resolve correctly inside methods with their own type parameters. Generic struct construction from local variables works with function type parameters.
- `Pair<Unit, T>` and similar types with zero-sized fields keep LLVM field indices aligned with layout metadata.
- `koja` CLI reliably links the embedded process runtime across Cargo target directories.
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
- VS Code extension: syntax highlighting for ` ```koja ` fenced code blocks in Markdown files.

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
- `koja doc` command -- generates static HTML documentation from `@doc` and `@moduledoc` annotations. Supports `@doc false` and `@moduledoc false` to exclude items. Renders markdown in doc strings via pulldown-cmark. Uses askama templates for HTML generation.
- Documentation generator supports recursive directory input (`koja doc src/`) with dotted module names derived from file paths (e.g. `src/what/util.koja` becomes `what.util`).
- Global sidebar navigation across all module pages with active module highlighting.
- Brand-colored documentation theme (burnt orange `#dd6900` + warm charcoal) with Source Sans 3 / Source Code Pro typography.
- LSP: hover and go-to-definition for qualified calls (`math.add()` shows signature and docs from the source module).
- LSP: nested module path resolution (`import what.util` correctly resolves to `what/util.koja`).
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
- `koja build` -- compile to native binary via LLVM.
- `koja run` -- build and execute in one step.
- `koja check` -- type check without compiling.
- `koja format` -- opinionated code formatter (`--check`, `--write`).
- `koja parse` -- dump AST.
- `koja lex` -- dump tokens.
- Structured error messages with source context, underlines, and hints.
- Colored output with `--no-color` flag and `NO_COLOR` env var support.
- VS Code / Cursor syntax highlighting extension.
- Vim syntax highlighting.
