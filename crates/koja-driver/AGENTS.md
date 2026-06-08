# koja-driver

CLI binary (`koja`) and compilation pipeline orchestration.

## Key files

- `main.rs` -- Clap CLI: subcommands delegate to `commands`
- `commands.rs` -- Implementations for build/run/check/fmt/doc/test/new/lex/parse
- `pipeline.rs` -- Shared compile pipeline: merge type contexts, run codegen, link binary
- `project.rs` -- Parses `koja.toml` into `ProjectConfig`
- `resolve.rs` -- File resolution: single-file vs project mode, `SourceSet` construction
- `diagnostics.rs` -- Rustc-style diagnostic printing
- `build.rs` -- Finds `libkoja_runtime.a` and `libcrypto.a`, sets linker env vars

## Vocabulary

A _package_ is a unit of distribution (your app, the stdlib, a dependency). A
_file_ is a single `.koja` source file. A `SourceSet` is the flat collection
of every file visible to one build invocation -- stdlib files plus the
project's files plus every dep package's files -- keyed by FQN. There is no
dependency graph between files: `SourceSet.order` is just "stdlib first, then
project files" for processing convenience. The Koja language has no "module"
concept; when you see `module` in code below this point it is the Rust
language item (`mod foo;`).

## Tests

- `tests/lang_suite.rs` -- Integration tests that compile and run `.koja`/`.kojs`
  fixtures from `tests/lang/`. Flat single-file goldens are `.kojs` scripts
  (top-level statements, no `fn main`); a handful of message-passing/actor tests
  (`io/*`, `functions/call_roundtrip`, `types/cast_loop`) stay as `.koja` +
  `fn main` since spawning/orchestrating actors is program-shaped, and they also
  pin the lone-file `SourceShape::Program` path. `koja.toml` project fixtures
  remain `.koja`. The collector (`collect_test_files`) accepts both extensions;
  every fixture runs via `koja run --backend=llvm`.

## Build notes

- `build.rs` expects `libkoja_runtime.a` in the cargo target dir (built by `just build-runtime`)
- BoringSSL's `libcrypto.a` is embedded and written to a temp dir at link time
