# Koja Language Roadmap

Koja is approaching stability through concrete `0.x` releases. This roadmap
tracks commitments that affect future language and ecosystem work. It does not
duplicate the complete feature inventory.

For the current language, see [LANGUAGE.md](../LANGUAGE.md). Use `koja help`
for the CLI surface, generated package documentation for library APIs, and
[CHANGELOG.md](../CHANGELOG.md) for changes between releases. The 0.16 to
0.18 release history is preserved in
[archive/20260912-ROADMAP.md](archive/20260912-ROADMAP.md), and the older
phase-based roadmap in
[archive/20260722-ROADMAP.md](archive/20260722-ROADMAP.md).

## Current baseline

The following facts constrain future planning.

- The compiler has a four-phase sealed pipeline. Both the LLVM backend and the
  interpreter consume the same sealed `IRProgram`.
- Native execution and cooperative interpretation implement the same process
  and scheduler semantics.
- Process crash containment, monitors, parenting, kill cascades, lifecycle
  delivery, graceful drain, preemption, priorities, and timer scheduling have
  landed.
- Fallible functions declare `-> T ! E` and use `try`, `fail`, and `rescue`
  ([ERROR-HANDLING.md](ERROR-HANDLING.md)).
- Struct fields and function parameters take default values. Iteration is
  cursor-based through nominal `Enumeration` conformance.
- `@deprecated` marks library surface for removal, and generated
  documentation shows the migration guidance.
- The `Runtime` API exposes global and per-process metrics.
- Git dependencies are reproducible through `koja.lock` and the `koja deps`
  command family.
- Project-aware commands accept `-S <path>` to select a project without
  changing the working directory.
- `koja shell` loads projects and provides completion. Its remaining
  improvements are optional and driven by use.
- Linux binaries are position-independent. Tagged releases build for every
  supported host and publish checksummed tarballs through
  [koja-lang/releases](https://github.com/koja-lang/releases).
- Koja remains pre-1.0. Breaking cleanup is still allowed when it produces a
  clearer long-term language.

## 0.19.0

The 0.19 release is a developer experience release. It finishes the breaking
cleanup announced in 0.18, replaces the test boilerplate with a `test`
declaration and compiler-known assertions, brings the language server up to
the features users miss first, and bounds socket waits. It also reshapes the
standard library's time and I/O types ([TIME.md](TIME.md), [IO.md](IO.md))
while there are no external users to protect. One release that breaks these
together costs one migration pass over the known Koja codebases, where
spreading the same changes across releases would cost one pass each.

### Breaking cleanup

- **[DONE]** Remove the `unless` keyword. The parser reports a targeted error
  with the `if not cond` replacement, and `unless` stays reserved.
- **[DONE]** Remove `JSON.StringBuilder` and `IPAddress.v4?()` / `v6?()`
  after their 0.18 deprecations.
- Change `IO.gets` to return `Option<String>` over a caller-supplied reader
  so callers can tell end of input from an empty line
  ([gap](GAPS.md#toolchain-and-stdlib-nits-from-the-git_hygiene-build)).
  Superseded by [IO.md](IO.md), where it is one step of the `Read`
  protocol.
- **[DONE]** Add a monotonic `Instant`, count `Duration` in nanoseconds,
  and rename `DateTime` to `Timestamp` counting microseconds
  ([TIME.md](TIME.md)). The test runner measured elapsed time with the
  wall clock, and a millisecond `DateTime` could not round-trip a
  Postgres timestamp. This landed before the IO work, whose socket
  timeouts take a `Duration`, and frees `DateTime` for the zoned type.

### Language

- **[DONE]** Let `alias` name a package-level function or constant, not only a type.
  `alias JSON.decode` binds `decode` in the file, every arity included, and
  `alias Pkg.DEFAULT_PORT` binds the constant. `as` renames either one. The shadow rule stays a hard error, so there is no precedence
  between an alias and a same-package name. The test surface is what made
  the types-only carve-out visible, and the fix is to remove the carve-out
  for every registry kind, not to add an import form.
- Let a `struct`, `enum`, or `builtin` body declare a `const`, read as
  `Duration.ZERO`. Package-level `const` already exists with a literal-shape
  rule, so this adds a namespace, not a new kind of value. The stdlib keeps
  writing fixed values as functions that return a literal, `IPAddress.any`,
  `IPAddress.loopback`, and `Offset.utc` and `TimeZone.utc` in
  [DATETIME.md](DATETIME.md), and a function is not eligible as a field
  default where a constant would be. Field defaults gain a constant reference
  in the same change, since constants inline to the literal shape defaults
  already accept. `Duration.ZERO`, `Timestamp.UNIX_EPOCH`, `Int.MAX`, and the
  `IPAddress` pair are the first uses. Lands before [DATETIME.md](DATETIME.md)
  so its zones are constants from the start.

### Testing

The design is accepted in [TESTING.md](TESTING.md).

- **[DONE]** Add a `test "description"` declaration at the top level and inside
  structs. Structs are suites and nested structs are the hierarchy. The
  body's channel is `Test.Failure` and nothing else. Setup errors enter
  through `try Test.require(...)`, and `fail "message"` keeps working because
  `Test.Failure` conforms to `StringLiteral`.
- **[DONE]** Add an `assert` statement. Koja has no macros, so only the compiler can
  stamp the source expression, the file, the line, and the `Debug` rendering
  of both operands into a `Test.Failure`. Assertions are hard: the first
  failure ends the test.
- **[DONE]** Add a `Test` package with the `Test.Failure` enum and three functions:
  `require` for setup, `skip`, and `crashes` for a body that must panic. The loader links it only when tests are included, which keeps
  `assert` out of application code without a special rule.
- **[DONE]** Make the `Test` package the runner. `Test.Runner` runs each test in its
  own process with a deadline, so a crash or a hang is one failure and not
  the end of the run, and feeds a `Test.Reporter`. The `dots`, `trace`, and
  `json` reporters ship, and any package can add one.
- **[DONE]** The human reporters follow the pretty and short styles of compiler
  diagnostics.
- **[DONE]** Run `koja test` on the interpreter by default, with `--backend llvm` for
  the native run. Done in phase 3 with the same backend selection as
  `koja run`. Since phase 4 the interpreter resolves a project's own
  externs through the dynamic loader, so only a project whose `@link`
  library exists solely as a static archive still compiles. The CI recipes
  run both, which turns every test suite into a parity check between the
  backends.
- **[DONE]** Deprecate `@test` in 0.19 for removal in 0.20. Done. The stdlib and the
  `koja new` scaffold migrated by hand, and every remaining annotation
  warns. The examples migrate after the release. No formatter rewrite.

### Language server

- **[DONE]** Find references and rename, built on one reference index over the
  resolved AST in the `koja-query` crate. Document highlight falls out of the
  same index.
- Code actions attached to diagnostics, so teaching diagnostics become
  one-keystroke fixes.
- **[DONE]** Inlay hints for inferred binding types and parameter names at call sites.
- Incremental text synchronization in place of full-document sync.
- A run-test code lens on each `test` declaration.
- **[DONE]** A `FEATURES.md` in `koja-lsp` that lists the supported and the
  declined protocol methods.

### Runtime

- Add socket read, write, connect, and accept timeouts so a stalled peer
  cannot block its owning process forever
  ([gap](GAPS.md#sockets-have-no-deadlines)). Sockets are non-blocking
  through the reactor on both backends, so a timeout is a bounded reactor
  wait, the same mechanism `receive ... after` and `Fd.watch` use, not a
  socket option. The shape is settled in
  [IO.md](IO.md#timeouts-are-socket-state) and depends on `Duration` from
  [TIME.md](TIME.md).
- **[DONE]** Give the interpreter general C FFI. Done. An `@extern "C"` with no
  hand-written shim resolves through the dynamic loader, in the `@link`
  library as a shared library under the project root, on the loader's
  search path, or in the running process, and runs through libffi with
  the declared signature. The reactor-aware shims for files, sockets, and
  TLS stay as overrides. A project whose `@link` library exists only as a
  static archive still compiles through LLVM, since the loader cannot open
  a `.a`.

The deferred standard library items stay in [GAPS.md](GAPS.md) and can ship
in any patch release: RFC 3339 formatting and parsing, `UUID.v4()`,
`Binary.compare` and endian helpers, `List.sort`, `System.cmd`, and
`File.ls`. `Fd` random access, durability, and locking also stay there. None
of them is a 0.19 release gate. The tree-sitter grammar, the editor
extensions, and kojalang.org pick up the `unless` removal and the `test`
syntax after the release ships. That is ecosystem work, not a release gate.

Later `0.x` releases will be added only when their scope is concrete.

## Ecosystem validation

Koja already has the primitives needed to explore supervision in real systems.
The language should not prescribe a universal supervision protocol before its
ecosystem demonstrates the recurring shapes.

Build representative process-based packages and applications first.

- An HTTP server
- A WebSocket or Discord client
- Telemetry and structured logging
- Connection and worker pools
- Registry-style discovery

These projects should validate restart ownership, child specifications,
shutdown order, transient and permanent failure, registration, observability,
and backpressure. A supervision protocol may then be derived from repeated
patterns. The existing monitor, parenting, crash, and lifecycle primitives are
the stable foundation.

The current shell is sufficient for this work. Inline help syntax and process
inspection remain optional improvements rather than release gates.

## Path to 1.0

Koja 1.0 is a stability release, not a deadline for every plausible feature.
It requires evidence that the language can support the applications it was
designed to build.

- Ship and operate representative libraries and applications.
- Review the complete language surface and resolve any remaining breaking
  questions before the specification freezes.
- Publish coherent language, package, concurrency, FFI, and tooling
  documentation.
- Complete a focused diagnostic quality pass.
- Define and continuously test the supported host and target tiers.
- Publish signed release artifacts for every supported tier-1 host.
- Lock the language specification after validation.

WebAssembly, self-hosting, and a universal supervision protocol are not
prerequisites for 1.0.

## Portability and WebAssembly

Native cross-compilation and WebAssembly are separate projects. Neither is
assigned to a release until its scope and user need are concrete.

Koja preserves the following portability invariants now.

- The sealed IR remains target-independent.
- The runtime core remains platform-neutral.
- Process and supervision semantics remain safe under cooperative scheduling.
- Unsupported target capabilities produce explicit diagnostics.
- Language contracts do not depend on POSIX threads or signals.

WASI 0.3 provides an async component model, but a Koja backend still depends on
separate progress in stack switching, engine support, LLVM emission, TLS and
crypto, FFI, lifecycle behavior, and browser integration.

Begin a WebAssembly spike when stack switching is practical through Wasmtime
and LLVM. Before a full backend is scheduled, the spike must prove nested-call
suspension, `receive`, timers, I/O wakeup, preemption, and continuation
resumption. This keeps Koja ready for WebAssembly without promising a runtime
whose core requirements are still moving upstream.

## Optional future research

The Rust compiler is an acceptable permanent implementation. `kojac` remains
optional research with no release assignment, parity commitment, or plan to
retire the Rust pipeline. Revisit self-hosting only when it offers a concrete
maintenance or product benefit.

Additional native targets, cross-compilation, WebAssembly, browser integration,
and alternative backends remain possible future work. Their designs must
preserve the compiler and runtime invariants above.

Embeddable script evaluation is another candidate: a host API around the
interpreter's script entry point that returns the script's result to the
embedding program. Explicit `return <value>` at the top level is rejected
today and stays reserved as the hosted-script result channel.

## Guiding principles

- Readability over cleverness. A reader should understand a line without
  hidden context.
- Error messages are a feature. Confusing diagnostics are bugs.
- Real applications validate the language better than speculative examples.
- Explicit behavior beats invisible control flow.
- Common patterns belong in coherent language features or libraries, not
  macros that fragment the language.
- The default path should remain approachable while advanced behavior stays
  available when needed.
- Every lasting design should still make sense in twenty years.
- After 1.0, language changes are additive. A truly breaking change belongs in
  a deliberate major release with migration tooling.
