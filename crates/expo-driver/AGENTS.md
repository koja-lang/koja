# expo-driver

CLI binary (`expo`) and compilation pipeline orchestration.

## Key files

- `main.rs` -- Clap CLI: subcommands delegate to `commands`
- `commands.rs` -- Implementations for build/run/check/fmt/doc/test/new/lex/parse
- `pipeline.rs` -- Shared compile pipeline: merge type contexts, run codegen, link binary
- `project.rs` -- Parses `expo.toml` into `ProjectConfig`
- `resolve.rs` -- Module resolution: single-file vs project mode, `ModuleGraph` construction
- `diagnostics.rs` -- Rustc-style diagnostic printing
- `build.rs` -- Finds `libexpo_runtime.a` and `libcrypto.a`, sets linker env vars

## Tests

- `tests/lang_suite.rs` -- Integration tests that compile and run `.expo` files from `tests/lang/`

## Build notes

- `build.rs` expects `libexpo_runtime.a` in the cargo target dir (built by `just build-runtime`)
- BoringSSL's `libcrypto.a` is embedded and written to a temp dir at link time
