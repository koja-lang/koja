# Project Model

This document describes the project, dependency, build, test, and script model
implemented by the Koja driver. User-facing package semantics live in
[LANGUAGE.md](../LANGUAGE.md). Future incremental compilation must preserve
the boundaries in [COMPILER-NORTHSTAR.md](COMPILER-NORTHSTAR.md).

## Project root

A project is rooted at the nearest `koja.toml`. Commands that omit an explicit
path search from the working directory. The manifest defines the package name,
entry process, minimum compiler version, and dependencies.

The conventional layout is:

```text
my_package/
  koja.toml
  koja.lock
  src/
  test/
  deps/
  build/
```

`src/` contains package declarations. `test/` is included only by test and
test-aware tooling. `deps/` contains materialized git dependencies. `build/`
contains native artifacts. The lockfile is committed, while `deps/` and
`build/` are reproducible and ignored by version control.

## Package namespace

All `.koja` files in a package share one namespace. Files do not introduce
modules, and declarations in one source file are visible from every other file
in the package.

Dependencies and qualified standard-library packages keep their package names.
External types use qualified access or a file-local `alias`. Package-private
declarations remain visible across files in their own package.

The stdlib is embedded in the compiler and bundled separately from project
sources. It is not copied into the project's `deps/` directory.

## Dependencies

Dependencies are declared in `koja.toml`.

- Path dependencies resolve directly from their declared directory.
- Git dependencies may use `git` URLs or `github` owner and repository names.
- A git dependency may select a tag, branch, revision, or default branch.
- Exactly one source and at most one ref selector are allowed.

`koja.lock` pins each git dependency to an exact commit. Path dependencies do
not receive lock entries.

`koja deps get` and `koja deps update` are the only commands that access the
network or change the lockfile. Build, check, run, test, doc, LSP, and shell
loading are offline. A missing or stale lock entry produces an actionable
error instead of fetching implicitly.

## Materialization and cache

Git repositories are stored in a global mirror cache. The selected commit is
exported into `deps/<Package>/` as a read-only tree with a revision marker.
Commands verify the manifest, lockfile, cache, marker, and package name before
loading sources.

A missing or stale materialization is rebuilt from the local mirror. If the
required commit is not cached, the command asks the user to run
`koja deps get`.

`koja deps clean` removes the project's materialized dependencies without
touching the lockfile. Its cache option also removes the global mirror.

## Build outputs

`koja build` lowers the complete project through the sealed pipeline and emits
a native binary through LLVM.

- Debug output defaults to `build/debug/<binary>`.
- `--release` output defaults to `build/release/<binary>`.
- An explicit output path overrides the default.

The current driver has no persistent typed-AST or IR cache. Proposed `.est` and
`.eir` artifacts belong to the incremental architecture described in
[COMPILER-NORTHSTAR.md](COMPILER-NORTHSTAR.md), not the current project layout.

Interpreter execution does not produce a native artifact. `koja run` selects
the interpreter by default, while `koja build` always uses LLVM.

## Tests

`koja test` loads `src/`, `test/`, dependencies, and the embedded stdlib. Tests
are `@test` functions declared inside types. The test runner discovers them,
adds a synthetic `Process` entry, compiles a debug LLVM binary, runs it, and
removes the temporary binary afterward.

Tests execute sequentially in a single process. Trace mode favors
interactive diagnostics and disables the whole-binary timeout. Test is an
execution mode, not a separate build profile. The command does not accept
`--release`.

## Scripts

`.kojs` files are scripts with top-level statements and expressions. They do
not require `koja.toml` and do not define a project package.

The interpreter is the default script backend. LLVM script execution compiles
a synthetic entry and uses a temporary binary. Script behavior must remain
consistent across backends except where a capability is explicitly
backend-specific, such as user C FFI in the interpreter.

## Tooling

The shell and LSP mirror the project loading rules of build-oriented commands.
They load package sources, dependencies, and the embedded stdlib under the same
namespace rules.

Generated documentation bundles the project, dependencies, and stdlib into one
browsable output while preserving package qualification.

## Invariants

- `koja.toml` is the declared dependency source.
- `koja.lock` is the reproducible git resolution.
- Network access is explicit through `koja deps`.
- Materialized dependencies are disposable and read-only.
- Build outputs are disposable.
- Package identity comes from manifests, not directories or source files.
- Every command observes the same package graph.
