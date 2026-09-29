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

The 0.20 release carries the I/O half of the 0.19 plan and the runtime half
of the observability design. 0.19 shipped the time types, the `test`
declaration, and the language server features, and left the `Read`
protocol, the socket deadlines, and the `@test` removal it had announced.
0.20 finishes those while there are still no external users to protect,
picks up the language server items that did not make the cut, and lands
the process context and log slot from
[OBSERVABILITY.md](OBSERVABILITY.md) in the same window. Both are runtime
changes to the same crate, and one breaking release means remem,
`auth_manager`, and later `untz-tan` migrate their logging and tracing
call sites once instead of twice.

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

- **[DONE]** Let a `const` hold a `List`, `Map`, or `Set` literal of
  constant expressions. Constants take the field default grammar, a
  collection constant is built once at program start and shared, and a
  constant can read another constant in any source order. The
  `koja-lang/tz` package is the first case that wanted it.
- **[DONE]** Let a field default name its struct through a dotted path
  or an alias. `options: TCPListener.Options = TCPListener.Options{}` was
  rejected and the aliased spelling panicked in resolve. Every default
  now resolves in its declaring file, at the declaration and at each
  site, and enum payload variants are accepted as defaults.
  `TCPListener.options` has its default.

### Language server

- Code actions attached to diagnostics, so teaching diagnostics become
  one-keystroke fixes.
- Incremental text synchronization in place of full-document sync.
- A run-test code lens on each `test` declaration.

### Runtime

- **[DONE]** Add socket read, write, connect, and accept timeouts so a
  stalled peer cannot block its owning process forever. Sockets are non-blocking
  through the reactor on both backends, so a timeout is a bounded reactor
  wait, the same mechanism `receive ... after` and `Fd.watch` use, not a
  socket option. The shape follows
  [IO.md](IO.md#timeouts-are-socket-state) and takes a `Duration` from
  [TIME.md](TIME.md). The timeouts landed on the current `Socket.Error`
  surface before the `IO.Error` migration, so that step renames the
  error type but not the fields.

### Observability

[OBSERVABILITY.md](OBSERVABILITY.md) is the contract. The runtime carries
two typed fields per process, `Process.context` copied on every message
and a log configuration copied on every `spawn`, and the standard library
owns `Trace` and `Log` on top of them. Exporters are packages. The work
is ordered by dependency, and the socket deadlines above come first
because an exporter that posts to a stalled collector must time out
rather than block forever.

1. `Process.context`: the 32-byte slot on the process, the copy on
   `spawn`, the copy onto every `send`, `cast`, `call`, and `send_after`
   envelope, and the install and restore around each handler run. Small
   and mechanical, and everything after it depends on it.
2. `Trace` in the standard library on top of the slot, with remem's
   `lib/open_telemetry` ported to the new API as the first exporter
   package. The export queue starts as one mutex queue with a drop
   counter. Per-scheduler buffers wait for a benchmark.
3. The log slot: `Log.configure` writes the calling process's slot and
   `spawn` copies it, handlers are closures, `Log.Record` is stamped from
   `Process.context`, and the runtime crash report flows through the same
   path. remem and `auth_manager` delete their vendored `lib/log` and
   threaded `Logger` values.

The rest of the I/O work is independent of these three and interleaves
wherever one of them stalls.

Cut list, in the order to drop items if the release runs long: `@SOURCE`,
the compile-time `[log] min_level` floor, span events on log records,
per-scheduler export buffers, then the language server items. None of them
changes the runtime shape, so each can ship in a patch or in 0.21 without
a second migration. If step 2 or 3 stalls, the escape hatch is to tag I/O
plus `Process.context` as 0.20 and finish `Log` and the exporter in 0.21.

The deferred standard library items stay in [GAPS.md](GAPS.md) and can ship
in any patch release: `UUID.v4()`, `Binary.compare` and endian helpers,
`List.sort`, `System.cmd`, and `File.ls`. `Fd` random access, durability,
and locking also stay there, as do the compiler fixes with a known cause,
such as the
[function reference default](GAPS.md#function-references-cannot-be-default-field-values).
None of them is a 0.20 release gate. The tree-sitter grammar, the
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
