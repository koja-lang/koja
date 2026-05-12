//! Source set resolution.
//!
//! Two resolution modes:
//! - **Single-file** ([`resolve_sources`]): one entry file
//! - **Project** ([`resolve_project_sources`]): uses [`ProjectConfig`] to scan
//!   directories for `.expo` files, building a flat namespace with stdlib
//!   auto-imported
//!
//! Resolution is parse-free: each resolver returns a [`SourceSet`] of
//! build-config metadata (entry, deps, target hints) plus a `Vec<SourceFile>`
//! of unparsed file bundles. The driver pipeline feeds the file vec to
//! [`expo_parser::parse_program`] as its own discrete step.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use expo_parser::SourceFile;

use crate::project::{self, ProjectConfig};

/// Build-config metadata for a single build.
///
/// Files live alongside in a `Vec<SourceFile>` returned by each
/// resolver; downstream stages thread them through
/// [`expo_parser::parse_program`] into a `ParsedProgram`. `SourceSet`
/// itself only carries what the driver needs to keep around for
/// codegen / linking after typecheck consumes the parsed program.
pub struct SourceSet {
    /// Package names loaded as dependencies (e.g. "JSON", "HTTP").
    pub dep_packages: Vec<String>,
    /// Path of the entry file (the one whose `fn main` / type entry
    /// drives the program). Empty until the test harness pipeline
    /// fills it in for `expo test`.
    pub entry: PathBuf,
    /// Package name the entry belongs to. Used as the executable's
    /// app name. Empty until set alongside `entry`.
    pub entry_package: String,
    /// Source text of the entry file, captured at resolve time so
    /// codegen-failure rendering (which fires after `ParsedProgram` is
    /// consumed by typecheck) can still render inline span context
    /// without re-borrowing the parsed program. The test-harness path
    /// fills this in alongside `entry` when it injects the synthetic
    /// `__expo_test_main__` source.
    pub entry_source: String,
    /// When the entry is a PascalCase type name (Process entry mode), this
    /// holds the type name (e.g. `"App"`). `None` for legacy `fn main` mode.
    pub entry_type: Option<String>,
}

impl SourceSet {
    fn new() -> Self {
        SourceSet {
            dep_packages: Vec::new(),
            entry: PathBuf::new(),
            entry_package: String::new(),
            entry_source: String::new(),
            entry_type: None,
        }
    }
}

// =============================================================================
// Single-file resolution
// =============================================================================

/// Builds a [`SourceSet`] + single-element file vector for one entry file.
pub fn resolve_sources(entry_path: &Path) -> Result<(SourceSet, Vec<SourceFile>), String> {
    let entry_stem = entry_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("invalid entry file name")?
        .to_string();

    let source = fs::read_to_string(entry_path)
        .map_err(|e| format!("error reading {}: {e}", entry_path.display()))?;

    let mut sources = SourceSet::new();
    sources.entry = entry_path.to_path_buf();
    sources.entry_package = entry_stem.clone();
    sources.entry_source = source.clone();

    let source_files = vec![SourceFile {
        package: entry_stem,
        path: entry_path.to_path_buf(),
        source,
    }];

    Ok((sources, source_files))
}

// =============================================================================
// Project-mode resolution
// =============================================================================

/// Builds a [`SourceSet`] + file vector for a project with `expo.toml`.
///
/// Scans all `src` directories for `.expo` files. Stdlib files are inserted
/// first. No import-following or topological sorting -- all project files
/// form a flat namespace.
pub fn resolve_project_sources(
    config: &ProjectConfig,
    project_root: &Path,
) -> Result<(SourceSet, Vec<SourceFile>), String> {
    let src_roots: Vec<PathBuf> = config.src.iter().map(|s| project_root.join(s)).collect();

    let entry = config
        .entry
        .as_deref()
        .ok_or("expo.toml has no `entry` field; required for build/run/check")?;

    let is_type_entry = config.entry_type_name().is_some();

    let mut sources = SourceSet::new();
    sources.entry_package = config.name.clone();
    sources.entry_type = config.entry_type_name().map(|s| s.to_string());

    let mut source_files: Vec<SourceFile> = Vec::new();

    if config.name != "Global" {
        insert_stdlib(&mut source_files, Some(&config.name));
    }
    scan_directories(&config.name, &src_roots, &mut source_files)?;
    resolve_dependencies(config, project_root, &mut sources, &mut source_files)?;

    let entry_path = if is_type_entry {
        let preferred = src_roots
            .iter()
            .map(|r| r.join("src.expo"))
            .find(|p| source_files.iter().any(|f| &f.path == p));
        match preferred {
            Some(p) => p,
            None => source_files
                .iter()
                .find(|f| f.package == config.name)
                .map(|f| f.path.clone())
                .ok_or_else(|| "no source files found in src directories".to_string())?,
        }
    } else {
        let candidates: Vec<PathBuf> = src_roots
            .iter()
            .map(|r| r.join(format!("{entry}.expo")))
            .collect();
        candidates
            .into_iter()
            .find(|p| source_files.iter().any(|f| &f.path == p))
            .ok_or_else(|| format!("entry file `{entry}` not found in src directories"))?
    };
    sources.entry_source = source_files
        .iter()
        .find(|f| f.path == entry_path)
        .map(|f| f.source.clone())
        .unwrap_or_default();
    sources.entry = entry_path;

    Ok((sources, source_files))
}

/// Inserts every embedded stdlib source file into `source_files`, with
/// synthetic paths like `<Global.io>` so they're stably keyed even though
/// they have no on-disk location. The package is derived from the
/// leading segment of the source name, so e.g. `<json.StringBuilder>`
/// joins the `json` package alongside the auto-imported `Global` package.
///
/// `skip_package` lets a project that *is* a stdlib package (e.g.
/// building or testing `lib/json`) bypass loading its own embedded
/// snapshot, so the local on-disk files become the authority instead
/// of double-defining every type.
pub fn insert_stdlib(source_files: &mut Vec<SourceFile>, skip_package: Option<&str>) {
    for &(name, source) in expo_stdlib::SOURCES {
        let package = name
            .split_once('.')
            .map_or(name, |(pkg, _)| pkg)
            .to_string();
        if Some(package.as_str()) == skip_package {
            continue;
        }
        source_files.push(SourceFile {
            package,
            path: PathBuf::from(format!("<{name}>")),
            source: source.to_string(),
        });
    }
}

/// Scans directories for `.expo` files and adds each as a [`SourceFile`].
/// Files already present in `source_files` (matched by `path`) are skipped,
/// so overlapping roots / repeat scans don't double-count.
///
/// Files whose stem starts with `alpha_` are skipped: they are
/// alpha-pipeline-only sources (e.g. `lib/global/src/alpha_debug_containers.expo`)
/// that depend on alpha-pipeline-only features like the universal-Debug
/// fallback. The v1 type checker, which drives every code path in this
/// module, would reject them. The alpha pipeline reaches them directly via
/// [`expo_stdlib::ALPHA_AUTOIMPORT`].
fn scan_directories(
    project_name: &str,
    roots: &[PathBuf],
    source_files: &mut Vec<SourceFile>,
) -> Result<(), String> {
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let files = collect_expo_files_recursive(root);
        for file_path in files {
            if source_files.iter().any(|f| f.path == file_path) {
                continue;
            }
            if is_alpha_only_path(&file_path) {
                continue;
            }
            let source = fs::read_to_string(&file_path)
                .map_err(|e| format!("error reading {}: {e}", file_path.display()))?;
            source_files.push(SourceFile {
                package: project_name.to_string(),
                path: file_path,
                source,
            });
        }
    }
    Ok(())
}

fn is_alpha_only_path(path: &Path) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|stem| stem.starts_with("alpha_"))
}

/// Builds a [`SourceSet`] + file vector for running tests.
///
/// Like [`resolve_project_sources`], but also scans `test` directories and
/// includes all source + test files in the source set. The project's entry
/// file (which contains `fn main`) is excluded since the test harness
/// replaces it. The `entry` field is left empty -- the caller inserts a
/// generated harness.
pub fn resolve_test_project_sources(
    config: &ProjectConfig,
    project_root: &Path,
) -> Result<(SourceSet, Vec<SourceFile>), String> {
    let src_roots: Vec<PathBuf> = config.src.iter().map(|s| project_root.join(s)).collect();
    let test_roots: Vec<PathBuf> = config.test.iter().map(|s| project_root.join(s)).collect();

    let all_roots: Vec<PathBuf> = src_roots.iter().chain(test_roots.iter()).cloned().collect();

    let mut sources = SourceSet::new();
    let mut source_files: Vec<SourceFile> = Vec::new();

    if config.name != "Global" {
        insert_stdlib(&mut source_files, Some(&config.name));
    }
    scan_directories(&config.name, &all_roots, &mut source_files)?;
    resolve_dependencies(config, project_root, &mut sources, &mut source_files)?;

    if let Some(ref entry) = config.entry
        && config.entry_type_name().is_none()
    {
        let entry_paths: Vec<PathBuf> = src_roots
            .iter()
            .map(|r| r.join(format!("{entry}.expo")))
            .collect();
        source_files.retain(|f| !entry_paths.iter().any(|p| p == &f.path));
    }

    Ok((sources, source_files))
}

/// Scans each dependency declared in `[dependencies]` and adds its source
/// files to the file vector. The dep's entry file is skipped to avoid
/// `fn main` conflicts with the consuming project.
///
/// Enforces the duplicate-package-name rule: every project implicitly imports
/// `Global`, and no two packages in the dependency graph (project + implicit
/// `Global` + each declared dep's `[project] name`) may share a name. The real
/// stdlib (the lone project with `name = "Global"`) is the one project that
/// doesn't get the implicit `Global` entry, so its self-build does not collide.
fn resolve_dependencies(
    config: &ProjectConfig,
    project_root: &Path,
    sources: &mut SourceSet,
    source_files: &mut Vec<SourceFile>,
) -> Result<(), String> {
    let mut seen_pkgs: BTreeSet<String> = BTreeSet::new();
    seen_pkgs.insert(config.name.clone());
    if config.name != "Global" {
        seen_pkgs.insert("Global".to_string());
    }
    for (alias, dep) in &config.dependencies {
        let dep_path = match &dep.path {
            Some(p) => project_root.join(p),
            None => {
                return Err(format!(
                    "dependency `{alias}` has no `path` (git dependencies are not yet supported)"
                ));
            }
        };

        let dep_config = project::load_project(&dep_path)?.ok_or_else(|| {
            format!(
                "dependency `{alias}`: no expo.toml found at {}",
                dep_path.display()
            )
        })?;

        if !seen_pkgs.insert(dep_config.name.clone()) {
            return Err(format!(
                "duplicate package name `{}` in dependency graph (declared by project, dependency `{alias}`, or implicit `Global` import)",
                dep_config.name
            ));
        }

        let dep_src_roots: Vec<PathBuf> = dep_config.src.iter().map(|s| dep_path.join(s)).collect();
        scan_directories(&dep_config.name, &dep_src_roots, source_files)?;
        sources.dep_packages.push(dep_config.name.clone());

        if let Some(ref entry) = dep_config.entry {
            let entry_paths: Vec<PathBuf> = dep_src_roots
                .iter()
                .map(|r| r.join(format!("{entry}.expo")))
                .collect();
            source_files.retain(|f| !entry_paths.iter().any(|p| p == &f.path));
        }
    }
    Ok(())
}

pub(crate) fn collect_expo_files_recursive(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            result.extend(collect_expo_files_recursive(&path));
        } else if path.extension().is_some_and(|ext| ext == "expo") {
            result.push(path);
        }
    }
    result
}
