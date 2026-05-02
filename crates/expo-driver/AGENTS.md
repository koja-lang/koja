# expo-driver

CLI binary (`expo`) and compilation pipeline orchestration.

## Key files

- `main.rs` -- Clap CLI: subcommands delegate to `commands`
- `commands.rs` -- Implementations for build/run/check/fmt/doc/test/new/lex/parse
- `pipeline.rs` -- Shared compile pipeline: merge type contexts, run codegen, link binary
- `project.rs` -- Parses `expo.toml` into `ProjectConfig`
- `resolve.rs` -- File resolution: single-file vs project mode, `SourceSet` construction
- `diagnostics.rs` -- Rustc-style diagnostic printing
- `build.rs` -- Finds `libexpo_runtime.a` and `libcrypto.a`, sets linker env vars

## Vocabulary

A _package_ is a unit of distribution (your app, the stdlib, a dependency). A
_file_ is a single `.expo` source file. A `SourceSet` is the flat collection
of every file visible to one build invocation -- stdlib files plus the
project's files plus every dep package's files -- keyed by FQN. There is no
dependency graph between files: `SourceSet.order` is just "stdlib first, then
project files" for processing convenience. The Expo language has no "module"
concept; when you see `module` in code below this point it is the Rust
language item (`mod foo;`) or the AST type (`expo_ast::Module`, an unrelated
holdover that a later refactor will rename to `File`).

## Tests

- `tests/lang_suite.rs` -- Integration tests that compile and run `.expo` files from `tests/lang/`

## Build notes

- `build.rs` expects `libexpo_runtime.a` in the cargo target dir (built by `just build-runtime`)
- BoringSSL's `libcrypto.a` is embedded and written to a temp dir at link time
