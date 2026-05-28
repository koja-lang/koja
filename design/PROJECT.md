# Project Layout: Build Artifacts, Dependencies, and Execution Contexts

Design notes for how an Koja project organizes itself on disk: where
source, dependencies, and tool-generated artifacts live; how builds
profile differently from how they scope; how `koja test` slots into the
profile/scope model; and how script execution (`.kojs`) handles the
absence of a project context. The goal is a layout where each directory
has a single clear lifecycle, every part is independently regenerable
from a higher-up piece, and the LSP / dev experience is uniform whether
you're touching stdlib, a third-party dep, or your own source.

---

## Top-level layout

A fully-wired Koja package looks like:

```
my_package/
  koja.toml         -- declared deps, package metadata
  koja.lock         -- pinned exact versions (committed)
  src/              -- source code
  test/             -- integration tests (optional sibling dir)
  deps/             -- materialized deps + stdlib (gitignored)
  build/            -- compiled artifacts (gitignored)
    debug/
      cache/        -- .est, .eir for default-scope code
      test/         -- .est, .eir and run output for test-scope code
    release/
      cache/
      test/
      bin/          -- native binaries
```

Each piece has a clear lifecycle:

| Piece       | Lifecycle                                              | Gitignored |
| ----------- | ------------------------------------------------------ | ---------- |
| `koja.toml` | hand-edited                                            | no         |
| `koja.lock` | tool-managed, committed for reproducibility            | no         |
| `src/`      | hand-edited                                            | no         |
| `test/`     | hand-edited                                            | no         |
| `deps/`     | materialized from `koja.lock` + registry / install dir | yes        |
| `build/`    | rebuilt from `deps/` + source                          | yes        |

The layering matters: `build/` regenerates from `deps/`, `deps/`
regenerates from `koja.lock`, `koja.lock` regenerates from `koja.toml` +
the registry. Any tier can be `rm -rf`'d without losing work; the next
tier up rebuilds it.

---

## The build directory

### Why `build/` and not `_build/` or `target/`

Three live conventions in the broader ecosystem:

| Style                  | Examples                            | Notes                                                  |
| ---------------------- | ----------------------------------- | ------------------------------------------------------ |
| `_build/` (underscore) | Mix, Rebar, Dune                    | Going out of fashion; signals "Elixir"                 |
| `target/` (Cargo)      | Cargo, Maven                        | Generic, but taken by Cargo at workspace root          |
| `build/` (plain)       | Gradle, CMake, npm scripts, Flutter | Most language-neutral, most discoverable               |
| `.build/` (hidden)     | Swift PM, Stack, Crystal            | Hides build artifacts from `ls`; can confuse new users |

`build/` wins on three counts: it's language-neutral (Koja isn't Elixir
or Rust by inheritance), it doesn't collide with the Cargo `target/`
that the rust-side compiler workspace uses, and it's the most boring
choice — which is the right vibe for tool-generated output.

### Profile-based subdirs

Inside `build/`, the first tier is the **build profile**: `debug` or
`release`. This is the only axis the build dir tier divides on.

| Profile   | Default backend              | Use                         |
| --------- | ---------------------------- | --------------------------- |
| `debug`   | `koja-ir-eval` (interpreter) | Inner dev loop, `koja test` |
| `release` | `koja-codegen` (LLVM)        | Shipping, `koja build`      |

The dual-backend story is a real Koja advantage: the interpreter gives
sub-100ms feedback for tests and dev runs; LLVM gives optimized native
binaries for shipping. Profiles pick the backend; everything else
follows.

Inside each profile dir, the layout is:

```
build/<profile>/
  cache/    -- .est + .eir for default-scope code
  test/     -- .est + .eir for test-scope code, plus test run output
  bin/      -- native binaries (release only; debug interprets in place)
```

`cache/` and `test/` are split so that test-scoped code doesn't pollute
the default-scope artifacts. A release binary built from `cache/` never
contains test code; a `cache/` artifact's fingerprint is independent of
what test code happens to exist.

`bin/` only populates under release. Debug runs through the interpreter,
which has nothing to write to disk beyond cached IR.

---

## Profiles vs scopes

A common source of confusion in build systems is conflating three
unrelated concepts under the word "environment":

1. **Build profile**: how the code is compiled (debug vs release).
2. **Compile-time scope**: what's in the input set (default vs test).
3. **Runtime environment**: dev/test/staging/prod as program-visible state.

Mix collapses all three into `MIX_ENV`, which is why `_build/dev/`,
`_build/test/`, `_build/prod/` exist. This causes deployment pain
(same code, different artifacts, "works on my machine" bugs) and Elixir
is actively moving away from it via runtime config.

Cargo splits them: profile is a build-dir tier, scope is handled via
`[dev-dependencies]` and `cfg`, runtime env is not the build tool's
problem. Koja follows Cargo's split.

### Profile × scope, not env tiers

The two axes that actually exist:

|                     | default scope                           | test scope                                           |
| ------------------- | --------------------------------------- | ---------------------------------------------------- |
| **debug profile**   | `koja run` / `koja check` (interpreter) | `koja test` (interpreter, fast)                      |
| **release profile** | `koja build` (LLVM, ship)               | `koja test --release` (LLVM, validates codegen path) |

This matrix dictates the build dir layout. No `build/dev/`, no
`build/prod/`. Runtime environment is whatever the program decides at
runtime (via env vars, config files, whatever the program reads). The
compiler doesn't branch on it; the build tool doesn't either.

### Why a test scope but not a test profile

`koja test` could conceivably be its own profile (Mix-style), but
scope-not-profile gives a strict superpower: **you can run the same
tests through either backend.**

```
koja test              # debug profile + test scope = fast iterate
koja test --release    # release profile + test scope = codegen smoke
```

A bug in `koja-codegen` won't surface under `koja-ir-eval`. The reverse
is also true. Running tests through only one backend silently leaves
the other untested. Making test a scope keeps both available without
duplication.

---

## Dependencies

### `deps/` sits next to `build/`, not inside

The deciding principle: **`build/` should be safely `rm -rf`'able
without losing work or requiring network access.**

If deps live under `build/`, `rm -rf build/` becomes "rebuild _and_
re-download everything." That's a friction point Cargo deliberately
avoids and Mix users routinely complain about. Keep them separate so
nuking `build/` is always a no-cost local operation.

### `koja.toml` + `koja.lock`

Standard modern shape:

- `koja.toml` declares deps with version constraints (e.g. `^1.2`).
- `koja.lock` pins exact resolved versions for reproducibility.
- `deps/` is the materialized view of `koja.lock` + the registry.
- Commit `koja.toml` and `koja.lock`; gitignore `deps/`.

### Path deps vs registry deps

`koja.toml` supports both:

```toml
[dependencies]
koja_ast = { path = "../koja_ast" }   -- in-tree workspace member
http_client = "1.2"                    -- fetched from registry
```

Path deps resolve directly to a sibling directory and don't copy into
`deps/`. Registry deps fetch through the global cache and materialize
into `deps/`. Same declaration site, two strategies — keeps the
workspace story (multi-package monorepos) clean without bolting on a
separate "workspace" concept.

### Materialization strategy

Three reasonable choices for how `deps/` gets populated from the
global cache:

| Strategy | Pros                                  | Cons                                       |
| -------- | ------------------------------------- | ------------------------------------------ |
| Copy     | Self-contained, edit-safe             | Disk usage scales with project count       |
| Symlink  | Zero disk cost                        | Accidental edits propagate to global cache |
| Hardlink | Zero disk cost, edits don't propagate | Windows historically struggles             |

Start with **copies, marked read-only** (`chmod -R a-w deps/`). Tiny
disk cost, clear failure mode if a user tries to edit a dep, no Windows
weirdness. Switch to hardlinks later if disk pressure becomes real.

---

## Stdlib as packages

### The uniformity principle

Stdlib is just packages. `Global`, `Http`, `Json`, `Net` — these are
real Koja packages with `koja.toml` and `src/`, materialized into
`deps/` like any third-party dep. The compiler resolves them through
the same lookup path as user deps.

This solves a category of papercut that plagues Rust: clicking `Option`
in the LSP doesn't take you anywhere unless you've separately installed
rust-src and configured rust-analyzer. Every Rust dev knows this. Most
other modern languages (Go, Elixir, Python, Zig, Swift, OCaml) ship
stdlib as readable source and uniform IDE jump-to-source just works.
Koja joins that group.

### Versioning rides the compiler

Stdlib version is auto-derived from compiler version and written into
`koja.lock` on first build. Users can't independently pin stdlib —
that way lies subtle UB from version-skew between compiler intrinsics
and stdlib shapes. Compiler upgrade → lock changes → `deps/global/`
re-materializes.

### Compiler-dev mode vs user mode

Two modes the compiler needs to support, distinguished by an env var:

| Mode         | Stdlib source                                  | When             |
| ------------ | ---------------------------------------------- | ---------------- |
| Compiler-dev | `KOJA_STDLIB_PATH` → `koja/lib/`               | Inside this repo |
| User         | Install location (`~/.koja/<version>/stdlib/`) | Everywhere else  |

Compiler-dev mode skips materialization entirely — you're editing stdlib
in place and need the resolver to read the in-tree source directly.
User mode materializes from the install location into the project's
`deps/`. The same two-mode story Cargo uses to bootstrap itself: cargo-the-
tool's own deps live in `~/.cargo/`, but rustc-the-compiler has its
libstd in-tree at `library/`.

### Intrinsics stay in the compiler

Stdlib being a package doesn't mean `Int + Int` becomes a method
dispatch. Things like `Int`, `String`, `List` literal syntax, and
arithmetic primitives are intrinsics — the compiler still has special
binding for `Global.Int` as "the magic int." `with_stdlib_stubs` (or
its successor) preloads the dep-graph slot for `Global` before
resolving anything else. The intrinsic binding stays; only the
_loading path_ uniforms.

### Override is technically possible, strongly discouraged

Power users may want to swap an stdlib package (custom `List` impl,
alternative scheduler). Allow via explicit:

```toml
[stdlib]
global = { path = "../my_global_fork" }
```

Compiler errors emitted under a forked stdlib should flag this loudly
("compiled against forked Global stdlib") so debugging weird behavior
under an override doesn't waste hours.

---

## Test execution

### `koja test` semantics

When `koja test` fires:

1. Resolve the dep graph including `[test-dependencies]` from `koja.toml`.
2. Include items annotated `@test "..."` and everything under `test/`.
3. Lower to IR, key the artifacts in `build/<profile>/test/cache/`.
4. Generate or select a test runner entry point.
5. Execute: under debug, hand IR to `koja-ir-eval`; under release,
   codegen and run the binary.
6. Report results.

Under debug, step 4 doesn't need to produce a binary at all — the
interpreter runs the cached IR directly. That's where the sub-second
test feedback loop comes from.

### `@test "description"` discovery

Tests are declared with an annotation that carries a human-readable
description, not a function name:

```koja
@test "encodes empty string as two quotes"
fn _ ->
  result = Encoder.encode("")
  Test.eq(result, "\"\"")
end
```

The description is the primary identifier for filtering and reporting —
no more `fn test_user_can_log_in_with_valid_credentials_and_remember_me_checked`
identifier abuse. Tests live in either:

- `src/` alongside the code they test (white-box style, can see private items)
- `test/` as a sibling dir (black-box style, public API only)

Both kinds participate in the test scope; the directory choice is an
ergonomic axis. Most languages support both shapes (Rust's `#[cfg(test)]`

- `tests/`, Go's `_test.go` + integration packages); Koja does the same.

### Concurrency by default

Tests run as separate Koja processes, in parallel, supervised. The
BEAM-flavored process model makes this essentially free: even release
builds spawn processes cheaply, since Koja processes are runtime-level,
not OS-level.

Implications baked into the runner from day one:

- **Per-test process isolation.** Each test starts with fresh process
  state. Globals can't bleed between tests. A huge bug-class
  eliminator.
- **Crash attribution via supervisor.** A failing test = a failing
  child process, attributed cleanly back to its `@test`. No "which
  test took down the runner?" mystery.
- **Output buffered per-process, emitted on completion.** Five parallel
  tests printing to stdout would scramble; buffering keeps logs
  readable. Stream-with-prefix is an optional mode for CI.
- **Opt-out, not opt-in, for serial tests.** Tests that genuinely can't
  parallelize (port bindings, fixed-path fixtures) declare so
  explicitly. The shape (`@test "..." serial: true` or
  `@test_serial "..."`) is a design choice to settle before the runner
  has enough users to make changing it painful.

---

## Script execution

### Two resolution modes

Scripts (`.kojs` files) don't have an `koja.toml`, `deps/`, or
`build/`. They need a different resolution path. The "stdlib is just
a package" promise is contextual to projects; scripts live in a
fallback mode that every script-capable language has.

|                 | Project mode                           | Script mode                                    |
| --------------- | -------------------------------------- | ---------------------------------------------- |
| Stdlib location | `deps/global/` (materialized)          | install location (`~/.koja/<version>/stdlib/`) |
| Cache location  | `build/<profile>/cache/` (per-project) | `~/.koja/<version>/cache/` (global)            |
| Deps            | `koja.toml` + `koja.lock` + `deps/`    | none initially (see future work)               |
| Backend default | debug = interpreter, release = LLVM    | interpreter, always                            |

This isn't really "compiler magic" in a deep sense — it's just runtime
stdlib resolution, the same pattern Python, Ruby, Node, Elixir scripts,
Perl, and Deno all use. Different lookup path, same physical bytes.

### First-run warming

The first `koja run foo.kojs` after a compiler install parses +
typechecks stdlib once and caches `.est` / `.eir` into
`~/.koja/<version>/cache/`. Every subsequent script run reads those
caches — no re-parse, no re-typecheck. Startup is dominated by
parsing the user's script, not the stdlib.

Target startup: under 100ms for a small script on a warm cache. This
is the bar for shebang ergonomics.

### LSP duality

The LSP needs the same dual-mode resolution. Opening a `.kojs` file
with no enclosing project, it resolves stdlib via the install location
instead of `deps/`. Jump-to-source still works — it just points into
`~/.koja/<version>/stdlib/global/src/option.koja` instead of the
project-local `deps/global/...`. Same source, different filesystem
path; user experience identical.

The amount of LSP-side code to support this is small: a
`resolve_stdlib_root()` helper that returns the right path based on
whether there's an enclosing project. Most of the LSP doesn't need to
care.

### Future: inline-dep scripts

Deno, Python (PEP 723), and Elixir (`Mix.install`) have all converged
on an ergonomic pattern for scripts that need one or two libraries:

```koja
#!/usr/bin/env koja
@deps {
  http_client: "1.2",
  json: "1.0",
}

alias Http.Client
alias Json.Encoder

response = Client.get("https://api.example.com/data")
print(Encoder.encode(response.body))
```

The compiler sees `@deps`, resolves through the global registry,
caches at `~/.koja/<version>/script-cache/<dep>@<version>/`, runs the
script. First run downloads; subsequent runs are instant.

Not a v1 feature. Worth keeping the design space open by not
hard-requiring `koja.toml` for any dep resolution.

---

## Sharp edges

### Stdlib version mismatch via shared caches

If two projects on the same machine pin different compiler versions
(via asdf or similar), they need different stdlib materializations.
The global install layout (`~/.koja/<version>/stdlib/`) keys on
compiler version, so this works out — but the `deps/global/` _inside_
a project is also version-specific. Switching the compiler version
the project uses should re-materialize `deps/global/` from the new
install. Forgetting this would mean stale stdlib + new compiler =
mystery errors.

### Disk usage from per-project stdlib copies

Copying stdlib into every project's `deps/` does duplicate it on disk.
Stdlib is small (probably under 10MB for a long time), so this isn't
real pain at first. If it becomes pain, the migration to hardlinks or
content-addressable cache + symlinks is mechanical and doesn't change
the user-facing model.

### Read-only enforcement on materialized deps

`chmod -R a-w deps/` prevents accidental edits from clobbering "source
of truth" deps. A user editing stdlib in `deps/global/` and getting a
permission-denied is much better feedback than silently working until
the next `koja install` wipes their changes. Worth getting right from
day one.

### Test output interleaving

Concurrent tests scribbling on stdout will interleave nonsensically
without buffering. Buffer-per-process and emit-on-completion is the
right default; a `--stream` flag for CI use is optional. Decide the
default early — switching from streaming to buffered later breaks log
scrapers people will have written.

### Serial-test annotation shape

Decide the syntax for opt-out-of-parallel before there are too many
test suites in the wild:

- `@test "..." serial: true` (annotation argument)
- `@test_serial "..."` (separate annotation)
- `@test "..." @serial` (stacked annotations)

Stacked annotations would generalize to other axes (`@slow`, `@flaky`,
etc.) and feels Koja-idiomatic. Probably the right call, but worth
making the decision explicit.

---

## What this replaces

Earlier sketches (see `archive/20260403-PROJECT.md`) handled the
"project system + module unification" question for v1. This document
supersedes the build / deps / test / script layout questions
specifically, in the context of:

- The four-phase pipeline (parser → typecheck → ir → codegen/eval) from
  `COMPILER-NORTHSTAR.md`.
- The dual-backend story (interpreter for inner loop, LLVM for ship).
- The `.est` / `.eir` sealed-artifact format that makes incremental
  compilation work across packages.
- The cookbook-not-packages distribution model from `PACKAGE.md` —
  `koja.toml` deps remain for sharing code between your own projects;
  the cookbook is for community reference implementations.

---

## Summary

1. **`build/` for artifacts, `deps/` for sources, both gitignored.**
   The four-piece model (`koja.toml`, `koja.lock`, `deps/`, `build/`)
   layers cleanly: each tier regenerates from the next one up.
2. **Profile-based subdirs only.** `build/debug/` and `build/release/`.
   No `build/dev/`, no `build/prod/`. Runtime environment is the
   program's concern, not the build tool's.
3. **Two axes: profile × scope.** Profile is debug vs release. Scope
   is default vs test. `koja test` is a scope, not a profile, which
   means tests can run through either backend.
4. **Debug → interpreter, release → LLVM.** The dual-backend split
   gives sub-100ms test feedback under debug and validates the codegen
   path under `koja test --release`.
5. **Stdlib is just packages.** Materialized into `deps/` like any
   third-party dep, resolved through the same path. LSP jump-to-source
   works uniformly. Versioning rides the compiler.
6. **Scripts use install-location stdlib + a global cache.** Different
   resolution mode, same physical bytes. First-run warms the cache;
   subsequent runs start in under 100ms.
7. **Tests are concurrent by default.** Process-per-test via Koja's
   runtime-level process model, supervised, with per-process output
   buffering. Serial is the opt-out, not the default.
8. **`@test "description"` carries human-readable test names.**
   Description is the primary identifier for filtering and reporting.
   Tests live in `src/` (white-box) or `test/` (black-box), both kinds
   participate in the test scope.

The shape is boring on purpose. Every directory has one lifecycle,
every artifact has one source of truth, every path is independently
regenerable. Boring means easy to clean up, easy to explain, easy to
keep working as the language grows.
