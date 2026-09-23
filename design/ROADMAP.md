# Koja Language Roadmap

Koja is approaching stability through concrete `0.x` releases. This roadmap
tracks commitments that affect future language and ecosystem work. It does not
duplicate the feature inventory. Koja remains pre-1.0, so breaking cleanup is
still allowed when it produces a clearer long-term language.

For the current language, see [LANGUAGE.md](../LANGUAGE.md). Use `koja help`
for the CLI surface, generated package documentation for library APIs, and
[CHANGELOG.md](../CHANGELOG.md) for what each release shipped. Earlier
release sections of this file are preserved under `archive/`, each snapshot
linking the one before it.

## 0.20.0

The 0.20 release carries the I/O half of the 0.19 plan. 0.19 shipped the
time types, the `test` declaration, and the language server features, and
left the `Read` protocol, the socket deadlines, and the `@test` removal it
had announced. 0.20 finishes those while there are still no external users
to protect, and picks up the language server items that did not make the
cut.

### Breaking cleanup

- Remove `@test` after its 0.19 deprecation. Every remaining annotation
  warns today, so the removal is a parser error with the `test "..."`
  replacement, on the `unless` model.
- Reshape the I/O types along [IO.md](IO.md). `IO.gets` returns
  `Option<String>` over a caller-supplied reader so callers can tell end
  of input from an empty line
  ([gap](GAPS.md#toolchain-and-stdlib-nits-from-the-git_hygiene-build)),
  as one step of the `Read` protocol.

### Language

- Let a `const` hold a `List`, `Map`, or `Set` literal of constant
  expressions. Field defaults already accept those shapes, so a user who
  writes `const TABLE = ["a": 1]` and gets "limited to literals" is meeting
  a gap, not a rule. The blocker is that constants inline at every use site,
  which would allocate a fresh collection per reference. A collection
  constant needs one allocation per constant instead, built once and shared,
  which is the same piece a `List` constant needs. The `koja-lang/tz`
  package is the first case that wanted it. With no static table, its
  identifier lookup is a generated `match` over string literals per region.

### Language server

- Code actions attached to diagnostics, so teaching diagnostics become
  one-keystroke fixes.
- Incremental text synchronization in place of full-document sync.
- A run-test code lens on each `test` declaration.

### Runtime

- Add socket read, write, connect, and accept timeouts so a stalled peer
  cannot block its owning process forever
  ([gap](GAPS.md#sockets-have-no-deadlines)). Sockets are non-blocking
  through the reactor on both backends, so a timeout is a bounded reactor
  wait, the same mechanism `receive ... after` and `Fd.watch` use, not a
  socket option. The shape is settled in
  [IO.md](IO.md#timeouts-are-socket-state) and takes a `Duration` from
  [TIME.md](TIME.md).

The deferred standard library items stay in [GAPS.md](GAPS.md) and can ship
in any patch release: `UUID.v4()`, `Binary.compare` and endian helpers,
`List.sort`, `System.cmd`, and `File.ls`. `Fd` random access, durability,
and locking also stay there, as do the compiler fixes with a known cause,
such as the
[struct literal default](GAPS.md#struct-literal-defaults-stop-at-the-package-boundary)
pair. None of them is a 0.20 release gate. The tree-sitter grammar, the
editor extensions, and kojalang.org pick up the `unless` removal and the
`test` syntax from 0.19. That is ecosystem work, not a release gate.

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
