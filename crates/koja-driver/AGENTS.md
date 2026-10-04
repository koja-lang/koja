# koja-driver

CLI binary (`koja`) and compilation pipeline orchestration.

## Key files

- `main.rs` -- Clap CLI: subcommands delegate to `commands`
- `commands.rs` -- Implementations for build/run/check/fmt/doc/test/new/lex/parse
- `pipeline.rs` -- Shared compile pipeline: merge type contexts, run codegen, link binary
- `deps/` -- The `koja deps` subcommands, a thin CLI over `koja_project::deps`
- `diagnostics.rs` -- Rustc-style diagnostic printing
- `build.rs` -- Finds `libkoja_runtime.a` and `libcrypto.a`, sets linker env vars

The manifest, the source walk, and the dependency resolver live in
`koja-project`, shared with the language server. The driver asks
`ProjectLoader` for a bundle and passes `StdlibOptions` through it.

## Vocabulary

A _package_ is a unit of distribution (your app, the stdlib, a dependency). A
_file_ is a single `.koja` source file. A bundle is the flat list of every
file visible to one build invocation, stdlib first, then the project's files
and every dep package's files. There is no dependency graph between files.
The Koja language has no "module" concept. When you see `module` in code
below this point it is the Rust language item (`mod foo;`).

## Tests

- `tests/lang_suite.rs` -- Integration tests that compile and run `.koja`/`.kojs`
  fixtures from `tests/lang/`. Flat single-file goldens are `.kojs` scripts
  (top-level statements); `koja.toml` project fixtures are `.koja` with a
  Process-implementing entry type named by `entry` in the manifest. Standalone
  `.koja` files are no longer runnable (`koja run`/`build` reject them with a
  hint). The collector (`collect_test_files`) accepts both extensions; every
  fixture runs via `koja run --backend=llvm`.

## Build notes

- `build.rs` expects `libkoja_runtime.a` in the cargo target dir (built by `just build-runtime`)
- BoringSSL's `libcrypto.a` is embedded and written to a temp dir at link time
