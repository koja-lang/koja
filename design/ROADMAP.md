# Koja Language Roadmap

Koja is approaching stability through concrete `0.x` releases. This roadmap
tracks commitments that affect future language and ecosystem work. It does not
duplicate the complete feature inventory.

For the current language, see [LANGUAGE.md](../LANGUAGE.md). Use `koja help`
for the CLI surface, generated package documentation for library APIs, and
[CHANGELOG.md](../CHANGELOG.md) for changes between releases. The former
phase-based roadmap is preserved in
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
- Git dependencies are reproducible through `koja.lock` and the `koja deps`
  command family.
- `koja shell` loads projects and provides completion. Its remaining
  improvements are optional and driven by use.
- Koja remains pre-1.0. Breaking cleanup is still allowed when it produces a
  clearer long-term language.

## 0.16.0

The 0.16 release closes the remaining known language contracts while adding the
last major product types planned before ecosystem validation.

### Language and tooling

- **[DONE]** Core anonymous tuple support and standard API migrations
- Anonymous tuple support in the VSCode, Vim, and tree-sitter integrations
- A CLI task protocol modeled on `Mix.Tasks`
- Anonymous records
- An `@deprecated` compile annotation
- Deprecation of `Pair<A, B>` with guidance to use anonymous tuples

### Correctness and production gates

- Type-check and coercion-stamp every explicit `return`. Script behavior must
  remain consistent across both backends.
- Reject reads of locals that are not definitely assigned on every path.
- Reject tuple equality when any element lacks valid equality semantics.
- Make public constants referenceable across package boundaries, or explicitly
  narrow the language contract before release.
- Attribute LSP diagnostics to their originating file in multi-file projects.
- Add a `Runtime` observability API with global and per-process metrics,
  including mailbox depth, and document the overload contract for long-running
  services.

The release is complete when these items are implemented, documented, and
covered by tests on both backends where applicable.

## 0.17.0

Remove `Pair<A, B>` after its 0.16 deprecation period. Migration guidance will
cover tuple construction, destructuring, patterns, and public API changes.

The rest of the 0.17 scope will be selected from evidence gathered while
building real packages and applications. Later `0.x` releases will be added
only when their scope is concrete.

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

The current shell is sufficient for this work. Inline help syntax, an explicit
`-S` selector, and process inspection remain optional improvements rather than
release gates.

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
