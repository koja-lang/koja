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

The 0.16 release closes the remaining known language contracts, ships
anonymous tuples, and adds the error channel.

### Language and tooling

- **[DONE]** Core anonymous tuple support and standard API migrations
- **[DONE]** Lexical nested type declarations with formatter, LSP, and editor
  grammar support
- **[DONE]** A CLI task protocol modeled on `Mix.Tasks` (`Koja.Task` + `[tasks]` manifest exports)
- **[DONE]** An `@deprecated` compile annotation
- **[DONE]** Deprecation of `Pair<A, B>` with guidance to use anonymous tuples
- **[DONE]** Error channel notation over `Result`: `-> T ! E` signatures with
  `try`, `fail`, and `rescue` ([ERROR-HANDLING.md](ERROR-HANDLING.md))

### Correctness and production gates

- **[DONE]** Type-check and coercion-stamp every explicit `return`. Script behavior must
  remain consistent across both backends.
- **[DONE]** Reject reads of locals that are not definitely assigned on every path.
- **[DONE]** Reject tuple equality when any element lacks valid equality semantics.
- **[DONE]** Make the formatter round-trip multiline string literals byte-exactly.
  Heredoc-style content must survive formatting without dedent or lexing
  corruption.

The release is complete when these items are implemented, documented, and
covered by tests on both backends where applicable. The VSCode, Vim, and
tree-sitter integrations pick up anonymous tuple support after the release
ships. That is ecosystem work, not a release gate.

## 0.17.0

The 0.17 release removes deprecated surface, modernizes type declarations
with conformance headers, struct field defaults, and the `builtin`
declaration kind, and hardens Linux deployments with position-independent
binaries.

### Language and tooling

- **[DONE]** Make public constants and functions referenceable across package
  boundaries (carried over from 0.16).
- **[DONE]** Attribute LSP diagnostics to their originating file in multi-file
  projects (carried over from 0.16).
- **[DONE]** Default values for struct and enum struct-variant fields,
  re-evaluated at each construction site.
- **[DONE]** Remove `Pair<A, B>` after its 0.16 deprecation period. The
  changelog covers migration to anonymous tuple construction, destructuring,
  and patterns.
- **[DONE]** Inline protocol conformance on type declarations:
  `struct MyApp: Process<C, M, R>` takes a comma-separated protocol list and
  desugars to the equivalent `impl` blocks. Enums take the same syntax. The
  main motivation is making application entry types read less awkwardly.
- **[DONE]** A `builtin` declaration kind for compiler-owned types. It
  replaces the `@intrinsic struct` workaround, the construction special
  cases, and the name-keyed IR shape match.
- **[DONE]** Emit position-independent Linux binaries so deployments get
  ASLR.

## 0.18.0

The 0.18 release adds optional function arguments and runtime observability.

### Language and tooling

- Default values for function parameters, so optional arguments do not
  require a separate options struct at every call site. A defaulted function
  is callable at every arity its defaults allow. `&name/arity` references
  pin one arity when a bare name is ambiguous. Separately declared functions
  sharing a name across arities stay out of scope
  ([MISC.md](MISC.md)). Struct field defaults (0.17) reduced the urgency by
  making options structs cheap to declare and construct.
- Add a `Runtime` observability API with global and per-process metrics,
  including mailbox depth, and document the overload contract for long-running
  services. Shape the API from the telemetry and structured logging package
  work in the ecosystem-validation section, not speculatively.

The rest of the 0.18 scope will be selected from evidence gathered while
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
