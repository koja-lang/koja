# koja-project

The project model shared by the compiler and the language server. It
reads `koja.toml`, finds the sources a project compiles with, and
resolves dependencies. `koja-driver` and `koja-lsp` both load through
this crate, so a manifest feature reaches both at once.

## Key files

- `manifest.rs`: `ProjectConfig` from `koja.toml`, `find_project_root`,
  `load_project`, `resolve_project_root`.
- `loader.rs`: `ProjectLoader` walks the manifest's `src` and `test`
  directories, the dependencies, and the stdlib into one `Loaded`
  bundle. `LoadOptions` picks the dependency mode, the error policy, and
  the `StdlibOptions`. `stdlib_sources` is the one rule for which stdlib
  packages join a bundle.
- `deps/mod.rs`: the resolver. `sync_project` fetches and materializes,
  `fetch_project` updates the lockfile, `resolve_on_disk` reads what is
  already under `deps/` and warns about the rest. `dep_state` and
  `remove_tree` back the `koja deps` subcommands.
- `deps/lock.rs`: `koja.lock` read and write.
- `deps/git.rs`: the bare mirror cache under the user's cache dir, ref
  resolution, and tree export.

## Loading rules

- The stdlib leads the bundle. `Global` comes first, then the qualified
  packages. A package the project or one of its dependencies claims is
  skipped, so a project named like a stdlib package does not load twice.
  When the project is `Global` itself, only the test build packages join
  and only when `link_tests` is set.
- `Dependencies::Sync` fetches what the lockfile pins and is what the
  compiler uses. `Dependencies::OnDisk` never touches the network and is
  what the language server uses. `Dependencies::Skip` loads the project
  alone.
- `ErrorPolicy::Strict` returns the first error. `ErrorPolicy::Lenient`
  turns every error into a warning in `Loaded.warnings` and keeps going,
  so an editor still gets a bundle when one file or one dependency is
  broken.
- Overlays replace the text read from disk for a path, by the path given
  or its canonical form. The language server passes its open buffers so
  the project compiles from unsaved editor state.

## Tests

Unit tests in `loader.rs` and `deps/mod.rs` build small projects in a
temp dir and check the bundle order, the overlay rule, the stdlib rule,
and the on-disk warnings.
