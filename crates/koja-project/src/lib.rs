//! The project side of the Koja toolchain: the `koja.toml` manifest,
//! source discovery, and dependency resolution.
//!
//! The compiler driver and the language server both load a project
//! through this crate, so a manifest field or a bundling rule added
//! here reaches both at once. Nothing here compiles or renders. The
//! output is a list of package-tagged sources ready for
//! `koja_parser::parse_program`.

pub mod deps;
pub mod loader;
pub mod manifest;

pub use deps::{ResolvedDep, sync_project};
pub use loader::{
    Dependencies, ErrorPolicy, LoadOptions, Loaded, LoadedSource, ProjectLoader, SourceOrigin,
    StdlibOptions, stdlib_sources, walk_source_files,
};
pub use manifest::{
    DepConfig, DepSource, GitRef, ProjectConfig, find_project_root, load_project,
    resolve_project_root,
};
